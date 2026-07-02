//! A payment term on a sale order pushes the invoice due date forward by its `days`; with no term the
//! due date equals the invoice date. ISO dates are lexically chronological, so a string compare is exact.
//! One test per binary (the repo convention) since each rebuilds the whole schema. Requires DATABASE_URL.

use kigumi::prelude::*;
use kigumi_db::Db;
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
async fn payment_term_shifts_the_invoice_due_date() {
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
    db.ensure_sequence("SO", "SO/", "", 5).await.unwrap();

    let (currency, partner, product, order, mv, account, journal, term) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("sale.order").unwrap(),
        resolve_registered("account.move").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
        resolve_registered("account.payment.term").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({
        "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
    }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&account, &su, &[], &[], json!({ "code": "121000", "name": "Receivable", "account_type": "receivable" }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&account, &su, &[], &[], json!({ "code": "400000", "name": "Sales", "account_type": "income" }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Customer Invoices", "code": "INV", "journal_type": "sale", "sequence_code": "INV" }).as_object().unwrap()).await.unwrap();
    let cust = db.insert_secured(&partner, &su, &[], &[], json!({ "name": "ACME" }).as_object().unwrap()).await.unwrap();
    let prod = db.insert_secured(&product, &su, &[], &[], json!({ "name": "Widget", "list_price": 100.0 }).as_object().unwrap()).await.unwrap();
    let net30 = db.insert_secured(&term, &su, &[], &[], json!({ "name": "30 Days", "days": 30, "active": true }).as_object().unwrap()).await.unwrap();

    let seller = Ctx::new(1, vec!["sales.user".to_string()]);
    let (acls, rules) = (kigumi_mod_sales::ACLS, kigumi_mod_sales::RECORD_RULES);

    // Order WITH a 30-day term: the due date is pushed past the accounting date.
    let with_term = db.insert_secured(&order, &su, &[], &[], json!({
        "name": "New", "partner_id": cust, "currency_id": cur, "payment_term_id": net30,
        "line_ids": [{ "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0 }]
    }).as_object().unwrap()).await.unwrap();
    db.run_action(&order, &su, &[], &[], with_term, "confirm").await.unwrap();
    let mid = db.run_service(&order, &seller, acls, rules, with_term, "create_invoice", serde_json::Map::new()).await.unwrap()["invoice"].as_i64().unwrap();
    let inv = db.find_one_secured(&mv, &su, &[], &[], mid).await.unwrap().unwrap();
    let (date, due) = (inv["date"].as_str().unwrap(), inv["invoice_date_due"].as_str().unwrap());
    assert!(due > date, "a 30-day term moves the due date ({due}) past the invoice date ({date})");

    // Order WITHOUT a term: the due date equals the accounting date.
    let no_term = db.insert_secured(&order, &su, &[], &[], json!({
        "name": "New", "partner_id": cust, "currency_id": cur,
        "line_ids": [{ "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0 }]
    }).as_object().unwrap()).await.unwrap();
    db.run_action(&order, &su, &[], &[], no_term, "confirm").await.unwrap();
    let mid2 = db.run_service(&order, &seller, acls, rules, no_term, "create_invoice", serde_json::Map::new()).await.unwrap()["invoice"].as_i64().unwrap();
    let inv2 = db.find_one_secured(&mv, &su, &[], &[], mid2).await.unwrap().unwrap();
    assert_eq!(inv2["date"], inv2["invoice_date_due"], "no term means due == invoice date");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
