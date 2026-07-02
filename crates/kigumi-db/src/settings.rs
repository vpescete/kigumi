//! Runtime settings store (the `ir.config_parameter` equivalent). These are mutable WITHOUT a
//! restart and the DATABASE is their single authority (boot-time config lives in kigumi.toml/env;
//! see docs/OPERATIONS.md §2.1). Keys like `base_url`, `mode`, `neutralized`, `banner` live here.

use crate::{Db, DbError};
use sqlx::Row;

const ENSURE_SETTING: &str = "CREATE TABLE IF NOT EXISTS kigumi_setting \
     (key text PRIMARY KEY, value text NOT NULL, vtype text NOT NULL DEFAULT 'string')";

impl Db {
    /// Creates the settings table if absent (idempotent).
    pub async fn ensure_setting_schema(&self) -> Result<(), DbError> {
        sqlx::query(ENSURE_SETTING).execute(&self.pool).await?;
        Ok(())
    }

    /// Sets (upserts) a runtime setting. `vtype` is a hint for typed readers (string/bool/int/json).
    pub async fn set_setting(&self, key: &str, value: &str, vtype: &str) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO kigumi_setting (key, value, vtype) VALUES ($1, $2, $3) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, vtype = EXCLUDED.vtype",
        )
        .bind(key)
        .bind(value)
        .bind(vtype)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Seeds a default value only if the key is absent (install-time defaults never overwrite an
    /// operator's runtime change).
    pub async fn seed_setting(&self, key: &str, value: &str, vtype: &str) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO kigumi_setting (key, value, vtype) VALUES ($1, $2, $3) \
             ON CONFLICT (key) DO NOTHING",
        )
        .bind(key)
        .bind(value)
        .bind(vtype)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Reads a setting's raw string value.
    pub async fn get_setting(&self, key: &str) -> Result<Option<String>, DbError> {
        let row = sqlx::query("SELECT value FROM kigumi_setting WHERE key = $1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<String, _>("value")))
    }

    /// All settings (key, value, vtype), ordered by key — for `config print` and the instance route.
    pub async fn all_settings(&self) -> Result<Vec<(String, String, String)>, DbError> {
        let rows = sqlx::query("SELECT key, value, vtype FROM kigumi_setting ORDER BY key")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get("key"), r.get("value"), r.get("vtype")))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use crate::Db;

    #[tokio::test]
    async fn settings_seed_set_and_read() {
        let url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => {
                eprintln!("skipping: DATABASE_URL not set");
                return;
            }
        };
        let db = Db::connect(&url).await.unwrap();
        db.ensure_setting_schema().await.unwrap();
        sqlx::query("DELETE FROM kigumi_setting WHERE key = 'test.k'").execute(db.pool()).await.unwrap();

        // seed sets the initial value...
        db.seed_setting("test.k", "first", "string").await.unwrap();
        assert_eq!(db.get_setting("test.k").await.unwrap().as_deref(), Some("first"));
        // ...and never overwrites an existing value.
        db.seed_setting("test.k", "second", "string").await.unwrap();
        assert_eq!(db.get_setting("test.k").await.unwrap().as_deref(), Some("first"));
        // set overwrites.
        db.set_setting("test.k", "third", "string").await.unwrap();
        assert_eq!(db.get_setting("test.k").await.unwrap().as_deref(), Some("third"));
        assert!(db.get_setting("missing.k").await.unwrap().is_none());

        sqlx::query("DELETE FROM kigumi_setting WHERE key = 'test.k'").execute(db.pool()).await.unwrap();
    }
}
