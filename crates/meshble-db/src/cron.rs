//! Scheduled jobs — the `ir.cron` equivalent. A job is registered at compile time (name, interval,
//! body) and run by the server's background scheduler. Each job's next run is persisted in
//! `meshble_cron`, and the "claim due jobs" step is a single atomic UPDATE with `FOR UPDATE SKIP
//! LOCKED`, so the schedule survives restarts and a job never double-runs across concurrent workers.
//!
//! A job body takes the database and returns a future; it does its own work through the secured/raw
//! API (building a `Ctx::sudo()` as needed). Jobs must be idempotent: the claim advances `next_run`
//! BEFORE running, so a failed job simply retries on its next interval rather than blocking the loop.

use std::future::Future;
use std::pin::Pin;

use crate::{Db, DbError};

/// A scheduled job's body. The returned future borrows `db` for the duration of the run.
pub type CronFn = for<'a> fn(&'a Db) -> Pin<Box<dyn Future<Output = Result<(), DbError>> + Send + 'a>>;

/// Registration of a scheduled job (emitted by hand or a future `register_cron!`).
pub struct CronRegistration {
    pub name: &'static str,
    pub interval_secs: i64,
    pub func: CronFn,
}
meshble_core::inventory::collect!(CronRegistration);

/// All compile-time-registered cron jobs.
pub fn registered_crons() -> Vec<&'static CronRegistration> {
    meshble_core::inventory::iter::<CronRegistration>.into_iter().collect()
}

const ENSURE_CRON: &str = "CREATE TABLE IF NOT EXISTS meshble_cron \
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
                "INSERT INTO meshble_cron (name, interval_secs) VALUES ($1, $2) \
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
            "UPDATE meshble_cron SET next_run = now() + interval_secs * interval '1 second', last_run = now() \
             WHERE name IN (SELECT name FROM meshble_cron WHERE active AND next_run <= now() FOR UPDATE SKIP LOCKED) \
             RETURNING name",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut ran = 0;
        for name in due {
            match registered_crons().into_iter().find(|c| c.name == name) {
                Some(c) => match (c.func)(self).await {
                    Ok(()) => ran += 1,
                    Err(e) => eprintln!("meshble cron '{name}' failed: {e:?}"),
                },
                None => {
                    let _ = sqlx::query("UPDATE meshble_cron SET active = false WHERE name = $1")
                        .bind(&name)
                        .execute(&self.pool)
                        .await;
                }
            }
        }
        Ok(ran)
    }
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
meshble_core::inventory::submit! {
    CronRegistration { name: "gc_done_activities", interval_secs: 86_400, func: gc_done_activities }
}
