//! Webhook fan-out: each domain event materializes one webhook_delivery per matching active
//! subscription, filtered by event type, idempotent (no duplicate deliveries on a re-run). Requires
//! DATABASE_URL.

use kigumi::prelude::*;
use kigumi_db::Db;
use serde_json::json;

fn link() {
    let _ = (&kigumi_mod_sales::MANIFEST, &kigumi_mod_base::MANIFEST, &kigumi_mod_mail::MANIFEST);
}

#[tokio::test]
async fn events_fan_out_to_matching_subscriptions_once() {
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

    // Two subscriptions: one for ALL events, one only for model.created.
    let sub_all = db.create_webhook_subscription("all", "https://a.test/hook", "sec_all", &[], None).await.unwrap();
    let sub_created = db.create_webhook_subscription("created-only", "https://b.test/hook", "sec_created", &["model.created".to_string()], None).await.unwrap();
    assert_ne!(sub_all, sub_created);

    // A create fires model.created -> matches BOTH subscriptions.
    let p = db.insert_secured(&partner, &su, &[], &[], json!({ "name": "ACME" }).as_object().unwrap()).await.unwrap();
    assert_eq!(db.fan_out_events().await.unwrap(), 2, "model.created fans out to both subscriptions");
    assert_eq!(db.deliveries_in_state("pending").await.unwrap(), 2);

    // Fan-out is idempotent: re-running creates nothing (the event is dispatched + the UNIQUE guard).
    assert_eq!(db.fan_out_events().await.unwrap(), 0, "re-running fan-out is a no-op");
    assert_eq!(db.deliveries_in_state("pending").await.unwrap(), 2);

    // An update fires model.updated -> matches ONLY the '*' subscription (not the created-only one).
    db.update_secured(&partner, &su, &[], &[], p, json!({ "name": "ACME Inc" }).as_object().unwrap()).await.unwrap();
    assert_eq!(db.fan_out_events().await.unwrap(), 1, "model.updated reaches only the wildcard subscription");
    assert_eq!(db.deliveries_in_state("pending").await.unwrap(), 3);

    // Deactivating a subscription stops future fan-out to it.
    assert!(db.deactivate_webhook_subscription(sub_all).await.unwrap());
    db.update_secured(&partner, &su, &[], &[], p, json!({ "name": "ACME LLC" }).as_object().unwrap()).await.unwrap();
    assert_eq!(db.fan_out_events().await.unwrap(), 0, "no fan-out to a deactivated subscription (and created-only ignores updates)");

    // The subscription listing never leaks the secret.
    let subs = db.list_webhook_subscriptions().await.unwrap();
    assert_eq!(subs.len(), 2);
    assert!(subs.iter().all(|s| s.get("secret").is_none()), "the secret is never listed");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
