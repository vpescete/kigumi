//! DB-backed ACL overrides (the runtime half of the hybrid ir.model.access, decision D12).
//!
//! The compile-time `register_acls!` registrations are the GUARANTEED module baseline. This store
//! holds ADDITIVE grants an admin configures at runtime: they union with the static baseline (access
//! is granted if ANY ACL matches), so a DB grant can only WIDEN access — it can never revoke a
//! static grant. The static set therefore stays a floor. Loaded once at server startup.

use crate::{Db, DbError};
use kigumi_core::{Acl, Domain, Operation, RecordRule, RuleDomain};
use sqlx::Row;

const ENSURE_ACL: &str = "CREATE TABLE IF NOT EXISTS kigumi_acl \
     (id bigserial PRIMARY KEY, model text NOT NULL, grp text NOT NULL, \
      can_read boolean NOT NULL DEFAULT false, can_write boolean NOT NULL DEFAULT false, \
      can_create boolean NOT NULL DEFAULT false, can_delete boolean NOT NULL DEFAULT false, \
      UNIQUE (model, grp))";
// Runtime record rules (the DB half of the hybrid ir.rule, D12 part 2): `grp` is a CSV of groups
// (empty = global), `ops` a CSV subset of r/w/c/d, `domain` the portable JSON domain AST.
const ENSURE_RULE: &str = "CREATE TABLE IF NOT EXISTS kigumi_rule \
     (id bigserial PRIMARY KEY, model text NOT NULL, grp text NOT NULL DEFAULT '', \
      ops text NOT NULL DEFAULT 'r', domain text NOT NULL, active boolean NOT NULL DEFAULT true)";

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
    /// Creates the ACL- and rule-override tables if absent (idempotent).
    pub async fn ensure_access_schema(&self) -> Result<(), DbError> {
        sqlx::query(ENSURE_ACL).execute(&self.pool).await?;
        sqlx::query(ENSURE_RULE).execute(&self.pool).await?;
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
            "INSERT INTO kigumi_acl (model, grp, can_read, can_write, can_create, can_delete) \
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
        sqlx::query("DELETE FROM kigumi_acl WHERE model = $1 AND grp = $2")
            .bind(model)
            .bind(group)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Lists the configured DB ACLs (for `kigumi acl list`).
    pub async fn list_db_acls(&self) -> Result<Vec<AclRow>, DbError> {
        let rows = sqlx::query(
            "SELECT model, grp, can_read, can_write, can_create, can_delete \
             FROM kigumi_acl ORDER BY model, grp",
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

    /// Adds a runtime record rule. `groups` is a CSV (empty = global), `ops` a CSV subset of
    /// r/w/c/d, `domain` the JSON AST. The domain is parsed up front so a malformed rule is rejected
    /// at write time, not at load. Returns the new rule id.
    pub async fn set_db_rule(
        &self,
        model: &str,
        groups: &str,
        ops: &str,
        domain_json: &str,
    ) -> Result<i64, DbError> {
        Domain::from_json(domain_json)
            .map_err(|e| DbError::BadInput(format!("invalid rule domain: {e:?}")))?;
        if ops_from_csv(ops).is_empty() {
            return Err(DbError::BadInput("rule needs at least one op (r/w/c/d)".to_string()));
        }
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO kigumi_rule (model, grp, ops, domain) VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(model)
        .bind(groups)
        .bind(ops)
        .bind(domain_json)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    /// Removes a runtime record rule by id (the static baseline is unaffected).
    pub async fn remove_db_rule(&self, id: i64) -> Result<(), DbError> {
        sqlx::query("DELETE FROM kigumi_rule WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// Lists the configured DB record rules (for `kigumi rule list`).
    pub async fn list_db_rules(&self) -> Result<Vec<RuleRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, model, grp, ops, domain, active FROM kigumi_rule ORDER BY model, id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| RuleRow {
                id: r.get("id"),
                model: r.get("model"),
                groups: r.get("grp"),
                ops: r.get("ops"),
                domain: r.get("domain"),
                active: r.get("active"),
            })
            .collect())
    }

    /// Loads the ACTIVE DB record rules as `RecordRule`s whose identifiers are leaked to `'static`
    /// and whose domain is parsed into a `RuleDomain::Owned`. Call ONCE at startup; the result is
    /// appended to the static (compiled-in) rules — both flow through the same engine, so a DB rule
    /// adds to (never silently removes) the static baseline (global rules AND, group rules OR).
    pub async fn load_rules_static(&self) -> Result<Vec<RecordRule>, DbError> {
        let mut out = Vec::new();
        for r in self.list_db_rules().await?.into_iter().filter(|r| r.active) {
            let domain = Domain::from_json(&r.domain)
                .map_err(|e| DbError::BadInput(format!("rule {}: invalid domain: {e:?}", r.id)))?;
            let ops = ops_from_csv(&r.ops);
            if ops.is_empty() {
                continue;
            }
            let groups: Vec<&'static str> = r
                .groups
                .split(',')
                .map(|g| g.trim())
                .filter(|g| !g.is_empty())
                .map(|g| &*Box::leak(g.to_string().into_boxed_str()))
                .collect();
            out.push(RecordRule {
                model: Box::leak(r.model.into_boxed_str()),
                groups: Box::leak(groups.into_boxed_slice()),
                ops: Box::leak(ops.into_boxed_slice()),
                domain: RuleDomain::Owned(domain),
            });
        }
        Ok(out)
    }
}

/// One configured DB record rule (for listing/inspection).
pub struct RuleRow {
    pub id: i64,
    pub model: String,
    pub groups: String,
    pub ops: String,
    pub domain: String,
    pub active: bool,
}

/// Parses a CSV of operation codes (r/w/c/d), dropping anything unrecognized.
fn ops_from_csv(s: &str) -> Vec<Operation> {
    s.split(',')
        .filter_map(|o| match o.trim() {
            "r" => Some(Operation::Read),
            "w" => Some(Operation::Write),
            "c" => Some(Operation::Create),
            "d" => Some(Operation::Delete),
            _ => None,
        })
        .collect()
}
