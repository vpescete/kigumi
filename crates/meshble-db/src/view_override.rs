//! Runtime view overrides — the declarative-extension layer for the UI contract (Odoo's `ir.ui.view`
//! with `manual`/inheritance, kept to the metadata an override can change without touching layout XML).
//! An override relabels, re-widgets, hides, or locks a field on a model; the host applies it as a
//! post-pass over the auto-derived contract at runtime, so it takes effect WITHOUT recompiling.
//!
//! Pure metadata: no DDL, no column changes (unlike a custom field). Conditional `invisible_when` /
//! `readonly_when` (domain-driven) is a follow-up — it needs an owned-domain UI rule; this layer only
//! does the unconditional overrides, which cover the common "rename / hide / lock a field" need.

use crate::{Db, DbError};
use sqlx::Row;

const ENSURE: &str = "CREATE TABLE IF NOT EXISTS ir_ui_view \
     (id bigserial PRIMARY KEY, model text NOT NULL, field text NOT NULL, \
      label text, widget text, invisible boolean NOT NULL DEFAULT false, \
      readonly boolean NOT NULL DEFAULT false, UNIQUE (model, field))";

/// A runtime UI override for one field. `label`/`widget` replace the contract value when set; `invisible`
/// drops the field from the served contract; `readonly` locks it. All fields are data — none reaches DDL.
#[derive(Debug, Clone)]
pub struct ViewOverride {
    pub model: String,
    pub field: String,
    pub label: Option<String>,
    pub widget: Option<String>,
    pub invisible: bool,
    pub readonly: bool,
}

impl Db {
    /// Creates the view-override table if absent (idempotent).
    pub async fn ensure_view_schema(&self) -> Result<(), DbError> {
        sqlx::query(ENSURE).execute(&self.pool).await?;
        Ok(())
    }

    /// Every configured view override (sorted by model, then field).
    pub async fn load_view_overrides(&self) -> Result<Vec<ViewOverride>, DbError> {
        let rows = sqlx::query(
            "SELECT model, field, label, widget, invisible, readonly FROM ir_ui_view ORDER BY model, field",
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
            })
            .collect())
    }

    /// Upserts a view override for `(model, field)`. Pure metadata — fully parameterized, no DDL.
    pub async fn set_view_override(
        &self,
        model: &str,
        field: &str,
        label: Option<&str>,
        widget: Option<&str>,
        invisible: bool,
        readonly: bool,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO ir_ui_view (model, field, label, widget, invisible, readonly) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (model, field) DO UPDATE SET label = EXCLUDED.label, \
             widget = EXCLUDED.widget, invisible = EXCLUDED.invisible, readonly = EXCLUDED.readonly",
        )
        .bind(model)
        .bind(field)
        .bind(label)
        .bind(widget)
        .bind(invisible)
        .bind(readonly)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
