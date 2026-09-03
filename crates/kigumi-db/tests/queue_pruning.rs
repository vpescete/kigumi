//! Retention on the four queue tables, and above all the trap in it: `webhook_delivery.outbox_id`
//! is ON DELETE CASCADE, so pruning `event_outbox` by age alone would silently take PENDING
//! deliveries with it — and a retry legitimately waits days. Requires DATABASE_URL.

use sqlx::Row;

async fn count(db: &kigumi_db::Db, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

/// Inserts an outbox row with an explicit age. Returns its id.
async fn outbox(db: &kigumi_db::Db, model: &str, dispatched: bool, age: &str) -> i64 {
    sqlx::query(
        "INSERT INTO event_outbox (event_type, model, record_id, dispatched, occurred_at) \
         VALUES ('model.created', $1, 1, $2, now() - $3::interval) RETURNING id",
    )
    .bind(model)
    .bind(dispatched)
    .bind(age)
    .fetch_one(db.pool())
    .await
    .unwrap()
    .get::<i64, _>("id")
}

async fn delivery(db: &kigumi_db::Db, sub: i64, outbox_id: i64, state: &str, age: &str) {
    sqlx::query(
        "INSERT INTO webhook_delivery (subscription_id, outbox_id, url, secret, payload, state, created_at) \
         VALUES ($1, $2, 'https://example.com/hook', 's', '{}'::jsonb, $3, now() - $4::interval)",
    )
    .bind(sub)
    .bind(outbox_id)
    .bind(state)
    .bind(age)
    .execute(db.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn pruning_bounds_the_queues_without_ever_cascading_a_pending_delivery() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    db.ensure_event_schema().await.unwrap();
    db.ensure_job_schema().await.unwrap();
    db.ensure_auth_schema().await.unwrap();
    for table in ["webhook_delivery", "event_outbox", "webhook_subscription", "kigumi_job", "kigumi_refresh"] {
        sqlx::query(&format!("DELETE FROM {table}")).execute(db.pool()).await.unwrap();
    }

    let sub: i64 = sqlx::query(
        "INSERT INTO webhook_subscription (name, url, secret) VALUES ('s', 'https://example.com/hook', 'k') RETURNING id",
    )
    .fetch_one(db.pool())
    .await
    .unwrap()
    .get("id");

    // THE trap: dispatched and far past retention, but a delivery is still waiting to be retried.
    let still_owed = outbox(db, "keep.pending", true, "200 days").await;
    delivery(db, sub, still_owed, "pending", "200 days").await;

    // Dispatched, past retention, nothing references it → collectable.
    let collectable = outbox(db, "gone.dispatched", true, "200 days").await;
    // Dispatched but young → kept.
    let young = outbox(db, "keep.young", true, "1 day").await;
    // Undispatched inside the longer window → kept (in an adopter runtime this is the whole log).
    let undispatched_recent = outbox(db, "keep.undispatched", false, "40 days").await;
    // Undispatched past the longer window → collectable.
    let undispatched_ancient = outbox(db, "gone.undispatched", false, "200 days").await;

    // A delivered row ages out; a dead one is the forensic record and is kept far longer. Two
    // outbox rows, because webhook_delivery is UNIQUE (subscription_id, outbox_id).
    let had_sent = outbox(db, "keep.hassent", true, "1 day").await;
    delivery(db, sub, had_sent, "sent", "30 days").await;
    let had_dead = outbox(db, "keep.hasdead", true, "1 day").await;
    delivery(db, sub, had_dead, "dead", "30 days").await;

    sqlx::query("INSERT INTO kigumi_job (name, state, created_at) VALUES ('j', 'done', now() - interval '30 days'), ('j', 'pending', now() - interval '30 days')")
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO kigumi_refresh (jti, user_id, expires_at) VALUES ('old', 1, now() - interval '90 days'), ('live', 1, now() + interval '1 day')")
        .execute(db.pool()).await.unwrap();

    db.prune_queues().await.unwrap();

    let survives = |id: i64| async move {
        count(db, &format!("SELECT count(*) FROM event_outbox WHERE id = {id}")).await == 1
    };
    assert!(survives(still_owed).await, "an outbox row with a PENDING delivery must never be cascaded away");
    assert_eq!(
        count(db, &format!("SELECT count(*) FROM webhook_delivery WHERE outbox_id = {still_owed}")).await,
        1,
        "and its pending delivery is still there to be retried"
    );
    assert!(!survives(collectable).await, "dispatched, past retention, unreferenced → pruned");
    assert!(survives(young).await, "inside the window → kept");
    assert!(survives(undispatched_recent).await, "undispatched inside the longer window → kept");
    assert!(!survives(undispatched_ancient).await, "undispatched past the longer window → pruned");

    assert_eq!(
        count(db, &format!("SELECT count(*) FROM webhook_delivery WHERE outbox_id = {had_sent}")).await,
        0,
        "a sent delivery ages out"
    );
    assert_eq!(
        count(db, &format!("SELECT count(*) FROM webhook_delivery WHERE outbox_id = {had_dead} AND state = 'dead'")).await,
        1,
        "a dead delivery is kept as the forensic record"
    );

    assert_eq!(count(db, "SELECT count(*) FROM kigumi_job WHERE state = 'done'").await, 0, "done jobs pruned");
    assert_eq!(count(db, "SELECT count(*) FROM kigumi_job WHERE state = 'pending'").await, 1, "pending jobs untouched");
    assert_eq!(count(db, "SELECT count(*) FROM kigumi_refresh WHERE jti = 'old'").await, 0, "long-expired token pruned");
    assert_eq!(count(db, "SELECT count(*) FROM kigumi_refresh WHERE jti = 'live'").await, 1, "live token untouched");
}
