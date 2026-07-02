//! Webhook delivery state machine: a pending delivery is sent on a 2xx (state -> sent), retried with
//! backoff on failure (attempts++, requeued, not immediately due), and dead-lettered after the attempt
//! cap so a permanently-broken endpoint never retries forever. The HTTP transport is stubbed by a closure
//! (the real one lives in the CLI). Requires DATABASE_URL.

use kigumi::prelude::*;
use kigumi_db::{Db, WebhookDelivery};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn link() {
    let _ = (&kigumi_mod_sales::MANIFEST, &kigumi_mod_base::MANIFEST, &kigumi_mod_mail::MANIFEST);
}

#[tokio::test]
async fn delivery_sends_retries_and_dead_letters() {
    link();
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let db = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();

    let plan = migration_plan().unwrap();
    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
    for t in &plan { db.create_table(&t.model).await.unwrap(); }
    for t in &plan { db.create_m2m_relations(&t.model).await.unwrap(); }
    db.ensure_event_schema().await.unwrap();
    db.clear_event_outbox().await.unwrap();
    db.clear_webhook_subscriptions().await.unwrap();

    let partner = resolve_registered("res.partner").unwrap();
    db.create_webhook_subscription("hook", "https://x.test/hook", "sec", &[], None).await.unwrap();

    // SUCCESS: one event -> one delivery, a 2xx marks it sent (transport called exactly once).
    db.insert_secured(&partner, &su, &[], &[], json!({ "name": "ACME" }).as_object().unwrap()).await.unwrap();
    assert_eq!(db.fan_out_events().await.unwrap(), 1);
    let ok_calls = Arc::new(AtomicUsize::new(0));
    let ok_calls2 = ok_calls.clone();
    let ok_send = move |d: &WebhookDelivery| -> Result<(), String> {
        ok_calls2.fetch_add(1, Ordering::SeqCst);
        assert_eq!(d.payload["type"], "model.created");
        assert!(d.payload["id"].as_str().unwrap().starts_with("evt_"));
        Ok(())
    };
    assert_eq!(db.flush_webhooks(&ok_send).await.unwrap(), 1, "a 2xx delivers");
    assert_eq!(ok_calls.load(Ordering::SeqCst), 1, "the transport ran once");
    assert_eq!(db.deliveries_in_state("sent").await.unwrap(), 1);
    assert_eq!(db.deliveries_in_state("pending").await.unwrap(), 0);

    // FAILURE: a fresh event whose endpoint always fails. The first flush retries (not dead yet), and the
    // backoff pushes next_attempt into the future, so an immediate re-flush delivers nothing.
    db.insert_secured(&partner, &su, &[], &[], json!({ "name": "BETA" }).as_object().unwrap()).await.unwrap();
    assert_eq!(db.fan_out_events().await.unwrap(), 1);
    let fail_calls = Arc::new(AtomicUsize::new(0));
    let fail_calls2 = fail_calls.clone();
    let fail_send = move |_: &WebhookDelivery| -> Result<(), String> {
        fail_calls2.fetch_add(1, Ordering::SeqCst);
        Err("boom".into())
    };

    db.flush_webhooks(&fail_send).await.unwrap();
    assert_eq!(db.deliveries_in_state("pending").await.unwrap(), 1, "a failed delivery is requeued, not lost");
    assert_eq!(db.deliveries_in_state("dead").await.unwrap(), 0);
    assert_eq!(db.flush_webhooks(&fail_send).await.unwrap(), 0, "the backoff defers the immediate re-flush");
    assert_eq!(fail_calls.load(Ordering::SeqCst), 1, "no extra send while backing off");

    // Drive the retry loop to the cap (forcing due each round) -> dead-lettered, and no further sends.
    for _ in 0..20 {
        if db.deliveries_in_state("dead").await.unwrap() == 1 { break; }
        db.force_deliveries_due().await.unwrap();
        db.flush_webhooks(&fail_send).await.unwrap();
    }
    assert_eq!(db.deliveries_in_state("dead").await.unwrap(), 1, "a permanently-failing endpoint is dead-lettered");
    assert_eq!(db.deliveries_in_state("pending").await.unwrap(), 0);
    // 8 attempts total (WEBHOOK_MAX_ATTEMPTS): 1 before the loop + the loop rounds up to the cap.
    assert_eq!(fail_calls.load(Ordering::SeqCst), 8, "retries stop at the attempt cap");

    // A dead delivery stays dead — forcing-due does not resurrect it.
    db.force_deliveries_due().await.unwrap();
    assert_eq!(db.flush_webhooks(&fail_send).await.unwrap(), 0);
    assert_eq!(db.deliveries_in_state("dead").await.unwrap(), 1);

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
