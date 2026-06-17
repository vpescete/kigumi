//! Versioned migration engine.
//!
//! Unlike Odoo's `migrations/<version>/*.py` (free Python run via the loader), a migration here is
//! declarative, versioned SQL with a SemVer target. State is tracked in `meshble_module` (current
//! version) and `meshble_migration` (one row per applied version, for audit + per-version
//! idempotency). Each install/upgrade is:
//! - ATOMIC: a single transaction — every step and the version bump commit together, or nothing.
//! - SERIALIZED: a per-module `pg_advisory_xact_lock` prevents concurrent callers from racing.
//! - IDEMPOTENT: re-running at the same version is a no-op; already-applied versions are skipped.
//!
//! Known limit: the `migrations` slice is assumed to be the complete, ordered chain for the
//! module. The engine cannot detect a *semantically required* intermediate step that was never
//! authored — but atomicity means a step depending on a missing one fails and rolls back the whole
//! upgrade rather than silently corrupting the schema.

use crate::{Db, DbError};
use meshble_core::ResolvedModel;
use meshble_schema::to_ddl;
use semver::Version;
use sqlx::Row;
use std::collections::HashSet;

/// One migration step: the SQL that brings a module's schema/data up to `version`.
#[derive(Clone, Copy)]
pub struct Migration {
    pub version: &'static str,
    pub statements: &'static [&'static str],
}

#[derive(Debug, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// Fresh install: the table was created from the current model and the version recorded.
    Installed,
    /// Upgraded from `from` to `to` by running `steps` migration steps.
    Upgraded { from: String, to: String, steps: usize },
    /// Already at (or above) the target version: nothing to do.
    UpToDate,
}

fn parse(s: &str) -> Result<Version, DbError> {
    Version::parse(s).map_err(|e| DbError::Migration(format!("bad version {s:?}: {e}")))
}

/// Rejects a migration list with two entries for the same version (a programming error).
fn check_no_duplicate_versions(migrations: &[Migration]) -> Result<(), DbError> {
    let mut seen = HashSet::new();
    for m in migrations {
        let v = parse(m.version)?;
        if !seen.insert(v.to_string()) {
            return Err(DbError::Migration(format!("duplicate migration version {}", m.version)));
        }
    }
    Ok(())
}

const ENSURE_MODULE: &str =
    "CREATE TABLE IF NOT EXISTS meshble_module (name text PRIMARY KEY, version text NOT NULL)";
const ENSURE_HISTORY: &str = "CREATE TABLE IF NOT EXISTS meshble_migration \
     (module text NOT NULL, version text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now(), \
      PRIMARY KEY (module, version))";
const UPSERT_MODULE: &str = "INSERT INTO meshble_module (name, version) VALUES ($1, $2) \
     ON CONFLICT (name) DO UPDATE SET version = EXCLUDED.version";

impl Db {
    /// Whether this database was migrated BEFORE module-selection existed — i.e. the per-model
    /// migration ledger `meshble_module` has rows. Used by `migrate` to tell a truly-fresh DB (install
    /// `base` only) from an existing one upgraded to module-selection (back-fill: keep all modules that
    /// were already installed, so nothing disappears).
    pub async fn has_prior_migration(&self) -> Result<bool, DbError> {
        let reg: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('meshble_module')::text").fetch_one(&self.pool).await?;
        if reg.is_none() {
            return Ok(false);
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM meshble_module").fetch_one(&self.pool).await?;
        Ok(count > 0)
    }

    /// Installs `model` for `module` at `target_version` (creating the table on first install), or
    /// upgrades an existing install by running the pending migration steps up to `target_version`.
    /// Atomic, serialized per module, and idempotent.
    pub async fn install_or_upgrade(
        &self,
        model: &ResolvedModel,
        module: &str,
        target_version: &str,
        migrations: &[Migration],
    ) -> Result<MigrationOutcome, DbError> {
        let target = parse(target_version)?;
        check_no_duplicate_versions(migrations)?;

        let mut tx = self.pool.begin().await?;
        sqlx::query(ENSURE_MODULE).execute(&mut *tx).await?;
        sqlx::query(ENSURE_HISTORY).execute(&mut *tx).await?;
        // Serialize concurrent install/upgrade for this module (lock auto-releases at tx end).
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
            .bind(module)
            .execute(&mut *tx)
            .await?;

        let installed: Option<String> = sqlx::query("SELECT version FROM meshble_module WHERE name = $1")
            .bind(module)
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| r.get::<String, _>("version"));

        let outcome = match installed {
            None => {
                // CREATE TABLE IF NOT EXISTS so a raced/retried install is a no-op, not an error.
                let ddl = to_ddl(model).replacen("CREATE TABLE ", "CREATE TABLE IF NOT EXISTS ", 1);
                sqlx::query(&ddl).execute(&mut *tx).await?;
                sqlx::query(UPSERT_MODULE).bind(module).bind(target_version).execute(&mut *tx).await?;
                MigrationOutcome::Installed
            }
            Some(installed_str) => {
                let installed_v = parse(&installed_str)?;
                if installed_v >= target {
                    MigrationOutcome::UpToDate
                } else {
                    let applied: HashSet<String> =
                        sqlx::query("SELECT version FROM meshble_migration WHERE module = $1")
                            .bind(module)
                            .fetch_all(&mut *tx)
                            .await?
                            .iter()
                            .map(|r| r.get::<String, _>("version"))
                            .collect();

                    let mut pending: Vec<(Version, &Migration)> = Vec::new();
                    for m in migrations {
                        let v = parse(m.version)?;
                        if v > installed_v && v <= target && !applied.contains(&v.to_string()) {
                            pending.push((v, m));
                        }
                    }
                    pending.sort_by(|a, b| a.0.cmp(&b.0));

                    // Reachability: a non-empty pending set must reach the target, so the recorded
                    // version never outruns the migrations that actually ran.
                    if let Some((maxv, _)) = pending.last() {
                        if *maxv != target {
                            return Err(DbError::Migration(format!(
                                "target {target_version} is unreachable; highest available migration is {maxv}"
                            )));
                        }
                    }

                    for (v, m) in &pending {
                        for stmt in m.statements {
                            sqlx::query(stmt).execute(&mut *tx).await?;
                        }
                        sqlx::query("INSERT INTO meshble_migration (module, version) VALUES ($1, $2)")
                            .bind(module)
                            .bind(v.to_string())
                            .execute(&mut *tx)
                            .await?;
                    }
                    sqlx::query(UPSERT_MODULE).bind(module).bind(target_version).execute(&mut *tx).await?;
                    MigrationOutcome::Upgraded {
                        from: installed_str,
                        to: target_version.to_string(),
                        steps: pending.len(),
                    }
                }
            }
        };
        tx.commit().await?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_migration_versions() {
        static DUP: &[Migration] = &[
            Migration { version: "1.1.0", statements: &[] },
            Migration { version: "1.1.0", statements: &[] },
        ];
        assert!(matches!(check_no_duplicate_versions(DUP), Err(DbError::Migration(_))));
    }

    #[test]
    fn accepts_distinct_versions() {
        static OK: &[Migration] = &[
            Migration { version: "1.1.0", statements: &[] },
            Migration { version: "1.2.0", statements: &[] },
        ];
        assert!(check_no_duplicate_versions(OK).is_ok());
    }
}
