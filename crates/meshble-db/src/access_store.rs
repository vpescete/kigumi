//! DB-backed ACL overrides (the runtime half of the hybrid ir.model.access, decision D12).
//!
//! The compile-time `register_acls!` registrations are the GUARANTEED module baseline. This store
//! holds ADDITIVE grants an admin configures at runtime: they union with the static baseline (access
//! is granted if ANY ACL matches), so a DB grant can only WIDEN access — it can never revoke a
//! static grant. The static set therefore stays a floor. Loaded once at server startup.

use crate::{Db, DbError};
use meshble_core::Acl;
use sqlx::Row;

const ENSURE_ACL: &str = "CREATE TABLE IF NOT EXISTS meshble_acl \
     (id bigserial PRIMARY KEY, model text NOT NULL, grp text NOT NULL, \
      can_read boolean NOT NULL DEFAULT false, can_write boolean NOT NULL DEFAULT false, \
      can_create boolean NOT NULL DEFAULT false, can_delete boolean NOT NULL DEFAULT false, \
      UNIQUE (model, grp))";

/// One configured ACL row (for listing/inspection).
pub struct AclRow {
    pub model: String,
    pub group: String,
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub delete: bool,
}

impl Db {
    /// Creates the ACL-override table if absent (idempotent).
    pub async fn ensure_access_schema(&self) -> Result<(), DbError> {
        sqlx::query(ENSURE_ACL).execute(&self.pool).await?;
        Ok(())
    }

    /// Grants (or updates) a DB ACL for `(model, group)`.
    pub async fn set_db_acl(
        &self,
        model: &str,
        group: &str,
        read: bool,
        write: bool,
        create: bool,
        delete: bool,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO meshble_acl (model, grp, can_read, can_write, can_create, can_delete) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (model, grp) DO UPDATE SET can_read = EXCLUDED.can_read, \
             can_write = EXCLUDED.can_write, can_create = EXCLUDED.can_create, \
             can_delete = EXCLUDED.can_delete",
        )
        .bind(model)
        .bind(group)
        .bind(read)
        .bind(write)
        .bind(create)
        .bind(delete)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Removes a DB ACL override for `(model, group)` (the static baseline is unaffected).
    pub async fn remove_db_acl(&self, model: &str, group: &str) -> Result<(), DbError> {
        sqlx::query("DELETE FROM meshble_acl WHERE model = $1 AND grp = $2")
            .bind(model)
            .bind(group)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Lists the configured DB ACLs (for `meshble acl list`).
    pub async fn list_db_acls(&self) -> Result<Vec<AclRow>, DbError> {
        let rows = sqlx::query(
            "SELECT model, grp, can_read, can_write, can_create, can_delete \
             FROM meshble_acl ORDER BY model, grp",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| AclRow {
                model: r.get("model"),
                group: r.get("grp"),
                read: r.get("can_read"),
                write: r.get("can_write"),
                create: r.get("can_create"),
                delete: r.get("can_delete"),
            })
            .collect())
    }

    /// Loads the DB ACLs as `Acl` values whose identifiers are leaked to `'static`. Call ONCE at
    /// startup (the leak is bounded by the number of configured rows); the result unions with the
    /// static baseline before being handed to the server for the process lifetime.
    pub async fn load_acls_static(&self) -> Result<Vec<Acl>, DbError> {
        Ok(self
            .list_db_acls()
            .await?
            .into_iter()
            .map(|r| Acl {
                model: Box::leak(r.model.into_boxed_str()),
                group: Box::leak(r.group.into_boxed_str()),
                read: r.read,
                write: r.write,
                create: r.create,
                delete: r.delete,
            })
            .collect())
    }
}
