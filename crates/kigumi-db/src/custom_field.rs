//! Runtime custom fields — the declarative-extension half of the metamodel (Odoo's `ir.model.fields`
//! with `manual=True`, Frappe's custom fields). A field added here gets a real column on the model's
//! table plus a registry row; the host merges it into the resolved model at runtime, so it appears in
//! the UI contract and flows through secured CRUD WITHOUT recompiling the binary.
//!
//! Scalar kinds (text/integer/float/decimal/bool/date/datetime) plus `many2one` (a bigint FK column,
//! with the target model in `relation`). One2many/Many2many (no own column) remain a follow-up.

use crate::{Db, DbError};
use sqlx::Row;

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
