//! Credential store + refresh-token store backing the auth lifecycle.
//!
//! Refresh tokens are STATEFUL: each is recorded by `jti` so it can be revoked (logout) and
//! rotated (every refresh invalidates the old one). A stolen-but-revoked refresh token is
//! rejected — the whole reason short access + long refresh exists.

use crate::{Db, DbError};
use sqlx::Row;

const ENSURE_USER: &str = "CREATE TABLE IF NOT EXISTS meshble_user \
     (id bigserial PRIMARY KEY, login text UNIQUE NOT NULL, password_hash text NOT NULL, \
      groups text NOT NULL DEFAULT '')";
const ENSURE_REFRESH: &str = "CREATE TABLE IF NOT EXISTS meshble_refresh \
     (jti text PRIMARY KEY, user_id bigint NOT NULL, expires_at timestamptz NOT NULL, \
      revoked boolean NOT NULL DEFAULT false)";

/// A user row used for credential verification.
pub struct UserRow {
    pub id: i64,
    pub password_hash: String,
    pub groups: Vec<String>,
}

fn split_groups(s: &str) -> Vec<String> {
    s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

impl Db {
    /// Creates the auth tables if absent (idempotent). Call once at startup.
    pub async fn ensure_auth_schema(&self) -> Result<(), DbError> {
        sqlx::query(ENSURE_USER).execute(&self.pool).await?;
        sqlx::query(ENSURE_REFRESH).execute(&self.pool).await?;
        Ok(())
    }

    /// Creates or updates a user by login (password hash + groups). Returns the user id.
    pub async fn upsert_user(
        &self,
        login: &str,
        password_hash: &str,
        groups: &[&str],
    ) -> Result<i64, DbError> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO meshble_user (login, password_hash, groups) VALUES ($1, $2, $3) \
             ON CONFLICT (login) DO UPDATE SET password_hash = EXCLUDED.password_hash, \
             groups = EXCLUDED.groups RETURNING id",
        )
        .bind(login)
        .bind(password_hash)
        .bind(groups.join(","))
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn find_user(&self, login: &str) -> Result<Option<UserRow>, DbError> {
        let row = sqlx::query("SELECT id, password_hash, groups FROM meshble_user WHERE login = $1")
            .bind(login)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| UserRow {
            id: r.get("id"),
            password_hash: r.get("password_hash"),
            groups: split_groups(&r.get::<String, _>("groups")),
        }))
    }

    /// The user's CURRENT groups — re-read on refresh so group changes take effect.
    pub async fn user_groups(&self, uid: i64) -> Result<Vec<String>, DbError> {
        let row = sqlx::query("SELECT groups FROM meshble_user WHERE id = $1")
            .bind(uid)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| split_groups(&r.get::<String, _>("groups"))).unwrap_or_default())
    }

    /// Records a refresh token id valid for `ttl_secs`.
    pub async fn store_refresh(&self, jti: &str, uid: i64, ttl_secs: i64) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO meshble_refresh (jti, user_id, expires_at) \
             VALUES ($1, $2, now() + ($3::bigint * interval '1 second'))",
        )
        .bind(jti)
        .bind(uid)
        .bind(ttl_secs)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Returns the user id if `jti` is an active refresh token (present, not revoked, not expired).
    pub async fn refresh_user(&self, jti: &str) -> Result<Option<i64>, DbError> {
        let row = sqlx::query(
            "SELECT user_id FROM meshble_refresh WHERE jti = $1 AND NOT revoked AND expires_at > now()",
        )
        .bind(jti)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get("user_id")))
    }

    pub async fn revoke_refresh(&self, jti: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE meshble_refresh SET revoked = true WHERE jti = $1")
            .bind(jti)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Atomically claims (revokes) an active refresh token, returning its user id. The check and
    /// the revoke happen in ONE statement, so two concurrent claims of the same token cannot both
    /// succeed: the loser's UPDATE affects zero rows → `None`. This prevents refresh double-spend.
    pub async fn claim_refresh(&self, jti: &str) -> Result<Option<i64>, DbError> {
        let row = sqlx::query(
            "UPDATE meshble_refresh SET revoked = true \
             WHERE jti = $1 AND NOT revoked AND expires_at > now() RETURNING user_id",
        )
        .bind(jti)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get("user_id")))
    }
}
