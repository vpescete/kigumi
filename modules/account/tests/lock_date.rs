//! Fiscal lock date: a journal entry dated on or before its company's lock date cannot be posted; a
//! later entry, or an entry with no company/lock, posts freely. Requires DATABASE_URL.

use kigumi::prelude::*;
use kigumi_db::Db;
use serde_json::json;

fn link() {
    let _ = (&kigumi_mod_account::MANIFEST, &kigumi_mod_base::MANIFEST, &kigumi_mod_mail::MANIFEST);
}

#[tokio::test]
async fn fiscal_lock_blocks_posting_in_a_locked_period() {
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
    db.ensure_sequence_schema().await.unwrap();

    let (currency, company, account, journal, mv) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.company").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
        resolve_registered("account.move").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true }).as_object().unwrap()).await.unwrap();
    let comp = db.insert_secured(&company, &su, &[], &[], json!({ "name": "Main", "currency_id": cur, "fiscalyear_lock_date": "2026-01-31", "active": true }).as_object().unwrap()).await.unwrap();
    let inc = db.insert_secured(&account, &su, &[], &[], json!({ "code": "400000", "name": "Sales", "account_type": "income" }).as_object().unwrap()).await.unwrap();
    let recv = db.insert_secured(&account, &su, &[], &[], json!({ "code": "121000", "name": "Receivable", "account_type": "receivable" }).as_object().unwrap()).await.unwrap();
    let j = db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Sales", "code": "INV", "journal_type": "sale", "sequence_code": "INV" }).as_object().unwrap()).await.unwrap();

    let balanced = |date: &str, comp: Option<i64>| {
        let mut m = json!({ "move_type": "entry", "journal_id": j, "date": date,
            "line_ids": [ { "account_id": inc, "debit": "0", "credit": "100" }, { "account_id": recv, "debit": "100", "credit": "0" } ] });
        if let Some(c) = comp { m["company_id"] = json!(c); }
        m
    };

    // Dated inside the locked period (<= 2026-01-31) → posting is refused.
    let locked = db.insert_secured(&mv, &su, &[], &[], balanced("2026-01-15", Some(comp)).as_object().unwrap()).await.unwrap();
    assert!(db.run_service(&mv, &su, &[], &[], locked, "post", serde_json::Map::new()).await.is_err(), "an entry in the locked period cannot be posted");
    assert_eq!(db.find_one_secured(&mv, &su, &[], &[], locked).await.unwrap().unwrap()["state"], "draft", "the refused entry stays draft");

    // The lock boundary date itself is locked (<=).
    let boundary = db.insert_secured(&mv, &su, &[], &[], balanced("2026-01-31", Some(comp)).as_object().unwrap()).await.unwrap();
    assert!(db.run_service(&mv, &su, &[], &[], boundary, "post", serde_json::Map::new()).await.is_err(), "the lock date itself is closed");

    // Dated after the lock → posts.
    let after = db.insert_secured(&mv, &su, &[], &[], balanced("2026-06-01", Some(comp)).as_object().unwrap()).await.unwrap();
    assert!(db.run_service(&mv, &su, &[], &[], after, "post", serde_json::Map::new()).await.is_ok(), "an entry after the lock posts");

    // No company (shared) → no lock applies, even in the locked period.
    let shared = db.insert_secured(&mv, &su, &[], &[], balanced("2026-01-10", None).as_object().unwrap()).await.unwrap();
    assert!(db.run_service(&mv, &su, &[], &[], shared, "post", serde_json::Map::new()).await.is_ok(), "an entry with no company is not locked");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
