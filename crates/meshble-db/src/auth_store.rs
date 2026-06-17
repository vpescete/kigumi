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
// Multi-company assignment, added in place so older instances upgrade without a destructive migration.
// company_id = the user's ACTIVE company; company_ids = the CSV of companies the user MAY access.
// Both empty → unrestricted (back-compat with the single-company default).
const ENSURE_USER_COMPANY: &str =
    "ALTER TABLE meshble_user ADD COLUMN IF NOT EXISTS company_id bigint, \
     ADD COLUMN IF NOT EXISTS company_ids text NOT NULL DEFAULT ''";
const ENSURE_REFRESH: &str = "CREATE TABLE IF NOT EXISTS meshble_refresh \
     (jti text PRIMARY KEY, user_id bigint NOT NULL, expires_at timestamptz NOT NULL, \
      revoked boolean NOT NULL DEFAULT false)";

/// A user row used for credential verification.
pub struct UserRow {
    pub id: i64,
    pub password_hash: String,
    pub groups: Vec<String>,
    /// The user's active company (or None = unrestricted).
    pub company_id: Option<i64>,
    /// Companies the user may access (empty = unrestricted).
    pub company_ids: Vec<i64>,
}

fn split_groups(s: &str) -> Vec<String> {
    s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

/// Parses a CSV of company ids, dropping blanks/garbage (the column is server-written, never raw input).
fn split_ids(s: &str) -> Vec<i64> {
    s.split(',').filter_map(|x| x.trim().parse().ok()).collect()
}

impl Db {
    /// Creates the auth tables if absent (idempotent). Call once at startup.
    pub async fn ensure_auth_schema(&self) -> Result<(), DbError> {
        sqlx::query(ENSURE_USER).execute(&self.pool).await?;
        sqlx::query(ENSURE_USER_COMPANY).execute(&self.pool).await?;
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
        let row = sqlx::query(
            "SELECT id, password_hash, groups, company_id, company_ids FROM meshble_user WHERE login = $1",
        )
        .bind(login)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| UserRow {
            id: r.get("id"),
            password_hash: r.get("password_hash"),
            groups: split_groups(&r.get::<String, _>("groups")),
            company_id: r.get("company_id"),
            company_ids: split_ids(&r.get::<String, _>("company_ids")),
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

    /// The user's CURRENT company scope `(active, allowed)` — re-read on refresh so reassignments
    /// take effect without re-login. Both empty/None → unrestricted.
    pub async fn user_scope(&self, uid: i64) -> Result<(Option<i64>, Vec<i64>), DbError> {
        let row = sqlx::query("SELECT company_id, company_ids FROM meshble_user WHERE id = $1")
            .bind(uid)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row
            .map(|r| (r.get::<Option<i64>, _>("company_id"), split_ids(&r.get::<String, _>("company_ids"))))
            .unwrap_or((None, Vec::new())))
    }

    /// Assigns a user's company scope: `active` is the default company, `allowed` the access set.
    /// `active` is folded into `allowed` (a user may always access their active company).
    pub async fn set_user_companies(
        &self,
        login: &str,
        active: Option<i64>,
        allowed: &[i64],
    ) -> Result<(), DbError> {
        let mut ids: Vec<i64> = allowed.to_vec();
        if let Some(a) = active {
            if !ids.contains(&a) {
                ids.push(a);
            }
        }
        let csv = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let n = sqlx::query("UPDATE meshble_user SET company_id = $1, company_ids = $2 WHERE login = $3")
            .bind(active)
            .bind(csv)
            .bind(login)
            .execute(&self.pool)
            .await?
            .rows_affected();
        if n == 0 {
            return Err(DbError::BadInput(format!("no such user: {login}")));
        }
        Ok(())
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
