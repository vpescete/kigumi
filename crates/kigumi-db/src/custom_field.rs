//! Runtime custom fields — the declarative-extension half of the metamodel (Odoo's `ir.model.fields`
//! with `manual=True`, Frappe's custom fields). A field added here gets a real column on the model's
//! table plus a registry row; the host merges it into the resolved model at runtime, so it appears in
//! the UI contract and flows through secured CRUD WITHOUT recompiling the binary.
//!
//! Scalar kinds (text/integer/float/decimal/bool/date/datetime) plus `many2one` (a bigint FK column,
//! with the target model in `relation`). One2many/Many2many (no own column) remain a follow-up.

use crate::{Db, DbError};
use kigumi_core::{FieldDef, FieldKind, ResolvedModel};
use sqlx::Row;
use std::collections::HashMap;

const ENSURE: &str = "CREATE TABLE IF NOT EXISTS ir_model_field \
     (id bigserial PRIMARY KEY, model text NOT NULL, name text NOT NULL, label text NOT NULL, \
      kind text NOT NULL, required boolean NOT NULL DEFAULT false, default_value text, relation text, \
      created_at timestamptz NOT NULL DEFAULT now(), UNIQUE (model, name))";

/// A runtime-defined field. Strings are owned (they come from the DB, not the compile-time `'static`
/// catalog); the host leaks them when it builds a `FieldDef`.
#[derive(Debug, Clone)]
pub struct CustomField {
    pub model: String,
    pub name: String,
    pub label: String,
    /// One of: text, integer, float, decimal, bool, date, datetime, many2one.
    pub kind: String,
    pub required: bool,
    pub default_value: Option<String>,
    /// Target model for a `many2one`; `None` for scalar kinds.
    pub relation: Option<String>,
}

/// The scalar `FieldKind` for a custom-field kind string, or `None` for an unknown one. `many2one`
/// is NOT here (it needs a leaked relation target); it is handled by the caller. Shared by the
/// runtime `FieldDef` build and the server's create-field DDL-type validation.
pub fn custom_scalar_kind(kind: &str) -> Option<FieldKind> {
    Some(match kind {
        "text" => FieldKind::Text,
        "integer" => FieldKind::Integer,
        "float" => FieldKind::Float,
        "decimal" => FieldKind::Decimal { currency_field: None },
        "bool" => FieldKind::Bool,
        "date" => FieldKind::Date,
        "datetime" => FieldKind::Datetime,
        _ => return None,
    })
}

impl CustomField {
    /// Builds a `'static` `FieldDef` from this runtime row, or `None` for an unknown kind (or a
    /// `many2one` with no relation). Strings are leaked — loaded once and held for the process
    /// lifetime, like the runtime ACL/rule strings. `many2one` carries its target in `relation`;
    /// all other kinds are scalars. The canonical conversion, shared by every host (server, MCP).
    pub fn to_field_def(&self) -> Option<FieldDef> {
        let leak = |s: &str| -> &'static str { Box::leak(s.to_string().into_boxed_str()) };
        let kind = match self.kind.as_str() {
            "many2one" => FieldKind::Many2one { target: leak(self.relation.as_deref()?) },
            other => custom_scalar_kind(other)?,
        };
        Some(FieldDef {
            name: leak(&self.name),
            label: leak(&self.label),
            kind,
            required: self.required,
            stored: true,
            compute: None,
            depends: &[],
            default: self.default_value.as_deref().map(leak),
            unique: false,
            check: None,
        })
    }
}

/// A Postgres identifier safe to interpolate into DDL: lowercase start, then letters/digits/underscore.
/// Custom-field names and the model table are validated against this before any `ALTER TABLE`.
pub fn is_safe_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl Db {
    /// Creates the custom-field registry table if absent (idempotent), and adds the `relation` column
    /// to a registry that predates it.
    pub async fn ensure_custom_field_schema(&self) -> Result<(), DbError> {
        sqlx::query(ENSURE).execute(&self.pool).await?;
        sqlx::query("ALTER TABLE ir_model_field ADD COLUMN IF NOT EXISTS relation text")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Every registered custom field (sorted by model, then name).
    pub async fn load_custom_fields(&self) -> Result<Vec<CustomField>, DbError> {
        let rows = sqlx::query(
            "SELECT model, name, label, kind, required, default_value, relation FROM ir_model_field ORDER BY model, name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| CustomField {
                model: r.get("model"),
                name: r.get("name"),
                label: r.get("label"),
                kind: r.get("kind"),
                required: r.get("required"),
                default_value: r.get("default_value"),
                relation: r.get("relation"),
            })
            .collect())
    }

    /// The runtime custom fields grouped by model, as `'static` `FieldDef`s ready to merge into a
    /// resolved model. The shared loader for every host that serves runtime-extended models (the
    /// server's live map, the MCP server's per-connection snapshot).
    pub async fn custom_fields_by_model(&self) -> Result<HashMap<String, Vec<FieldDef>>, DbError> {
        let mut map: HashMap<String, Vec<FieldDef>> = HashMap::new();
        for cf in self.load_custom_fields().await? {
            if let Some(def) = cf.to_field_def() {
                map.entry(cf.model.clone()).or_default().push(def);
            }
        }
        Ok(map)
    }

    /// Resolves `model` from the compile-time catalog and merges its runtime custom fields, so the
    /// returned `ResolvedModel` matches what secured CRUD and the UI contract see — the shape an MCP
    /// tool or a one-off caller needs without standing up the server's live refresh loop.
    pub async fn resolve_with_custom_fields(
        &self,
        model: &ResolvedModel,
    ) -> Result<ResolvedModel, DbError> {
        let extra = self.custom_fields_by_model().await?;
        Ok(match extra.get(model.name) {
            Some(fields) if !fields.is_empty() => {
                let mut m = model.clone();
                m.fields.extend(fields.iter().cloned());
                m
            }
            _ => model.clone(),
        })
    }

    /// Registers a custom field on `model` and adds its column to `table`. `col_type` is the Postgres
    /// type for the kind (the caller derives it from the field kind). Both `name` and `table` MUST be
    /// safe identifiers (validated by the caller and re-checked here) — they are interpolated into DDL,
    /// so this is the one place a custom name reaches raw SQL; everything else is parameterized.
    #[allow(clippy::too_many_arguments)]
    pub async fn add_custom_field(
        &self,
        model: &str,
        name: &str,
        label: &str,
        kind: &str,
        required: bool,
        default_value: Option<&str>,
        relation: Option<&str>,
        table: &str,
        col_type: &str,
    ) -> Result<(), DbError> {
        if !is_safe_ident(name) {
            return Err(DbError::BadInput(format!("'{name}' is not a valid field name (lowercase letters, digits, underscore)")));
        }
        if !is_safe_ident(table) {
            return Err(DbError::BadInput(format!("'{table}' is not a valid table name")));
        }
        if col_type.is_empty() {
            return Err(DbError::BadInput(format!("kind '{kind}' has no own column and cannot be a custom field yet")));
        }
        let mut tx = self.pool.begin().await?;
        // The registry row (parameterized) and the column (DDL, identifiers validated above) commit
        // together — no half state where the column exists without its registration or vice versa.
        sqlx::query(
            "INSERT INTO ir_model_field (model, name, label, kind, required, default_value, relation) VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(model)
        .bind(name)
        .bind(label)
        .bind(kind)
        .bind(required)
        .bind(default_value)
        .bind(relation)
        .execute(&mut *tx)
        .await?;
        sqlx::query(&format!("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS {name} {col_type}"))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}
