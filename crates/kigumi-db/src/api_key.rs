//! API-key store — long-lived, revocable machine credentials, the stateful sibling of the refresh
//! token. A key IMPERSONATES a user (`user_id`): it inherits that user's groups and company scope,
//! optionally narrowed by its own `scopes` (a subset of groups — a key can never exceed its user).
//! This crate stores only the Argon2 `hash` and looks up by `prefix`; minting and constant-time
//! verification live in `kigumi-auth` and the server, so kigumi-db stays crypto-free (as with
//! passwords). Revocation is a soft-delete (`revoked_at`), keeping the audit trail.

use crate::{Db, DbError};
use kigumi_core::Ctx;
use sqlx::Row;

const ENSURE: &str = "CREATE TABLE IF NOT EXISTS kigumi_api_key \
     (id bigserial PRIMARY KEY, prefix text UNIQUE NOT NULL, hash text NOT NULL, \
      user_id bigint NOT NULL, name text NOT NULL DEFAULT '', scopes text NOT NULL DEFAULT '', \
      expires_at timestamptz, last_used_at timestamptz, revoked_at timestamptz, \
      created_at timestamptz NOT NULL DEFAULT now())";

/// The verification view of a key (looked up by prefix): what the server needs to build a Ctx.
pub struct ApiKeyAuth {
    pub hash: String,
    pub user_id: i64,
    /// Group subset the key is restricted to (empty = all the user's groups).
    pub scopes: Vec<String>,
}

/// The management view of a key (never carries the hash or the secret).
pub struct ApiKeyInfo {
    pub id: i64,
    pub prefix: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub created_at: String,
}

fn split_scopes(s: &str) -> Vec<String> {
    s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

impl Db {
    /// Creates the API-key table if absent (idempotent).
    pub async fn ensure_api_key_schema(&self) -> Result<(), DbError> {
        sqlx::query(ENSURE).execute(&self.pool).await?;
        Ok(())
    }

    /// Stores a minted key. `hash` is the Argon2 of the secret (never the secret); `expires_in_secs`
    /// None means no expiry (revocation is the control). Returns the new row id.
    pub async fn create_api_key(
        &self,
        prefix: &str,
        hash: &str,
        user_id: i64,
        name: &str,
        scopes: &[String],
        expires_in_secs: Option<i64>,
    ) -> Result<i64, DbError> {
        let row = sqlx::query(
            "INSERT INTO kigumi_api_key (prefix, hash, user_id, name, scopes, expires_at) \
             VALUES ($1, $2, $3, $4, $5, CASE WHEN $6::bigint IS NULL THEN NULL \
                     ELSE now() + make_interval(secs => $6::bigint) END) RETURNING id",
        )
        .bind(prefix)
        .bind(hash)
        .bind(user_id)
        .bind(name)
        .bind(scopes.join(","))
        .bind(expires_in_secs)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get("id"))
    }

    /// Looks up a live key by its prefix for verification: not revoked, not expired. Returns None
    /// if absent/revoked/expired — the server then verifies the secret against `hash` constant-time.
    pub async fn find_api_key(&self, prefix: &str) -> Result<Option<ApiKeyAuth>, DbError> {
        let row = sqlx::query(
            "SELECT hash, user_id, scopes FROM kigumi_api_key \
             WHERE prefix = $1 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())",
        )
        .bind(prefix)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| ApiKeyAuth {
            hash: r.get("hash"),
            user_id: r.get("user_id"),
            scopes: split_scopes(&r.get::<String, _>("scopes")),
        }))
    }

    /// Builds the impersonated `Ctx` for a verified key: the user's groups NARROWED to the key's
    /// scopes (never widened), with the user's company scope (active derived from the allowed set
    /// when the stored active is NULL, mirroring `verify_access`). The one place this identity math
    /// lives — every host that authenticates a key (server, MCP) calls it, so the never-exceed-your-
    /// user contract has a single implementation. Crypto (verifying the secret) is the caller's job.
    pub async fn build_key_ctx(&self, user_id: i64, key_scopes: &[String]) -> Result<Ctx, DbError> {
        let (company_id, company_ids) = self.user_scope(user_id).await?;
        let mut groups = self.user_groups(user_id).await?;
        if !key_scopes.is_empty() {
            groups.retain(|g| key_scopes.iter().any(|s| s == g));
        }
        let mut ctx = Ctx::new(user_id, groups);
        if let Some(active) = company_id.or_else(|| company_ids.first().copied()) {
            ctx = ctx.in_companies(active, company_ids);
        }
        Ok(ctx)
    }

    /// Stamps `last_used_at` — but at most once per `throttle_secs` per key, so a busy key is not a
    /// write on every request. Best-effort: a failure here never fails the request (caller ignores).
    pub async fn touch_api_key(&self, prefix: &str, throttle_secs: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE kigumi_api_key SET last_used_at = now() WHERE prefix = $1 \
             AND (last_used_at IS NULL OR last_used_at < now() - make_interval(secs => $2::bigint))",
        )
        .bind(prefix)
        .bind(throttle_secs)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The live (non-revoked) keys owned by `user_id`, newest first. Never returns the hash.
    pub async fn list_api_keys(&self, user_id: i64) -> Result<Vec<ApiKeyInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT id, prefix, name, scopes, expires_at::text, last_used_at::text, created_at::text \
             FROM kigumi_api_key WHERE user_id = $1 AND revoked_at IS NULL ORDER BY id DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| ApiKeyInfo {
                id: r.get("id"),
                prefix: r.get("prefix"),
                name: r.get("name"),
                scopes: split_scopes(&r.get::<String, _>("scopes")),
                expires_at: r.get("expires_at"),
                last_used_at: r.get("last_used_at"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    /// Revokes a key by id regardless of owner — for the operator-trusted CLI. The HTTP path uses
    /// the user-scoped [`Db::revoke_api_key`] so a caller can only revoke their own.
    pub async fn revoke_api_key_admin(&self, id: i64) -> Result<bool, DbError> {
        let n = sqlx::query("UPDATE kigumi_api_key SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// Revokes a key (soft-delete). Scoped to `user_id` so a caller can only revoke their own;
    /// pass the key owner's id (an admin managing another user resolves that id first). Returns
    /// whether a live row was revoked.
    pub async fn revoke_api_key(&self, id: i64, user_id: i64) -> Result<bool, DbError> {
        let n = sqlx::query(
            "UPDATE kigumi_api_key SET revoked_at = now() \
             WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
        )
        .bind(id)
        .bind(user_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }
}
