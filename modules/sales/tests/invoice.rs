//! M16.4b: a confirmed sale order generates a posted, balanced customer invoice (account.move). Driven
//! by a sales.user with NO account groups — create_sale_invoice gates on the order write and posts the
//! move elevated — proving the cross-module invoicing seam end to end. Requires DATABASE_URL.

use meshble::prelude::*;
use meshble_db::Db;
use serde_json::json;

/// Link base + mail + sales + account so every referenced model is registered in this test binary.
fn link() {
    let _ = (
        &meshble_mod_sales::MANIFEST,
        &meshble_mod_base::MANIFEST,
        &meshble_mod_mail::MANIFEST,
        &meshble_mod_account::MANIFEST,
    );
}

fn money(v: &serde_json::Value, field: &str) -> f64 {
    v[field].as_str().unwrap_or_else(|| panic!("{field} not a string: {v:?}")).parse().unwrap()
}

#[tokio::test]
async fn confirmed_order_generates_a_posted_balanced_invoice() {
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

    let (currency, partner, product, order, mv, account, journal) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("sale.order").unwrap(),
        resolve_registered("account.move").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({
        "name": "Euro", "code": "EUR", "symbol": "€", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
    }).as_object().unwrap()).await.unwrap();
    // Chart (shared / no company so the company-less seller can read the order) + a sale journal.
    let recv = db.insert_secured(&account, &su, &[], &[], json!({ "code": "121000", "name": "Receivable", "account_type": "receivable" }).as_object().unwrap()).await.unwrap();
    let inc = db.insert_secured(&account, &su, &[], &[], json!({ "code": "400000", "name": "Sales", "account_type": "income" }).as_object().unwrap()).await.unwrap();
    let taxacc = db.insert_secured(&account, &su, &[], &[], json!({ "code": "251000", "name": "Tax", "account_type": "tax" }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Customer Invoices", "code": "INV", "journal_type": "sale", "sequence_code": "INV" }).as_object().unwrap()).await.unwrap();

    let cust = db.insert_secured(&partner, &su, &[], &[], json!({ "name": "ACME" }).as_object().unwrap()).await.unwrap();
    let prod = db.insert_secured(&product, &su, &[], &[], json!({ "name": "Widget", "list_price": 100.0 }).as_object().unwrap()).await.unwrap();

    // A draft order with a taxed line: 1 x 100 @ 22% → untaxed 100, tax 22, total 122.
    let oid = db.insert_secured(&order, &su, &[], &[], json!({
        "name": "New", "partner_id": cust, "currency_id": cur,
        "line_ids": [{ "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0, "tax_rate": "22" }]
    }).as_object().unwrap()).await.unwrap();
    db.run_action(&order, &su, &[], &[], oid, "confirm").await.unwrap();
    let confirmed = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(confirmed["invoice_status"], "to_invoice", "confirm marks the order to invoice");
    assert_eq!(money(&confirmed, "amount_total"), 122.0);

    // Invoice as a plain sales.user (no account groups): gate on order write, GL posting runs elevated.
    let seller = Ctx::new(1, vec!["sales.user".to_string()]);
    let (acls, rules) = (meshble_mod_sales::ACLS, meshble_mod_sales::RECORD_RULES);
    let move_id = db.run_service(&order, &seller, acls, rules, oid, "create_invoice", serde_json::Map::new()).await.unwrap()["invoice"].as_i64().unwrap();

    let inv = db.find_one_secured(&mv, &su, &[], &[], move_id).await.unwrap().unwrap();
    assert_eq!(inv["state"], "posted", "the invoice is posted");
    assert_eq!(inv["move_type"], "out_invoice");
    assert_eq!(money(&inv, "amount_total"), 122.0, "invoice total equals the order total");
    let lines = inv["line_ids"].as_array().unwrap();
    assert_eq!(lines.len(), 3, "income + tax + receivable");
    let net: f64 = lines.iter().map(|l| money(l, "balance")).sum();
    assert_eq!(net, 0.0, "the invoice is balanced");
    assert!(lines.iter().any(|l| l["account_id"].as_i64() == Some(inc) && money(l, "credit") == 100.0), "income credit 100");
    assert!(lines.iter().any(|l| l["account_id"].as_i64() == Some(taxacc) && money(l, "credit") == 22.0), "tax credit 22");
    assert!(lines.iter().any(|l| l["account_id"].as_i64() == Some(recv) && money(l, "debit") == 122.0), "receivable debit 122");

    let after = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(after["invoice_status"], "invoiced", "the order is now invoiced");

    // Re-invoicing is rejected (the order is no longer to_invoice).
    assert!(db.run_service(&order, &seller, acls, rules, oid, "create_invoice", serde_json::Map::new()).await.is_err(), "cannot invoice twice");

    // A non-positive total (here a 100% discount → total 0) is refused — no degenerate/zero invoice,
    // and the order is NOT marked invoiced (the claim never happened).
    let zero = db.insert_secured(&order, &su, &[], &[], json!({
        "name": "New", "partner_id": cust, "currency_id": cur,
        "line_ids": [{ "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0, "discount": "100" }]
    }).as_object().unwrap()).await.unwrap();
    db.run_action(&order, &su, &[], &[], zero, "confirm").await.unwrap();
    assert!(db.run_service(&order, &seller, acls, rules, zero, "create_invoice", serde_json::Map::new()).await.is_err(), "a non-positive total is refused");
    assert_eq!(
        db.find_one_secured(&order, &su, &[], &[], zero).await.unwrap().unwrap()["invoice_status"],
        "to_invoice",
        "the refused order keeps its to_invoice status (no orphan claim)"
    );

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
