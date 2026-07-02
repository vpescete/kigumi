//! Installed-module registry — the runtime half of module selection (Odoo's `ir.module.module`,
//! decision: approach B). A module is AVAILABLE when its crate is linked into the binary
//! (compile-time); it is INSTALLED when it has a row here. `migrate`/`serve` only materialise and
//! serve the models of installed modules.
//!
//! Uninstall = DELETE the row: the module stops being migrated/served, but its tables and data are
//! KEPT (non-destructive, reversible) — deliberately unlike Odoo, which drops them.

use crate::{Db, DbError};
use kigumi_core::migration_plan;
use sqlx::Row;
use std::future::Future;
use std::pin::Pin;

/// A module's reference-data seeder (emitted by `register_seed!`). Runs at EVERY migrate for
/// installed modules, in dependency order — so bodies must be idempotent and never overwrite an
/// operator change (guard with count/exists checks: the DB is the authority).
pub type SeedFn =
    for<'a> fn(&'a Db) -> Pin<Box<dyn Future<Output = Result<(), DbError>> + Send + 'a>>;

pub struct SeedRegistration {
    pub module: &'static str,
    pub func: SeedFn,
}
kigumi_core::inventory::collect!(SeedRegistration);

/// A versioned per-module DATA migration (emitted by `register_migration!`): the upgrade contract.
/// Runs when migrate finds the module installed at a ledger version < `to_version` while the
/// linked crate is >= `to_version`. Steps run in semver order, each bumping the ledger to its
/// `to_version` — a failed upgrade resumes exactly where it stopped, so bodies must be idempotent
/// (at-least-once, like jobs). A fresh install runs no migrations: the declarative schema is
/// already current-shape, and the ledger starts at the linked version.
pub type DataMigrationFn = SeedFn;

pub struct DataMigrationRegistration {
    pub module: &'static str,
    pub to_version: &'static str,
    pub func: DataMigrationFn,
}
kigumi_core::inventory::collect!(DataMigrationRegistration);

const ENSURE: &str = "CREATE TABLE IF NOT EXISTS installed_module \
     (name text PRIMARY KEY, installed_version text NOT NULL, \
      installed_at timestamptz NOT NULL DEFAULT now())";

impl Db {
    /// Creates the installed-module table if absent (idempotent).
    pub async fn ensure_module_schema(&self) -> Result<(), DbError> {
        sqlx::query(ENSURE).execute(&self.pool).await?;
        Ok(())
    }

    /// The names of the currently-installed modules (sorted).
    pub async fn installed_modules(&self) -> Result<Vec<String>, DbError> {
        let rows = sqlx::query("SELECT name FROM installed_module ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| r.get::<String, _>("name")).collect())
    }

    pub async fn is_module_installed(&self, name: &str) -> Result<bool, DbError> {
        let row = sqlx::query("SELECT 1 FROM installed_module WHERE name = $1")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    /// Records a module as installed (or updates its recorded version). Migrating its tables is the
    /// caller's job.
    pub async fn mark_module_installed(&self, name: &str, version: &str) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO installed_module (name, installed_version) VALUES ($1, $2) \
             ON CONFLICT (name) DO UPDATE SET installed_version = EXCLUDED.installed_version",
        )
        .bind(name)
        .bind(version)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Removes a module from the installed set. Its tables and data are left untouched (disable, not
    /// drop), so re-installing restores it with the data intact.
    pub async fn mark_module_uninstalled(&self, name: &str) -> Result<(), DbError> {
        sqlx::query("DELETE FROM installed_module WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Migrates the SCHEMA of every currently-installed module (the model tables in FK-dependency
    /// order, then Many2many junctions, then the framework indexes and the cron ledger). Idempotent —
    /// re-running at the same version is a no-op. This is the reusable core of the CLI's
    /// `migrate_installed`; the CLI then layers module reference-data seeding on top. The server calls
    /// it on a live install so a newly-installed module's tables exist without a restart. Reference-data
    /// seeds are NOT run here (they live with the host).
    pub async fn migrate_installed_schema(&self) -> Result<(), DbError> {
        let installed = self.installed_modules().await?;
        let plan = migration_plan().map_err(DbError::Migration)?;
        let targets: Vec<_> = plan.iter().filter(|t| installed.iter().any(|m| m == t.module)).collect();
        for t in &targets {
            self.install_or_upgrade(&t.model, t.model.name, &t.version, &[]).await?;
        }
        // Additive safety net: materialize any model field added since install (a new #[field] on an
        // already-installed table) so an in-place upgrade needs no DB reset. Additive only.
        for t in &targets {
            self.ensure_model_columns(&t.model).await?;
        }
        // Second pass: Many2many junction tables, once every model table exists (FKs need both ends).
        for t in &targets {
            self.create_m2m_relations(&t.model).await?;
        }
        self.ensure_mail_indexes().await?;
        self.ensure_transient_defaults().await?;
        self.ensure_stock_indexes().await?;
        self.ensure_event_schema().await?;
        self.ensure_crons().await?;
        self.ensure_registered_sequences().await?;
        // Upgrades BEFORE seeds: migrations transform existing data (the additive columns they
        // backfill were just materialized above); seeds then top up missing reference data.
        self.run_pending_upgrades().await?;
        self.run_installed_seeds().await?;
        Ok(())
    }

    /// Runs every pending `register_migration!` step: for each installed module whose linked crate
    /// is NEWER than the ledger version, applies the registered migrations with
    /// `ledger < to_version <= linked` in semver order, bumping the ledger after each step (a
    /// failure resumes from the failed step on the next migrate). Modules with a version bump but
    /// no registered steps just get their ledger bumped. A linked version OLDER than the ledger is
    /// a refused downgrade. Returns the applied `(module, to_version)` steps.
    pub async fn run_pending_upgrades(&self) -> Result<Vec<(String, String)>, DbError> {
        use semver::Version;
        fn bad(m: &str, e: semver::Error) -> DbError {
            DbError::Migration(format!("{m}: {e}"))
        }

        let mods = kigumi_core::resolve_modules().map_err(|e| DbError::Migration(format!("{e:?}")))?;
        // Author-bug validation over ALL linked registrations, installed or not: an unknown module
        // name, an unparseable/duplicate to_version, or a step beyond the linked crate version
        // (a forgotten manifest bump) must fail migrate loudly, not skip silently.
        let mut seen: std::collections::HashSet<(&str, &str)> = std::collections::HashSet::new();
        for r in kigumi_core::inventory::iter::<DataMigrationRegistration>() {
            let Some(m) = mods.iter().find(|m| m.name == r.module) else {
                return Err(DbError::Migration(format!(
                    "migration to {} registered for unknown module '{}'",
                    r.to_version, r.module
                )));
            };
            let to = Version::parse(r.to_version).map_err(|e| bad(r.module, e))?;
            let linked = Version::parse(m.version).map_err(|e| bad(r.module, e))?;
            if to > linked {
                return Err(DbError::Migration(format!(
                    "module '{}' registers a migration to {to} but the linked crate is {linked} — bump the manifest version",
                    r.module
                )));
            }
            if !seen.insert((r.module, r.to_version)) {
                return Err(DbError::Migration(format!(
                    "duplicate migration to {to} for module '{}'",
                    r.module
                )));
            }
        }

        let ledger: Vec<(String, String)> =
            sqlx::query("SELECT name, installed_version FROM installed_module")
                .fetch_all(&self.pool)
                .await?
                .into_iter()
                .map(|row| (row.get(0), row.get(1)))
                .collect();

        let mut applied = Vec::new();
        for (name, db_ver_str) in ledger {
            // A ledger row without a linked crate (module removed from the binary) is left alone —
            // uninstall is an explicit operator action, not a side effect of a build change.
            let Some(m) = mods.iter().find(|m| m.name == name) else { continue };
            let db_ver = Version::parse(&db_ver_str).map_err(|e| bad(&name, e))?;
            let linked = Version::parse(m.version).map_err(|e| bad(&name, e))?;
            if linked < db_ver {
                return Err(DbError::Migration(format!(
                    "module '{name}' is installed at {db_ver} but this binary links {linked}: downgrades are not supported"
                )));
            }
            if linked == db_ver {
                continue;
            }
            let mut steps: Vec<(Version, &DataMigrationRegistration)> = Vec::new();
            for r in kigumi_core::inventory::iter::<DataMigrationRegistration>() {
                if r.module == name {
                    let to = Version::parse(r.to_version).map_err(|e| bad(&name, e))?;
                    if db_ver < to && to <= linked {
                        steps.push((to, r));
                    }
                }
            }
            steps.sort_by(|a, b| a.0.cmp(&b.0));
            for (to, reg) in steps {
                (reg.func)(self).await?;
                self.mark_module_installed(&name, reg.to_version).await?;
                println!("upgraded module {name} to {to}");
                applied.push((name.clone(), reg.to_version.to_string()));
            }
            self.mark_module_installed(&name, m.version).await?;
        }
        Ok(applied)
    }

    /// Runs every `register_seed!` body whose module is installed, in dependency order (a module
    /// seeds after everything it depends on — account's chart needs base's company).
    pub async fn run_installed_seeds(&self) -> Result<(), DbError> {
        let installed: std::collections::HashSet<String> =
            self.installed_modules().await?.into_iter().collect();
        // Kahn's algorithm over the linked manifests, restricted to installed modules; candidates
        // are taken name-sorted each round so the order is deterministic.
        let mods: Vec<_> = kigumi_core::resolve_modules()
            .map_err(|e| DbError::Migration(format!("{e:?}")))?
            .into_iter()
            .filter(|m| installed.contains(m.name))
            .collect();
        let mut done: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut order: Vec<&str> = Vec::with_capacity(mods.len());
        while order.len() < mods.len() {
            let mut ready: Vec<&str> = mods
                .iter()
                .filter(|m| !done.contains(m.name))
                .filter(|m| {
                    m.depends.iter().all(|d| done.contains(d.name) || !installed.contains(d.name))
                })
                .map(|m| m.name)
                .collect();
            if ready.is_empty() {
                return Err(DbError::Migration("module dependency cycle in seed ordering".to_string()));
            }
            ready.sort_unstable();
            for name in ready {
                done.insert(name);
                order.push(name);
            }
        }
        for name in order {
            for reg in kigumi_core::inventory::iter::<SeedRegistration>() {
                if reg.module == name {
                    (reg.func)(self).await?;
                }
            }
        }
        Ok(())
    }
}
