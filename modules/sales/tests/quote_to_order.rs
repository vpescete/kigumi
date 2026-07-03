//! M5 quote-to-order vertical, end to end on a real database: migrate the whole registered catalog
//! (base + sales) in FK order, seed currency/company/customer/product, build a draft order with two
//! nested lines, check the stored computes roll up (subtotal/margin → amount_total/margin), then run
//! the `confirm` action and check the state transition + SO numbering.

use kigumi::prelude::*;
use serde_json::json;

/// Force the module crates to link so their `inventory` registrations are present (sales depends on
/// base and mail — sale.order opts into the mail subsystem, so mail's catalog must be migrated too).
fn link() {
    let _ = (&kigumi_mod_sales::MANIFEST, &kigumi_mod_base::MANIFEST, &kigumi_mod_mail::MANIFEST);
}

/// A computed money field comes back as an exact JSON string; parse it for scale-independent asserts.
fn money(v: &serde_json::Value, field: &str) -> f64 {
    v[field].as_str().unwrap_or_else(|| panic!("{field} not a string: {v:?}")).parse().unwrap()
}

#[tokio::test]
async fn quote_to_order_rolls_up_and_confirms() {
    link();
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let su = kigumi_test::su();
    db.ensure_sequence("SO", "SO/", "", 5).await.unwrap();

    let (currency, company, partner, product, order) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.company").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("sale.order").unwrap(),
    );

    let cur = db.insert_secured(&currency, &su, &[], &[], json!({
        "name": "Euro", "code": "EUR", "symbol": "€", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
    }).as_object().unwrap()).await.unwrap();
    let comp = db.insert_secured(&company, &su, &[], &[], json!({ "name": "Main", "currency_id": cur, "active": true }).as_object().unwrap()).await.unwrap();
    let cust = db.insert_secured(&partner, &su, &[], &[], json!({ "name": "ACME" }).as_object().unwrap()).await.unwrap();
    let prod = db.insert_secured(&product, &su, &[], &[], json!({
        "name": "Widget", "default_code": "W-1", "list_price": 100.0, "standard_price": 60.0
    }).as_object().unwrap()).await.unwrap();

    // Draft order with two nested lines (bare objects = create).
    let oid = db.insert_secured(&order, &su, &[], &[], json!({
        "partner_id": cust, "company_id": comp, "currency_id": cur,
        "line_ids": [
            { "product_id": prod, "name": "Widget x2", "product_uom_qty": 2, "price_unit": 100.0, "purchase_price": 60.0 },
            { "product_id": prod, "name": "Widget x1", "product_uom_qty": 1, "price_unit": 50.0,  "purchase_price": 30.0 }
        ]
    }).as_object().unwrap()).await.unwrap();

    // Lines: 2×100 + 1×50 = 250 subtotal; margin (100−60)×2 + (50−30)×1 = 80 + 20 = 100.
    let got = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(got["state"], "draft", "starts as a draft");
    assert_eq!(got["name"], "New", "unnumbered until confirmed");
    assert_eq!(money(&got, "amount_total"), 250.0, "amount_total rolls up from the lines");
    assert_eq!(money(&got, "margin"), 100.0, "margin rolls up from the lines");

    // Confirm: draft → sale, assigned an SO number.
    db.run_action(&order, &su, &[], &[], oid, "confirm").await.unwrap();
    let confirmed = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(confirmed["state"], "sale", "confirmed");
    assert!(confirmed["name"].as_str().unwrap().starts_with("SO/"), "numbered, got {:?}", confirmed["name"]);
    // Totals survive the transition unchanged.
    assert_eq!(money(&confirmed, "amount_total"), 250.0);

    // Record-rule parity: a non-su clerk sees the lines of a live order, but once the order is
    // locked done they vanish for the clerk (line_parent_not_done), while su still sees them.
    let clerk = Ctx::new(1, vec!["sales.user".to_string()]);
    let line = resolve_registered("sale.order.line").unwrap();
    let (acls, rules) = (kigumi_mod_sales::ACLS, kigumi_mod_sales::RECORD_RULES);
    assert_eq!(db.count_secured(&line, &clerk, acls, rules, None).await.unwrap(), 2, "clerk sees lines of a live order");

    db.run_action(&order, &su, &[], &[], oid, "done").await.unwrap();
    assert_eq!(db.count_secured(&line, &clerk, acls, rules, None).await.unwrap(), 0, "done order's lines hidden from the clerk");
    assert_eq!(db.count_secured(&line, &su, &[], &[], None).await.unwrap(), 2, "su still sees them");
}
