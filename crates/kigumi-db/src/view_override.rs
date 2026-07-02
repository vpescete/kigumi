//! Runtime view overrides — the declarative-extension layer for the UI contract (Odoo's `ir.ui.view`
//! with `manual`/inheritance, kept to the metadata an override can change without touching layout XML).
//! An override relabels, re-widgets, hides, or locks a field on a model, or makes it conditionally
//! invisible/readonly via a domain; the host applies it as a post-pass over the auto-derived contract
//! at runtime, so it takes effect WITHOUT recompiling.
//!
//! Pure metadata: no DDL, no column changes (unlike a custom field). `invisible_when` / `readonly_when`
//! hold a JSON domain AST (validated against the model at write time); the contract emits them and the
//! frontend evaluates them per record — the same shape a compile-time UI rule produces.

use crate::{Db, DbError};
use sqlx::Row;

const ENSURE: &str = "CREATE TABLE IF NOT EXISTS ir_ui_view \
     (id bigserial PRIMARY KEY, model text NOT NULL, field text NOT NULL, \
      label text, widget text, invisible boolean NOT NULL DEFAULT false, \
      readonly boolean NOT NULL DEFAULT false, invisible_when text, readonly_when text, \
      UNIQUE (model, field))";

/// A runtime UI override for one field. `label`/`widget` replace the contract value when set; `invisible`
/// drops the field from the served contract; `readonly` locks it; `invisible_when`/`readonly_when` carry
/// a JSON domain AST for conditional visibility/lock. All fields are data — none reaches DDL.
#[derive(Debug, Clone)]
pub struct ViewOverride {
    pub model: String,
    pub field: String,
    pub label: Option<String>,
    pub widget: Option<String>,
    pub invisible: bool,
    pub readonly: bool,
    pub invisible_when: Option<String>,
    pub readonly_when: Option<String>,
}

impl Db {
    /// Creates the view-override table if absent (idempotent), and adds the conditional-domain columns
    /// to a table that predates them.
    pub async fn ensure_view_schema(&self) -> Result<(), DbError> {
        sqlx::query(ENSURE).execute(&self.pool).await?;
        sqlx::query("ALTER TABLE ir_ui_view ADD COLUMN IF NOT EXISTS invisible_when text")
            .execute(&self.pool)
            .await?;
        sqlx::query("ALTER TABLE ir_ui_view ADD COLUMN IF NOT EXISTS readonly_when text")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Every configured view override (sorted by model, then field).
    pub async fn load_view_overrides(&self) -> Result<Vec<ViewOverride>, DbError> {
        let rows = sqlx::query(
            "SELECT model, field, label, widget, invisible, readonly, invisible_when, readonly_when \
             FROM ir_ui_view ORDER BY model, field",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| ViewOverride {
                model: r.get("model"),
                field: r.get("field"),
                label: r.get("label"),
                widget: r.get("widget"),
                invisible: r.get("invisible"),
                readonly: r.get("readonly"),
                invisible_when: r.get("invisible_when"),
                readonly_when: r.get("readonly_when"),
            })
            .collect())
    }

    /// Upserts a view override for `(model, field)`. Pure metadata — fully parameterized, no DDL.
    #[allow(clippy::too_many_arguments)]
    pub async fn set_view_override(
        &self,
        model: &str,
        field: &str,
        label: Option<&str>,
        widget: Option<&str>,
        invisible: bool,
        readonly: bool,
        invisible_when: Option<&str>,
        readonly_when: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO ir_ui_view (model, field, label, widget, invisible, readonly, invisible_when, readonly_when) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (model, field) DO UPDATE SET label = EXCLUDED.label, \
             widget = EXCLUDED.widget, invisible = EXCLUDED.invisible, readonly = EXCLUDED.readonly, \
             invisible_when = EXCLUDED.invisible_when, readonly_when = EXCLUDED.readonly_when",
        )
        .bind(model)
        .bind(field)
        .bind(label)
        .bind(widget)
        .bind(invisible)
        .bind(readonly)
        .bind(invisible_when)
        .bind(readonly_when)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
