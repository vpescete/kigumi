//! M16.1: chart of accounts + journals on a real database. Migrate the catalog, create a receivable
//! account, an income account and a sale journal, round-trip them, and confirm the ACL reserves
//! account creation to account.manager. Requires DATABASE_URL.

use kigumi::prelude::*;
use kigumi_db::Db;
use serde_json::json;

/// Link the module crates so their inventory registrations are present (account depends on base + mail).
fn link() {
    let _ = (&kigumi_mod_account::MANIFEST, &kigumi_mod_base::MANIFEST, &kigumi_mod_mail::MANIFEST);
}

#[tokio::test]
async fn chart_and_journals_round_trip_and_acl() {
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

    let (currency, company, account, journal) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.company").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({
        "name": "Euro", "code": "EUR", "symbol": "€", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
    }).as_object().unwrap()).await.unwrap();
    let comp = db.insert_secured(&company, &su, &[], &[], json!({ "name": "Main", "currency_id": cur, "active": true }).as_object().unwrap()).await.unwrap();

    // A receivable account (reconcilable), an income account, and a sale journal defaulting to income.
    let recv = db.insert_secured(&account, &su, &[], &[], json!({ "code": "121000", "name": "Account Receivable", "account_type": "receivable", "reconcile": true, "company_id": comp }).as_object().unwrap()).await.unwrap();
    let inc = db.insert_secured(&account, &su, &[], &[], json!({ "code": "400000", "name": "Product Sales", "account_type": "income", "company_id": comp }).as_object().unwrap()).await.unwrap();
    let jid = db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Customer Invoices", "code": "INV", "journal_type": "sale", "company_id": comp, "default_account_id": inc, "sequence_code": "INV" }).as_object().unwrap()).await.unwrap();

    let got = db.find_one_secured(&account, &su, &[], &[], recv).await.unwrap().unwrap();
    assert_eq!(got["account_type"], "receivable");
    assert_eq!(got["reconcile"].as_bool(), Some(true), "reconcile round-trips as a boolean");
    let gj = db.find_one_secured(&journal, &su, &[], &[], jid).await.unwrap().unwrap();
    assert_eq!(gj["journal_type"], "sale");
    assert_eq!(gj["default_account_id"].as_i64(), Some(inc), "journal default account links the income account");

    // ACL: account.user (non-manager) cannot create accounts — that is reserved to account.manager.
    let clerk = Ctx::new(1, vec!["account.user".to_string()]);
    let denied = db
        .insert_secured(&account, &clerk, kigumi_mod_account::ACLS, &[], json!({ "code": "999", "name": "X", "account_type": "expense", "company_id": comp }).as_object().unwrap())
        .await;
    assert!(denied.is_err(), "account.user cannot create accounts (manager-only)");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
