//! OIDC (SSO) server-side state: the short-lived per-login flow rows that bind an in-flight
//! authorization request (state → nonce + PKCE verifier) across the browser round-trip, and the
//! just-in-time provisioning of a user from a verified OIDC identity. Everything lives on Postgres,
//! consistent with the rest of the framework — no external session store.

use crate::{Db, DbError, UserRow};
use sqlx::Row;

/// Password hash stored for a JIT-provisioned OIDC user. It is deliberately NOT a valid PHC string, so
/// `verify_password` can never match it — the account is OIDC-only until an admin sets a real password.
pub const OIDC_NO_PASSWORD: &str = "!";

/// How long an in-flight OIDC login may sit between `/start` and `/callback` before it is rejected.
const FLOW_TTL: &str = "10 minutes";

const ENSURE: &str = "CREATE TABLE IF NOT EXISTS kigumi_oidc_flow \
     (state text PRIMARY KEY, nonce text NOT NULL, pkce_verifier text NOT NULL, \
      created_at timestamptz NOT NULL DEFAULT now())";

impl Db {
    /// Creates the OIDC flow table if absent (idempotent).
    pub async fn ensure_oidc_schema(&self) -> Result<(), DbError> {
        sqlx::query(ENSURE).execute(&self.pool).await?;
        Ok(())
    }

    /// Records an in-flight login: `state` (the CSRF token) → `nonce` + `pkce_verifier`, to be consumed
    /// once by the callback. Opportunistically drops expired rows so the table cannot grow unbounded.
    pub async fn store_oidc_flow(&self, state: &str, nonce: &str, pkce_verifier: &str) -> Result<(), DbError> {
        sqlx::query("INSERT INTO kigumi_oidc_flow (state, nonce, pkce_verifier) VALUES ($1, $2, $3)")
            .bind(state)
            .bind(nonce)
            .bind(pkce_verifier)
            .execute(&self.pool)
            .await?;
        sqlx::query(&format!(
            "DELETE FROM kigumi_oidc_flow WHERE created_at <= now() - interval '{FLOW_TTL}'"
        ))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Atomically consumes the flow for `state`, returning `(nonce, pkce_verifier)`. A one-shot DELETE:
    /// the row is gone whether or not it was still valid, so a state cannot be replayed. Returns `None`
    /// for an unknown, already-used, or expired state — all of which the callback must reject.
    pub async fn take_oidc_flow(&self, state: &str) -> Result<Option<(String, String)>, DbError> {
        let row = sqlx::query(&format!(
            "DELETE FROM kigumi_oidc_flow WHERE state = $1 AND created_at > now() - interval '{FLOW_TTL}' \
             RETURNING nonce, pkce_verifier"
        ))
        .bind(state)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| (r.get("nonce"), r.get("pkce_verifier"))))
    }

    /// Resolves the user for a verified OIDC email: an existing user (linking by email) or a
    /// just-in-time create with an unusable password and NO groups (the caller can authenticate but
    /// sees nothing until an admin grants groups). Race-safe: a concurrent first login is absorbed by
    /// `ON CONFLICT DO NOTHING`, and both requests read back the same row.
    pub async fn find_or_create_oidc_user(&self, login: &str) -> Result<UserRow, DbError> {
        if let Some(u) = self.find_user(login).await? {
            return Ok(u);
        }
        sqlx::query(
            "INSERT INTO kigumi_user (login, password_hash, groups) VALUES ($1, $2, '') \
             ON CONFLICT (login) DO NOTHING",
        )
        .bind(login)
        .bind(OIDC_NO_PASSWORD)
        .execute(&self.pool)
        .await?;
        self.find_user(login).await?.ok_or(DbError::Sql(sqlx::Error::RowNotFound))
    }
}
