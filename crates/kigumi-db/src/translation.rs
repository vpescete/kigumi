//! Runtime UI translations — the i18n sibling of the view-override layer. A translation replaces a
//! compile-time English label with a locale-specific one when the UI contract is served, applied as a
//! post-pass, so it takes effect WITHOUT recompiling and never touches stored data.
//!
//! Narrow by design: only contract METADATA is translated (field labels and selection option labels),
//! never record content. Keyed `(model, field, value, lang)` where `value = ''` targets the field's own
//! label and a non-empty `value` targets that selection option's label.

use crate::{Db, DbError};
use sqlx::Row;

const ENSURE: &str = "CREATE TABLE IF NOT EXISTS ir_translation \
     (id bigserial PRIMARY KEY, model text NOT NULL, field text NOT NULL, \
      value text NOT NULL DEFAULT '', lang text NOT NULL, text text NOT NULL, \
      UNIQUE (model, field, value, lang))";

/// One per-locale label. `value = ""` translates the field's own label; a non-empty `value` translates
/// that selection option's label. `text` is the translated string. Pure UI metadata — never reaches DDL.
#[derive(Debug, Clone)]
pub struct Translation {
    pub model: String,
    pub field: String,
    pub value: String,
    pub lang: String,
    pub text: String,
}

impl Db {
    /// Creates the translation table if absent (idempotent).
    pub async fn ensure_translation_schema(&self) -> Result<(), DbError> {
        sqlx::query(ENSURE).execute(&self.pool).await?;
        Ok(())
    }

    /// Every configured translation (sorted for a stable order).
    pub async fn load_translations(&self) -> Result<Vec<Translation>, DbError> {
        let rows = sqlx::query(
            "SELECT model, field, value, lang, text FROM ir_translation \
             ORDER BY model, field, value, lang",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| Translation {
                model: r.get("model"),
                field: r.get("field"),
                value: r.get("value"),
                lang: r.get("lang"),
                text: r.get("text"),
            })
            .collect())
    }

    /// Upserts one translation for `(model, field, value, lang)`. `value = ""` is the field's own label.
    /// Pure metadata — fully parameterized, no DDL.
    pub async fn set_translation(
        &self,
        model: &str,
        field: &str,
        value: &str,
        lang: &str,
        text: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO ir_translation (model, field, value, lang, text) VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (model, field, value, lang) DO UPDATE SET text = EXCLUDED.text",
        )
        .bind(model)
        .bind(field)
        .bind(value)
        .bind(lang)
        .bind(text)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
