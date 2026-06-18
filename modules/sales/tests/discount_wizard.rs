//! M15.4 slice 3: the sale.order.discount wizard applied end to end on a real database. Opening is
//! covered by the server test; here we exercise the apply service method — it writes the wizard's
//! discount onto every line of the target order and the line/order compute cascade re-rolls the
//! amounts — plus the boundary check that a percent outside [0, 100] is rejected. Requires DATABASE_URL.

use meshble::prelude::*;
use meshble_db::Db;
use serde_json::json;

/// Link the module crates so their inventory registrations are present.
fn link() {
    let _ = (&meshble_mod_sales::MANIFEST, &meshble_mod_base::MANIFEST, &meshble_mod_mail::MANIFEST);
}

fn money(v: &serde_json::Value, field: &str) -> f64 {
    v[field].as_str().unwrap_or_else(|| panic!("{field} not a string: {v:?}")).parse().unwrap()
}

#[tokio::test]
async fn discount_wizard_applies_to_every_line_and_rerolls_totals() {
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
    db.ensure_transient_defaults().await.unwrap(); // give sale_order_discount.create_date a DEFAULT now()

    let (currency, partner, product, order, line, wizard) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("sale.order").unwrap(),
        resolve_registered("sale.order.line").unwrap(),
        resolve_registered("sale.order.discount").unwrap(),
    );

    let cur = db.insert_secured(&currency, &su, &[], &[], json!({
        "name": "Euro", "code": "EUR", "symbol": "€", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
    }).as_object().unwrap()).await.unwrap();
    let cust = db.insert_secured(&partner, &su, &[], &[], json!({ "name": "ACME" }).as_object().unwrap()).await.unwrap();
    let prod = db.insert_secured(&product, &su, &[], &[], json!({ "name": "Widget", "list_price": 100.0, "standard_price": 60.0 }).as_object().unwrap()).await.unwrap();

    // 2×100 + 1×50 = 250 untaxed (no discount yet, tax_rate 0).
    let oid = db.insert_secured(&order, &su, &[], &[], json!({
        "partner_id": cust, "currency_id": cur,
        "line_ids": [
            { "product_id": prod, "product_uom_qty": 2, "price_unit": 100.0 },
            { "product_id": prod, "product_uom_qty": 1, "price_unit": 50.0 }
        ]
    }).as_object().unwrap()).await.unwrap();
    let before = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(money(&before, "amount_untaxed"), 250.0, "no discount yet");

    // Open-equivalent: a wizard row seeded with the order + a 10% discount (create_date via DB default).
    let wid = db.insert_secured(&wizard, &su, &[], &[], json!({ "order_id": oid, "discount": "10" }).as_object().unwrap()).await.unwrap();
    let applied = db.apply_sale_order_discount(&su, &[], &[], wid).await.unwrap();
    assert_eq!(applied, 2, "both lines discounted");

    // Every line now carries the discount, and the order amount re-rolled: 250 less 10% = 225.
    let lines = db.find_secured(&line, &su, &[], &[], Some(&Domain::field("order_id").eq(oid))).await.unwrap();
    assert_eq!(lines.len(), 2);
    for l in &lines {
        assert_eq!(l["discount"].as_str().and_then(|s| s.parse::<f64>().ok()), Some(10.0), "line discount set");
    }
    let after = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(money(&after, "amount_untaxed"), 225.0, "amount_untaxed re-rolled after the discount");
    assert_eq!(money(&after, "amount_total"), 225.0, "amount_total re-rolled (tax 0)");

    // Boundary: a percent outside [0, 100] is rejected, and nothing is written.
    let bad = db.insert_secured(&wizard, &su, &[], &[], json!({ "order_id": oid, "discount": "150" }).as_object().unwrap()).await.unwrap();
    assert!(db.apply_sale_order_discount(&su, &[], &[], bad).await.is_err(), "out-of-range discount rejected");
    let still = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(money(&still, "amount_untaxed"), 225.0, "rejected apply left the order unchanged");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
