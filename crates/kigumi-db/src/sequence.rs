//! Gapless document numbering (the `ir.sequence` equivalent), e.g. `SO/2026/0001`.
//!
//! `next_value` is a single atomic `UPDATE … RETURNING`, so concurrent callers on the same code are
//! serialized by the row lock and never get the same number (gapless under concurrency). A number is
//! consumed when the statement commits; tying consumption to a wider caller transaction (so a
//! rollback un-consumes it) is a later refinement.

use crate::{Db, DbError};
use sqlx::Row;

const ENSURE_SEQUENCE: &str = "CREATE TABLE IF NOT EXISTS kigumi_sequence \
     (code text PRIMARY KEY, prefix text NOT NULL DEFAULT '', suffix text NOT NULL DEFAULT '', \
      padding int NOT NULL DEFAULT 0, step bigint NOT NULL DEFAULT 1, \
      next_number bigint NOT NULL DEFAULT 1)";

impl Db {
    /// Creates the sequence table if absent (idempotent).
    pub async fn ensure_sequence_schema(&self) -> Result<(), DbError> {
        sqlx::query(ENSURE_SEQUENCE).execute(&self.pool).await?;
        Ok(())
    }

    /// Registers a sequence `code` with its formatting, if it does not already exist. Safe to call
    /// on every startup (an existing sequence keeps its counter).
    pub async fn ensure_sequence(
        &self,
        code: &str,
        prefix: &str,
        suffix: &str,
        padding: i32,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO kigumi_sequence (code, prefix, suffix, padding) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (code) DO NOTHING",
        )
        .bind(code)
        .bind(prefix)
        .bind(suffix)
        .bind(padding)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Ensures every `register_sequence!`-declared sequence exists (existing counters kept).
    /// Two modules claiming the same code is an author bug reported with both names. Deliberately
    /// LINKED-scoped, not installed-scoped (unlike seeds/migrations): a sequence row is inert
    /// shape, and the test kit runs without an install ledger.
    pub async fn ensure_registered_sequences(&self) -> Result<(), DbError> {
        self.ensure_sequence_schema().await?;
        let mut owner: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for s in kigumi_core::registered_sequences() {
            if let Some(prev) = owner.insert(s.code, s.module) {
                if prev != s.module {
                    return Err(DbError::Migration(format!(
                        "sequence code '{}' is declared by both '{}' and '{}'",
                        s.code, prev, s.module
                    )));
                }
            }
            self.ensure_sequence(s.code, s.prefix, s.suffix, s.padding).await?;
            // Drift check: the insert is DO NOTHING, so a row created by another binary (or tuned
            // by an operator) with a different shape silently wins. The DB stays the authority —
            // this only makes the divergence visible instead of formatting numbers surprisingly.
            let row =
                sqlx::query("SELECT prefix, suffix, padding FROM kigumi_sequence WHERE code = $1")
                    .bind(s.code)
                    .fetch_one(&self.pool)
                    .await?;
            let (prefix, suffix, padding): (String, String, i32) =
                (row.get("prefix"), row.get("suffix"), row.get("padding"));
            if (prefix.as_str(), suffix.as_str(), padding) != (s.prefix, s.suffix, s.padding) {
                eprintln!(
                    "warning: sequence '{}' is ('{}', '{}', {}) in the DB but module '{}' declares ('{}', '{}', {}); keeping the DB shape",
                    s.code, prefix, suffix, padding, s.module, s.prefix, s.suffix, s.padding
                );
            }
        }
        Ok(())
    }

    /// Returns the next formatted value for `code` (e.g. `SO/0001`) and advances the counter
    /// atomically. Errors if the code is unknown.
    pub async fn next_value(&self, code: &str) -> Result<String, DbError> {
        let row = sqlx::query(
            "UPDATE kigumi_sequence SET next_number = next_number + step \
             WHERE code = $1 RETURNING next_number - step AS n, prefix, suffix, padding",
        )
        .bind(code)
        .fetch_optional(&self.pool)
        .await?;
        let row = row.ok_or_else(|| DbError::BadInput(format!("unknown sequence code '{code}'")))?;
        let n: i64 = row.get("n");
        let prefix: String = row.get("prefix");
        let suffix: String = row.get("suffix");
        let padding: i32 = row.get("padding");
        let width = padding.max(0) as usize;
        Ok(format!("{prefix}{n:0width$}{suffix}"))
    }
}

#[cfg(test)]
mod tests {
    use crate::Db;

    #[tokio::test]
    async fn sequence_formats_and_advances() {
        let url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => {
                eprintln!("skipping: DATABASE_URL not set");
                return;
            }
        };
        let db = Db::connect(&url).await.unwrap();
        db.ensure_sequence_schema().await.unwrap();
        sqlx::query("DELETE FROM kigumi_sequence WHERE code = 'TST'")
            .execute(db.pool())
            .await
            .unwrap();

        db.ensure_sequence("TST", "SO/", "", 4).await.unwrap();
        assert_eq!(db.next_value("TST").await.unwrap(), "SO/0001");
        assert_eq!(db.next_value("TST").await.unwrap(), "SO/0002");
        assert_eq!(db.next_value("TST").await.unwrap(), "SO/0003");

        // ensure is idempotent: re-registering does not reset the counter.
        db.ensure_sequence("TST", "SO/", "", 4).await.unwrap();
        assert_eq!(db.next_value("TST").await.unwrap(), "SO/0004");

        assert!(db.next_value("NOPE").await.is_err(), "unknown code errors");

        sqlx::query("DELETE FROM kigumi_sequence WHERE code = 'TST'")
            .execute(db.pool())
            .await
            .unwrap();
    }
}
