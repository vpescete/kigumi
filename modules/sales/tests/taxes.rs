//! Runtime tax engine: a sale.order line's effective `tax_rate` is DERIVED from its `account.tax`
//! (not hand-typed) by `apply_taxes`, and the existing line/order compute cascade then rolls it into
//! price_tax / amount_tax / amount_total. Driven by a plain sales.user. Requires DATABASE_URL.

use meshble::prelude::*;
use meshble_db::Db;
use serde_json::json;

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
async fn apply_taxes_derives_the_rate_from_account_tax() {
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

    let (currency, partner, product, order, line, tax) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("sale.order").unwrap(),
        resolve_registered("sale.order.line").unwrap(),
        resolve_registered("account.tax").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({
        "name": "Euro", "code": "EUR", "symbol": "€", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
    }).as_object().unwrap()).await.unwrap();
    let vat22 = db.insert_secured(&tax, &su, &[], &[], json!({
        "name": "VAT 22%", "type_tax_use": "sale", "amount_type": "percent", "amount": "22", "active": true
    }).as_object().unwrap()).await.unwrap();
    let cust = db.insert_secured(&partner, &su, &[], &[], json!({ "name": "ACME" }).as_object().unwrap()).await.unwrap();
    let prod = db.insert_secured(&product, &su, &[], &[], json!({ "name": "Widget", "list_price": 100.0 }).as_object().unwrap()).await.unwrap();

    // A line referencing the tax but with NO rate typed in (tax_rate defaults to 0).
    let oid = db.insert_secured(&order, &su, &[], &[], json!({
        "name": "New", "partner_id": cust, "currency_id": cur,
        "line_ids": [{ "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0, "tax_id": vat22 }]
    }).as_object().unwrap()).await.unwrap();

    let before = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(money(&before, "amount_tax"), 0.0, "no tax before apply_taxes (rate not yet derived)");
    assert_eq!(money(&before, "amount_total"), 100.0);

    // Derive the rate from account.tax, as a plain sales.user (gates on order write).
    let seller = Ctx::new(1, vec!["sales.user".to_string()]);
    let (acls, rules) = (meshble_mod_sales::ACLS, meshble_mod_sales::RECORD_RULES);
    let n = db.apply_taxes(&seller, acls, rules, oid).await.unwrap();
    assert_eq!(n, 1, "one line processed");

    let after = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    let l = &after["line_ids"].as_array().unwrap()[0];
    assert_eq!(money(l, "tax_rate"), 22.0, "rate materialized from the tax");
    assert_eq!(money(l, "price_tax"), 22.0);
    assert_eq!(money(l, "price_total"), 122.0);
    assert_eq!(money(&after, "amount_untaxed"), 100.0);
    assert_eq!(money(&after, "amount_tax"), 22.0, "tax now rolled into the order");
    assert_eq!(money(&after, "amount_total"), 122.0);

    // A fixed-amount tax does NOT map onto the percentage compute in v1 → rate 0.
    let fixed = db.insert_secured(&tax, &su, &[], &[], json!({
        "name": "Eco fee", "type_tax_use": "sale", "amount_type": "fixed", "amount": "5", "active": true
    }).as_object().unwrap()).await.unwrap();
    db.update_secured(&line, &su, &[], &[], l["id"].as_i64().unwrap(), json!({ "tax_id": fixed }).as_object().unwrap()).await.unwrap();
    db.apply_taxes(&seller, acls, rules, oid).await.unwrap();
    let after2 = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(money(&after2, "amount_tax"), 0.0, "fixed tax yields rate 0 in v1");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
