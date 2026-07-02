//! The background-job queue: enqueue → claim (SKIP LOCKED, only names registered in THIS binary,
//! no double-run) → run (panic-safe); failure retries with exponential backoff and dead-letters at
//! the registration's max_attempts; the in-tx enqueue rolls back with the caller's transaction (a
//! job exists iff the business write committed); an expired lease is reaped without burning an
//! attempt; an unregistered name fails fast at enqueue, and a row for a job kind this binary does
//! not register stays claimable for a capable worker. Requires DATABASE_URL.

use kigumi_db::{Db, DbError, JobRegistration};
use serde_json::{json, Value as Json};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};

type Fut<'a> = Pin<Box<dyn Future<Output = Result<(), DbError>> + Send + 'a>>;

static PROBE_RUNS: AtomicI64 = AtomicI64::new(0);
static FLAKY_RUNS: AtomicI64 = AtomicI64::new(0);

/// Succeeds, counting executions and recording its payload in a probe table.
fn probe_job(db: &Db, payload: Json) -> Fut<'_> {
    Box::pin(async move {
        PROBE_RUNS.fetch_add(1, Ordering::SeqCst);
        sqlx::query("INSERT INTO job_probe (note) VALUES ($1)")
            .bind(payload["note"].as_str().unwrap_or("").to_string())
            .execute(db.pool())
            .await?;
        Ok(())
    })
}
kigumi_core::inventory::submit! { JobRegistration { name: "probe", max_attempts: 8, func: probe_job } }

/// Fails on its first execution, succeeds on the second — exercises the retry path.
fn flaky_job(_db: &Db, _payload: Json) -> Fut<'_> {
    Box::pin(async move {
        if FLAKY_RUNS.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(DbError::BadInput("transient failure".to_string()));
        }
        Ok(())
    })
}
kigumi_core::inventory::submit! { JobRegistration { name: "flaky", max_attempts: 8, func: flaky_job } }

/// Always fails, with a tight attempt budget — exercises the dead-letter path.
fn doomed_job(_db: &Db, _payload: Json) -> Fut<'_> {
    Box::pin(async move { Err(DbError::BadInput("always fails".to_string())) })
}
kigumi_core::inventory::submit! { JobRegistration { name: "doomed", max_attempts: 2, func: doomed_job } }

/// PANICS — a module author's stray unwrap must be a job failure, never a dead runner task.
fn panicky_job(_db: &Db, _payload: Json) -> Fut<'_> {
    Box::pin(async move { panic!("stray unwrap in a module job") })
}
kigumi_core::inventory::submit! { JobRegistration { name: "panicky", max_attempts: 1, func: panicky_job } }

async fn job_row(db: &Db, id: i64) -> (String, i32, Option<String>) {
    use sqlx::Row;
    let r = sqlx::query("SELECT state, attempts, last_error FROM kigumi_job WHERE id = $1")
        .bind(id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    (r.get(0), r.get(1), r.get(2))
}

async fn force_due(db: &Db, id: i64) {
    sqlx::query("UPDATE kigumi_job SET next_attempt_at = now() WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn jobs_run_retry_deadletter_and_enqueue_transactionally() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    db.ensure_job_schema().await.unwrap();
    sqlx::query("CREATE TABLE IF NOT EXISTS job_probe (id BIGSERIAL PRIMARY KEY, note TEXT)")
        .execute(db.pool())
        .await
        .unwrap();
    PROBE_RUNS.store(0, Ordering::SeqCst);
    FLAKY_RUNS.store(0, Ordering::SeqCst);

    // Unregistered name fails fast at enqueue (a typo is caught here, not as a dead letter later).
    assert!(db.enqueue_job("tpyo", json!({})).await.is_err(), "unknown job name rejected at enqueue");

    // Happy path: enqueue → run → done, exactly once, side effect recorded.
    let id = db.enqueue_job("probe", json!({ "note": "hello" })).await.unwrap();
    let ran = db.run_due_jobs().await.unwrap();
    assert_eq!(ran, 1);
    assert_eq!(PROBE_RUNS.load(Ordering::SeqCst), 1);
    let (state, _, _) = job_row(db, id).await;
    assert_eq!(state, "done");
    let notes: i64 = sqlx::query_scalar("SELECT count(*) FROM job_probe WHERE note = 'hello'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(notes, 1, "the job's side effect landed");

    // A second tick with nothing due runs nothing (done rows are never re-claimed).
    assert_eq!(db.run_due_jobs().await.unwrap(), 0);
    assert_eq!(PROBE_RUNS.load(Ordering::SeqCst), 1, "no double-run of a done job");

    // Retry: first execution fails → pending with attempts=1, a recorded error and a FUTURE
    // next_attempt_at (backoff); forcing it due lets the second execution succeed.
    let fid = db.enqueue_job("flaky", json!({})).await.unwrap();
    db.run_due_jobs().await.unwrap();
    let (state, attempts, err) = job_row(db, fid).await;
    assert_eq!((state.as_str(), attempts), ("pending", 1));
    assert!(err.unwrap().contains("transient failure"));
    let due: bool = sqlx::query_scalar("SELECT next_attempt_at > now() FROM kigumi_job WHERE id = $1")
        .bind(fid)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(due, "backoff pushed the retry into the future");
    assert_eq!(db.run_due_jobs().await.unwrap(), 0, "not due yet — backoff respected");
    force_due(db, fid).await;
    assert_eq!(db.run_due_jobs().await.unwrap(), 1);
    let (state, _, _) = job_row(db, fid).await;
    assert_eq!(state, "done", "the retry succeeded");

    // Dead letter: always-fails with max_attempts=2 → two executions then dead, error preserved.
    let did = db.enqueue_job("doomed", json!({})).await.unwrap();
    db.run_due_jobs().await.unwrap();
    force_due(db, did).await;
    db.run_due_jobs().await.unwrap();
    let (state, attempts, err) = job_row(db, did).await;
    assert_eq!((state.as_str(), attempts), ("dead", 2));
    assert!(err.unwrap().contains("always fails"));

    // Transactional enqueue THROUGH THE API: rolled back with the tx → no row; committed →
    // visible and runnable (the exactly-with-the-business-write guarantee).
    let mut tx = db.pool().begin().await.unwrap();
    db.enqueue_job_in_tx(&mut tx, "probe", json!({ "note": "never" })).await.unwrap();
    drop(tx); // rollback
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM kigumi_job WHERE state = 'pending'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(pending, 0, "a rolled-back enqueue leaves no job");
    let mut tx = db.pool().begin().await.unwrap();
    let cid = db.enqueue_job_in_tx(&mut tx, "probe", json!({ "note": "committed" })).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(db.run_due_jobs().await.unwrap(), 1, "a committed in-tx enqueue is runnable");
    let (state, _, _) = job_row(db, cid).await;
    assert_eq!(state, "done");

    // Reap: a claimed job whose lease expired goes back to pending WITHOUT burning an attempt, and
    // the stale claimant's late outcome is discarded by the state guard.
    let rid = db.enqueue_job("probe", json!({ "note": "reaped" })).await.unwrap();
    sqlx::query("UPDATE kigumi_job SET state = 'running', lease_until = now() - interval '1 minute' WHERE id = $1")
        .bind(rid)
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(db.reap_stuck_jobs().await.unwrap(), 1);
    let (state, attempts, _) = job_row(db, rid).await;
    assert_eq!((state.as_str(), attempts), ("pending", 0), "reap requeues without burning an attempt");
    assert_eq!(db.run_due_jobs().await.unwrap(), 1, "the reaped job re-runs");

    // A PANICKING body is recorded as a failure (here max_attempts=1 → dead) — and the runner
    // survives to run the next job.
    let pid = db.enqueue_job("panicky", json!({})).await.unwrap();
    db.run_due_jobs().await.unwrap();
    let (state, _, err) = job_row(db, pid).await;
    assert_eq!(state, "dead");
    assert!(err.unwrap().contains("panicked"), "the panic is recorded as the error");
    let nid = db.enqueue_job("probe", json!({ "note": "after-panic" })).await.unwrap();
    assert_eq!(db.run_due_jobs().await.unwrap(), 1, "the runner survives a panicking body");
    let (state, _, _) = job_row(db, nid).await;
    assert_eq!(state, "done");

    // Concurrency: one pending job, two workers racing — SKIP LOCKED gives it to exactly one.
    PROBE_RUNS.store(0, Ordering::SeqCst);
    db.enqueue_job("probe", json!({ "note": "race" })).await.unwrap();
    let (a, b) = tokio::join!(db.run_due_jobs(), db.run_due_jobs());
    assert_eq!(a.unwrap() + b.unwrap(), 1, "exactly one worker claimed the job");
    assert_eq!(PROBE_RUNS.load(Ordering::SeqCst), 1, "the body ran exactly once");

    // A row for a job kind this binary does NOT register is never claimed (a capable worker in a
    // mixed fleet — rolling deploy, optional module — must be able to take it later).
    let sid: i64 = sqlx::query_scalar("INSERT INTO kigumi_job (name) VALUES ('foreign_job') RETURNING id")
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.run_due_jobs().await.unwrap();
    let (state, attempts, _) = job_row(db, sid).await;
    assert_eq!((state.as_str(), attempts), ("pending", 0), "foreign jobs stay claimable, untouched");
}
