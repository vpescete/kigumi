//! Scheduler: a due cron job runs exactly once, then its next_run is advanced by its interval so an
//! immediate re-tick does not run it again (no double-run). Registers a probe job whose body inserts
//! a row, so the run is observable. Live Postgres.

use std::future::Future;
use std::pin::Pin;

use meshble_db::{CronRegistration, Db, DbError};

/// Probe job body: inserts one row into `cron_probe` (created by the test) so a run is observable.
fn probe(db: &Db) -> Pin<Box<dyn Future<Output = Result<(), DbError>> + Send + '_>> {
    Box::pin(async move {
        sqlx::query("INSERT INTO cron_probe DEFAULT VALUES").execute(db.pool()).await?;
        Ok(())
    })
}
meshble_core::inventory::submit! {
    CronRegistration { name: "test_probe", interval_secs: 3600, func: probe }
}

#[tokio::test]
async fn due_job_runs_once_then_waits_its_interval() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    sqlx::query("DROP TABLE IF EXISTS cron_probe").execute(db.pool()).await.unwrap();
    sqlx::query("CREATE TABLE cron_probe (id bigserial PRIMARY KEY, at timestamptz DEFAULT now())")
        .execute(db.pool()).await.unwrap();

    db.ensure_crons().await.unwrap(); // creates meshble_cron + seeds test_probe (and the builtins)
    // Make ONLY test_probe due (push everything else to the future) so the test is deterministic.
    sqlx::query("UPDATE meshble_cron SET next_run = now() + interval '1 day'").execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE meshble_cron SET next_run = now() - interval '1 hour' WHERE name = 'test_probe'")
        .execute(db.pool()).await.unwrap();

    // The due job runs once.
    let ran = db.run_due_crons().await.unwrap();
    assert!(ran >= 1, "the due job ran");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM cron_probe").fetch_one(db.pool()).await.unwrap();
    assert_eq!(n, 1, "ran exactly once");
    // Its next_run was advanced by its interval (~1h ahead), so it is no longer due.
    let due_now: i64 = sqlx::query_scalar("SELECT count(*) FROM meshble_cron WHERE name='test_probe' AND next_run <= now()")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(due_now, 0, "next_run advanced past now");

    // An immediate second tick does NOT re-run it (no double-run within the interval).
    db.run_due_crons().await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM cron_probe").fetch_one(db.pool()).await.unwrap();
    assert_eq!(n, 1, "not re-run within its interval");

    sqlx::query("DROP TABLE cron_probe").execute(db.pool()).await.unwrap();
    sqlx::query("DELETE FROM meshble_cron WHERE name='test_probe'").execute(db.pool()).await.unwrap();
}
