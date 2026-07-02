//! A customer's accounting defaults flow to its orders: an order with NO explicit payment term bills at
//! the partner's property_payment_term_id, and an order with NO fiscal position remaps taxes through the
//! partner's property_account_position_id. Requires DATABASE_URL.

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

fn money(v: &serde_json::Value, field: &str) -> f64 {
    v[field].as_str().unwrap_or_else(|| panic!("{field} not a string: {v:?}")).parse().unwrap()
}

#[tokio::test]
async fn partner_accounting_defaults_flow_to_orders() {
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

    let (currency, partner, product, order, line, mv, account, journal, tax, term, fpos, fpostax) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("sale.order").unwrap(),
        resolve_registered("sale.order.line").unwrap(),
        resolve_registered("account.move").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
        resolve_registered("account.tax").unwrap(),
        resolve_registered("account.payment.term").unwrap(),
        resolve_registered("account.fiscal.position").unwrap(),
        resolve_registered("account.fiscal.position.tax").unwrap(),
    );
    let ins = |m: &ResolvedModel, v: serde_json::Value| {
        let db = &db; let su = &su; let m = m.clone();
        async move { db.insert_secured(&m, su, &[], &[], v.as_object().unwrap()).await.unwrap() }
    };

    let cur = ins(&currency, json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true })).await;
    ins(&account, json!({ "code": "121000", "name": "Receivable", "account_type": "receivable" })).await;
    ins(&account, json!({ "code": "400000", "name": "Sales", "account_type": "income" })).await;
    ins(&account, json!({ "code": "251000", "name": "Tax", "account_type": "tax" })).await;
    ins(&journal, json!({ "name": "Customer Invoices", "code": "INV", "journal_type": "sale", "sequence_code": "INV" })).await;
    let net30 = ins(&term, json!({ "name": "30 Days", "days": 30, "active": true })).await;
    let vat22 = ins(&tax, json!({ "name": "VAT 22%", "amount_type": "percent", "amount": "22", "active": true })).await;
    let export = ins(&fpos, json!({ "name": "Export", "active": true })).await;
    ins(&fpostax, json!({ "position_id": export, "tax_src_id": vat22 })).await; // NULL dest = drop VAT
    let prod = ins(&product, json!({ "name": "Widget", "list_price": 100.0 })).await;

    // A customer carrying BOTH defaults (the property_* fields hold plain ids).
    let cust = db.insert_secured(&partner, &su, &[], &[], json!({
        "name": "ACME", "property_payment_term_id": net30, "property_account_position_id": export
    }).as_object().unwrap()).await.unwrap();

    let seller = Ctx::new(1, vec!["sales.user".to_string()]);
    let (acls, rules) = (kigumi_mod_sales::ACLS, kigumi_mod_sales::RECORD_RULES);

    // Order with NO explicit term and NO explicit fiscal position: both come from the partner.
    let oid = db.insert_secured(&order, &su, &[], &[], json!({ "name": "New", "partner_id": cust, "currency_id": cur }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&line, &su, &[], &[], json!({ "order_id": oid, "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0, "tax_ids": [vat22] }).as_object().unwrap()).await.unwrap();

    // Fiscal default: apply_taxes remaps via the partner's Export position -> VAT dropped.
    db.run_service(&order, &seller, acls, rules, oid, "apply_taxes", serde_json::Map::new()).await.unwrap();
    let taxed = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(money(&taxed, "amount_tax"), 0.0, "the partner's Export fiscal position dropped VAT");

    // Payment-term default: the invoice due date is today + 30 (from the partner).
    db.run_action(&order, &su, &[], &[], oid, "confirm").await.unwrap();
    let mid = db.run_service(&order, &seller, acls, rules, oid, "create_invoice", serde_json::Map::new()).await.unwrap()["invoice"].as_i64().unwrap();
    let inv = db.find_one_secured(&mv, &su, &[], &[], mid).await.unwrap().unwrap();
    assert!(inv["invoice_date_due"].as_str().unwrap() > inv["date"].as_str().unwrap(), "the partner's 30-day term pushed the due date past the invoice date");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
