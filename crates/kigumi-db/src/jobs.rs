//! Ad-hoc background jobs on Postgres — the "run X now, asynchronously, with retries" counterpart
//! to the recurring cron subsystem. No broker: the queue is a table driven by the same
//! claim-and-lease SKIP LOCKED shape as webhook delivery (event_schema.rs), with the same
//! exponential backoff and dead-letter ledger. One deliberate strength over external queues:
//! [`ServiceCtx::enqueue_job`](crate::ServiceCtx::enqueue_job) enqueues ON the service transaction,
//! so a job exists if and only if the business write that scheduled it committed.
//!
//! Bodies MUST be idempotent: a worker crash after the work but before the `done` update re-runs
//! the job when its lease expires (at-least-once, exactly like cron bodies and webhook delivery).

use crate::{Db, DbError};
use serde_json::Value as Json;
use std::future::Future;
use std::pin::Pin;

/// A registered job body: owns its payload, reaches the DB through the full handle (same trust
/// class as a cron body). Return `Err` to schedule a retry (or dead-letter past `max_attempts`).
pub type JobFn = for<'a> fn(&'a Db, Json) -> Pin<Box<dyn Future<Output = Result<(), DbError>> + Send + 'a>>;

/// Registration of a job kind, emitted by `register_job!`. `max_attempts` counts executions, not
/// retries: 1 = never retry; the webhook-parity default in the macro is 8.
pub struct JobRegistration {
    pub name: &'static str,
    pub max_attempts: i32,
    pub func: JobFn,
}
kigumi_core::inventory::collect!(JobRegistration);

/// Looks up a registered job kind.
pub fn job_for(name: &str) -> Option<&'static JobRegistration> {
    kigumi_core::inventory::iter::<JobRegistration>.into_iter().find(|j| j.name == name)
}

/// Retry shape — identical to webhook delivery: 30s · 2^attempts, capped at 6 hours.
const JOB_BACKOFF_BASE_SECS: i64 = 30;
const JOB_BACKOFF_CAP_SECS: i64 = 21_600;
/// How long a claimed job may run before a reaper re-queues it (crashed worker recovery).
const JOB_LEASE_MINUTES: i32 = 5;
/// Claim batch size per tick.
const JOB_BATCH: i64 = 50;

const ENSURE_JOB: &str = "CREATE TABLE IF NOT EXISTS kigumi_job (\
    id BIGSERIAL PRIMARY KEY, \
    name TEXT NOT NULL, \
    payload JSONB NOT NULL DEFAULT '{}', \
    state TEXT NOT NULL DEFAULT 'pending', \
    attempts INTEGER NOT NULL DEFAULT 0, \
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
    lease_until TIMESTAMPTZ, \
    last_error TEXT, \
    created_at TIMESTAMPTZ NOT NULL DEFAULT now())";

impl Db {
    /// Creates the job table + due-index (idempotent) and validates the registry (duplicate names
    /// are an authoring bug). Run during migrate/serve, like the other ensure_* schemas.
    pub async fn ensure_job_schema(&self) -> Result<(), DbError> {
        let mut seen = std::collections::BTreeSet::new();
        for j in kigumi_core::inventory::iter::<JobRegistration> {
            if !seen.insert(j.name) {
                return Err(DbError::Migration(format!("duplicate job registration: '{}'", j.name)));
            }
        }
        sqlx::query(ENSURE_JOB).execute(&self.pool).await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS kigumi_job_due ON kigumi_job (next_attempt_at) WHERE state = 'pending'")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Enqueues a job (own transaction — visible to the runner immediately). The name must be
    /// registered: a typo fails HERE, loudly, not as a dead-letter row hours later.
    pub async fn enqueue_job(&self, name: &str, payload: Json) -> Result<i64, DbError> {
        if job_for(name).is_none() {
            return Err(DbError::BadInput(format!("unknown job '{name}' (register_job! it first)")));
        }
        let id: i64 = sqlx::query_scalar("INSERT INTO kigumi_job (name, payload) VALUES ($1, $2) RETURNING id")
            .bind(name)
            .bind(&payload)
            .fetch_one(&self.pool)
            .await?;
        Ok(id)
    }

    /// The in-tx twin: the job row commits (or rolls back) WITH the caller's transaction — used by
    /// `ServiceCtx::enqueue_job` so a service schedules follow-on work atomically with its writes.
    pub(crate) async fn enqueue_job_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        name: &str,
        payload: Json,
    ) -> Result<i64, DbError> {
        if job_for(name).is_none() {
            return Err(DbError::BadInput(format!("unknown job '{name}' (register_job! it first)")));
        }
        let id: i64 = sqlx::query_scalar("INSERT INTO kigumi_job (name, payload) VALUES ($1, $2) RETURNING id")
            .bind(name)
            .bind(&payload)
            .fetch_one(&mut **tx)
            .await?;
        Ok(id)
    }

    /// Claims a batch of due jobs (atomically: `pending` → `running` + lease, `FOR UPDATE SKIP
    /// LOCKED` so concurrent workers never double-claim) and runs each. Success → `done`; failure →
    /// exponential backoff, or `dead` at the registration's `max_attempts`; an UNREGISTERED name
    /// (a stale row from a removed job) → `dead` with a clear error. Returns how many ran.
    pub async fn run_due_jobs(&self) -> Result<u64, DbError> {
        let claimed = sqlx::query(
            "UPDATE kigumi_job SET state = 'running', lease_until = now() + make_interval(mins => $1) \
             WHERE id IN (\
                 SELECT id FROM kigumi_job \
                 WHERE state = 'pending' AND next_attempt_at <= now() \
                 ORDER BY next_attempt_at LIMIT $2 \
                 FOR UPDATE SKIP LOCKED) \
             RETURNING id, name, payload, attempts",
        )
        .bind(JOB_LEASE_MINUTES)
        .bind(JOB_BATCH)
        .fetch_all(&self.pool)
        .await?;

        let mut ran = 0u64;
        for row in &claimed {
            use sqlx::Row;
            let id: i64 = row.get("id");
            let name: String = row.get("name");
            let payload: Json = row.get("payload");
            let attempts: i32 = row.get("attempts");

            let Some(reg) = job_for(&name) else {
                sqlx::query("UPDATE kigumi_job SET state = 'dead', last_error = $2 WHERE id = $1")
                    .bind(id)
                    .bind(format!("no registered job named '{name}' in this binary"))
                    .execute(&self.pool)
                    .await?;
                continue;
            };
            match (reg.func)(self, payload).await {
                Ok(()) => {
                    sqlx::query("UPDATE kigumi_job SET state = 'done', last_error = NULL WHERE id = $1")
                        .bind(id)
                        .execute(&self.pool)
                        .await?;
                    ran += 1;
                }
                Err(e) => {
                    let next_attempts = attempts + 1;
                    if next_attempts >= reg.max_attempts {
                        sqlx::query("UPDATE kigumi_job SET state = 'dead', attempts = $2, last_error = $3 WHERE id = $1")
                            .bind(id)
                            .bind(next_attempts)
                            .bind(format!("{e}"))
                            .execute(&self.pool)
                            .await?;
                    } else {
                        let backoff = (JOB_BACKOFF_BASE_SECS << next_attempts.min(30)).min(JOB_BACKOFF_CAP_SECS);
                        sqlx::query(
                            "UPDATE kigumi_job SET state = 'pending', attempts = $2, last_error = $3, \
                             next_attempt_at = now() + make_interval(secs => $4) WHERE id = $1",
                        )
                        .bind(id)
                        .bind(next_attempts)
                        .bind(format!("{e}"))
                        .bind(backoff as f64)
                        .execute(&self.pool)
                        .await?;
                    }
                }
            }
        }
        Ok(ran)
    }

    /// Re-queues `running` jobs whose lease expired (a crashed worker never recorded an outcome).
    /// The attempt was never counted, so the re-run doesn't burn one — bodies must be idempotent.
    pub async fn reap_stuck_jobs(&self) -> Result<u64, DbError> {
        let n = sqlx::query("UPDATE kigumi_job SET state = 'pending', lease_until = NULL WHERE state = 'running' AND lease_until < now()")
            .execute(&self.pool)
            .await?;
        Ok(n.rows_affected())
    }
}
