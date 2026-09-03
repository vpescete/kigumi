//! Scheduled jobs — the `ir.cron` equivalent. A job is registered at compile time (name, interval,
//! body) and run by the server's background scheduler. Each job's next run is persisted in
//! `kigumi_cron`, and the "claim due jobs" step is a single atomic UPDATE with `FOR UPDATE SKIP
//! LOCKED`, so the schedule survives restarts and a job never double-runs across concurrent workers.
//!
//! A job body takes the database and returns a future; it does its own work through the secured/raw
//! API (building a `Ctx::sudo()` as needed). Jobs must be idempotent: the claim advances `next_run`
//! BEFORE running, so a failed job simply retries on its next interval rather than blocking the loop.

use std::future::Future;
use std::pin::Pin;

use kigumi_core::{resolve_registered, transient_models};

use crate::{Db, DbError};

/// Postgres SQLSTATEs we tolerate when sweeping transient tables: the table or the `create_date`
/// column may not exist yet (the wizard's module isn't migrated) — treat as nothing to sweep.
fn is_missing_table_or_column(e: &sqlx::Error) -> bool {
    matches!(
        e.as_database_error().and_then(|d| d.code()).as_deref(),
        Some("42P01") | Some("42703")
    )
}

/// A scheduled job's body. The returned future borrows `db` for the duration of the run.
pub type CronFn = for<'a> fn(&'a Db) -> Pin<Box<dyn Future<Output = Result<(), DbError>> + Send + 'a>>;

/// Registration of a scheduled job (emitted by hand or a future `register_cron!`).
pub struct CronRegistration {
    pub name: &'static str,
    pub interval_secs: i64,
    pub func: CronFn,
}
kigumi_core::inventory::collect!(CronRegistration);

/// All compile-time-registered cron jobs.
pub fn registered_crons() -> Vec<&'static CronRegistration> {
    kigumi_core::inventory::iter::<CronRegistration>.into_iter().collect()
}

const ENSURE_CRON: &str = "CREATE TABLE IF NOT EXISTS kigumi_cron \
     (name text PRIMARY KEY, interval_secs bigint NOT NULL, \
      next_run timestamptz NOT NULL DEFAULT now(), last_run timestamptz, \
      active boolean NOT NULL DEFAULT true)";

impl Db {
    /// Creates the cron table and upserts every registered job (idempotent). An existing job keeps
    /// its `next_run`/`active`; only its interval is refreshed. Run during migrate.
    pub async fn ensure_crons(&self) -> Result<(), DbError> {
        sqlx::query(ENSURE_CRON).execute(&self.pool).await?;
        for c in registered_crons() {
            sqlx::query(
                "INSERT INTO kigumi_cron (name, interval_secs) VALUES ($1, $2) \
                 ON CONFLICT (name) DO UPDATE SET interval_secs = EXCLUDED.interval_secs",
            )
            .bind(c.name)
            .bind(c.interval_secs)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// Claims every due job atomically — advancing its `next_run`, with `SKIP LOCKED` so a second
    /// scheduler can't claim the same row — then runs each body. A failure is logged (the job retries
    /// next interval). A persisted job with no registered body is deactivated. Returns how many ran.
    pub async fn run_due_crons(&self) -> Result<usize, DbError> {
        let due: Vec<String> = sqlx::query_scalar(
            "UPDATE kigumi_cron SET next_run = now() + interval_secs * interval '1 second', last_run = now() \
             WHERE name IN (SELECT name FROM kigumi_cron WHERE active AND next_run <= now() FOR UPDATE SKIP LOCKED) \
             RETURNING name",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut ran = 0;
        for name in due {
            match registered_crons().into_iter().find(|c| c.name == name) {
                Some(c) => match (c.func)(self).await {
                    Ok(()) => ran += 1,
                    Err(e) => eprintln!("kigumi cron '{name}' failed: {e:?}"),
                },
                None => {
                    let _ = sqlx::query("UPDATE kigumi_cron SET active = false WHERE name = $1")
                        .bind(&name)
                        .execute(&self.pool)
                        .await;
                }
            }
        }
        Ok(ran)
    }
}

/// Retention windows for the queue tables. Constants, not config: every one is a "how much history
/// do you keep" question with a defensible default, and no operator has asked for different numbers.
/// ponytail: fixed windows; a `[retention]` config section if one ever does.
const RETAIN_DELIVERY_SENT: &str = "7 days";
/// A `dead` delivery is the forensic record of an endpoint that never accepted the payload — the row
/// someone reads months later to explain a missing integration. Kept far longer than a success.
const RETAIN_DELIVERY_DEAD: &str = "90 days";
const RETAIN_OUTBOX_DISPATCHED: &str = "30 days";
/// Undispatched rows are kept LONGER because in an adopter runtime they are the only record there
/// is: the webhook fan-out task is spawned by the CLI binary alone, so `kigumi_runtime::serve`
/// never marks anything dispatched and every row lands in this bucket.
const RETAIN_OUTBOX_UNDISPATCHED: &str = "90 days";
const RETAIN_JOB_DONE: &str = "7 days";
const RETAIN_JOB_DEAD: &str = "90 days";
const RETAIN_REFRESH: &str = "30 days";

impl Db {
    /// Bounds the four queue tables. Public so it is testable without the cron ledger; the
    /// `gc_queues` job is a thin wrapper.
    ///
    /// ORDER IS LOAD-BEARING. `webhook_delivery.outbox_id` is `ON DELETE CASCADE`, so deleting an
    /// outbox row takes its deliveries with it — including `pending` ones, which legitimately wait
    /// days between retries. Deliveries are therefore pruned FIRST, on their own state and age, and
    /// an outbox row leaves only once nothing references it any more.
    ///
    /// Consequence to know: `event_outbox` is also what the SSE stream reads, so a `Last-Event-ID`
    /// older than the retention window resumes from the oldest surviving row instead of replaying
    /// what was pruned. That is inside the events-are-hints contract, and it is documented in the
    /// API guide rather than left to be discovered.
    pub async fn prune_queues(&self) -> Result<(), DbError> {
        let statements = [
            format!("DELETE FROM webhook_delivery WHERE state = 'sent' AND created_at < now() - interval '{RETAIN_DELIVERY_SENT}'"),
            format!("DELETE FROM webhook_delivery WHERE state = 'dead' AND created_at < now() - interval '{RETAIN_DELIVERY_DEAD}'"),
            // NOT EXISTS, not a join: an outbox row with ANY surviving delivery — pending, leased,
            // or simply young enough to be kept above — must outlive this sweep.
            format!(
                "DELETE FROM event_outbox e WHERE e.dispatched AND e.occurred_at < now() - interval '{RETAIN_OUTBOX_DISPATCHED}'                  AND NOT EXISTS (SELECT 1 FROM webhook_delivery d WHERE d.outbox_id = e.id)"
            ),
            format!(
                "DELETE FROM event_outbox e WHERE NOT e.dispatched AND e.occurred_at < now() - interval '{RETAIN_OUTBOX_UNDISPATCHED}'                  AND NOT EXISTS (SELECT 1 FROM webhook_delivery d WHERE d.outbox_id = e.id)"
            ),
            format!("DELETE FROM kigumi_job WHERE state = 'done' AND created_at < now() - interval '{RETAIN_JOB_DONE}'"),
            format!("DELETE FROM kigumi_job WHERE state = 'dead' AND created_at < now() - interval '{RETAIN_JOB_DEAD}'"),
            // Safe by construction: `refresh_user` requires the row to be PRESENT, so a pruned token
            // is a rejected token. Revoked-but-unexpired rows are left to die of old age.
            format!("DELETE FROM kigumi_refresh WHERE expires_at < now() - interval '{RETAIN_REFRESH}'"),
        ];
        for sql in &statements {
            match sqlx::query(sql).execute(&self.pool).await {
                Ok(_) => {}
                // The integration/job/auth schema may not be migrated on this instance.
                Err(e) if is_missing_table_or_column(&e) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

/// Builtin job: bound the queue tables (outbox, webhook deliveries, jobs, refresh tokens). Daily.
fn gc_queues(db: &Db) -> Pin<Box<dyn Future<Output = Result<(), DbError>> + Send + '_>> {
    Box::pin(async move { db.prune_queues().await })
}
kigumi_core::inventory::submit! {
    CronRegistration { name: "gc_queues", interval_secs: 86_400, func: gc_queues }
}

/// Builtin job: prune done (`active = false`) mail activities whose deadline is long past, bounding
/// the table. Tolerates an unmigrated `mail_activity` (no-op). Runs daily.
fn gc_done_activities(db: &Db) -> Pin<Box<dyn Future<Output = Result<(), DbError>> + Send + '_>> {
    Box::pin(async move {
        match sqlx::query(
            "DELETE FROM mail_activity WHERE active = false AND date_deadline < now() - interval '30 days'",
        )
        .execute(db.pool())
        .await
        {
            Ok(_) => Ok(()),
            // 42P01 = undefined_table: the mail module isn't migrated → nothing to prune.
            Err(e) if e.as_database_error().and_then(|d| d.code()).as_deref() == Some("42P01") => Ok(()),
            Err(e) => Err(e.into()),
        }
    })
}
kigumi_core::inventory::submit! {
    CronRegistration { name: "gc_done_activities", interval_secs: 86_400, func: gc_done_activities }
}

impl Db {
    /// Gives every transient model's `create_date` a `DEFAULT now()` (idempotent; `to_ddl` emits no
    /// column default). Postgres then stamps `create_date` on EVERY insert path, so the GC cron can
    /// reclaim rows by age regardless of how they were created. Tolerates an unmigrated table or a
    /// model lacking `create_date`. Run during migrate, after the tables exist.
    pub async fn ensure_transient_defaults(&self) -> Result<(), DbError> {
        for model in transient_models() {
            let Ok(m) = resolve_registered(model) else { continue };
            // The table name is the compile-time descriptor's, never user input — safe to format.
            let sql = format!("ALTER TABLE {} ALTER COLUMN create_date SET DEFAULT now()", m.table);
            match sqlx::query(&sql).execute(&self.pool).await {
                Ok(_) => {}
                Err(e) if is_missing_table_or_column(&e) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Reclaims ephemeral transient (wizard) rows older than the TTL, bounding their tables. Public so
    /// it is unit-testable without the cron ledger; the `gc_transient_records` job is a thin wrapper.
    /// ponytail: global 1-hour TTL for all transient models; a per-model knob can come if a wizard
    /// ever needs a longer-lived scratchpad. Tolerates unmigrated transient tables (no-op).
    pub async fn sweep_transient_records(&self) -> Result<(), DbError> {
        for model in transient_models() {
            let Ok(m) = resolve_registered(model) else { continue };
            let sql = format!("DELETE FROM {} WHERE create_date < now() - interval '1 hour'", m.table);
            match sqlx::query(&sql).execute(&self.pool).await {
                Ok(_) => {}
                Err(e) if is_missing_table_or_column(&e) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

/// Builtin job: hourly sweep of aged transient (wizard) rows. Thin wrapper over `sweep_transient_records`.
fn gc_transient_records(db: &Db) -> Pin<Box<dyn Future<Output = Result<(), DbError>> + Send + '_>> {
    Box::pin(async move { db.sweep_transient_records().await })
}
kigumi_core::inventory::submit! {
    CronRegistration { name: "gc_transient_records", interval_secs: 3_600, func: gc_transient_records }
}
