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
        Ok(())
    }
}
