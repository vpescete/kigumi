//! Domain events captured at the CRUD seams: model.created / model.updated / model.state_changed (real
//! transitions only) / model.deleted, plus transactional atomicity — a rolled-back mutation enqueues no
//! event. The event_outbox is the integration foundation (webhooks fan out from it). Requires DATABASE_URL.

use kigumi::prelude::*;
use serde_json::json;

fn link() {
    let _ = (
        &kigumi_mod_sales::MANIFEST,
        &kigumi_mod_base::MANIFEST,
        &kigumi_mod_mail::MANIFEST,
        &kigumi_mod_account::MANIFEST,
    );
}

#[tokio::test]
async fn crud_seams_emit_domain_events_atomically() {
    link();
    let Some(tdb) = kigumi_test::TestDb::new().await else { return };
    let db = &tdb.db;
    let su = kigumi_test::su();

    let (currency, partner, order, mv, account, journal) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("sale.order").unwrap(),
        resolve_registered("account.move").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true }).as_object().unwrap()).await.unwrap();
    let cust = db.insert_secured(&partner, &su, &[], &[], json!({ "name": "ACME" }).as_object().unwrap()).await.unwrap();

    // CREATE -> model.created
    let oid = db.insert_secured(&order, &su, &[], &[], json!({ "name": "New", "partner_id": cust, "currency_id": cur }).as_object().unwrap()).await.unwrap();
    let types = |evs: &[serde_json::Value]| -> Vec<String> { evs.iter().map(|e| e["event_type"].as_str().unwrap().to_string()).collect() };
    let evs = db.events_for("sale.order", oid).await.unwrap();
    assert!(types(&evs).contains(&"model.created".to_string()), "create emits model.created (got {:?})", types(&evs));

    // UPDATE a non-state field -> model.updated, NO state_changed.
    db.update_secured(&order, &su, &[], &[], oid, json!({ "name": "SO-1" }).as_object().unwrap()).await.unwrap();
    let evs = db.events_for("sale.order", oid).await.unwrap();
    let t = types(&evs);
    assert!(t.contains(&"model.updated".to_string()), "update emits model.updated");
    assert_eq!(t.iter().filter(|x| *x == "model.state_changed").count(), 0, "a non-state edit emits no state_changed");

    // STATE CHANGE draft -> sale -> model.state_changed with from/to.
    db.update_secured(&order, &su, &[], &[], oid, json!({ "state": "sale" }).as_object().unwrap()).await.unwrap();
    let evs = db.events_for("sale.order", oid).await.unwrap();
    let sc = evs.iter().find(|e| e["event_type"].as_str() == Some("model.state_changed")).expect("state change emits model.state_changed");
    assert_eq!(sc["change_summary"]["from"].as_str(), Some("draft"));
    assert_eq!(sc["change_summary"]["to"].as_str(), Some("sale"));

    // ATOMICITY: an unbalanced account.move is rejected (check_balanced) -> its tx rolls back -> NO event.
    let inc = db.insert_secured(&account, &su, &[], &[], json!({ "code": "400000", "name": "Sales", "account_type": "income" }).as_object().unwrap()).await.unwrap();
    let recv = db.insert_secured(&account, &su, &[], &[], json!({ "code": "121000", "name": "Recv", "account_type": "receivable" }).as_object().unwrap()).await.unwrap();
    let j = db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Inv", "code": "INV", "journal_type": "sale", "sequence_code": "INV" }).as_object().unwrap()).await.unwrap();
    // No fan-out runs in this test, so every event is still undispatched: pending count == total.
    let before = db.outbox_pending_count().await.unwrap();
    let bad = db.insert_secured(&mv, &su, &[], &[], json!({
        "move_type": "entry", "journal_id": j,
        "line_ids": [ { "account_id": inc, "debit": "0", "credit": "100" }, { "account_id": recv, "debit": "50", "credit": "0" } ]
    }).as_object().unwrap()).await;
    assert!(bad.is_err(), "an unbalanced move is rejected");
    let after = db.outbox_pending_count().await.unwrap();
    assert_eq!(after, before, "the rolled-back insert enqueued no event (transactional outbox)");

    // DELETE -> model.deleted (best-effort post-commit). Use a throwaway partner that nothing references.
    let tmp = db.insert_secured(&partner, &su, &[], &[], json!({ "name": "Temp" }).as_object().unwrap()).await.unwrap();
    db.delete_secured(&partner, &su, &[], &[], tmp).await.unwrap();
    let evs = db.events_for("res.partner", tmp).await.unwrap();
    assert!(types(&evs).contains(&"model.deleted".to_string()), "delete emits model.deleted");
}
