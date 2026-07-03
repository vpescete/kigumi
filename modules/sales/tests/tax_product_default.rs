//! Product default taxes flow to the line: a line with no explicit tax (no tax_ids, no legacy tax_id)
//! inherits its product's default taxes (product.template.taxes_id) when apply_taxes runs. Requires
//! DATABASE_URL.

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

fn money(v: &serde_json::Value, field: &str) -> f64 {
    v[field].as_str().unwrap_or_else(|| panic!("{field} not a string: {v:?}")).parse().unwrap()
}

#[tokio::test]
async fn line_inherits_the_product_default_taxes() {
    link();
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let su = kigumi_test::su();

    let (currency, partner, product, template, order, line, tax) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("product.template").unwrap(),
        resolve_registered("sale.order").unwrap(),
        resolve_registered("sale.order.line").unwrap(),
        resolve_registered("account.tax").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true }).as_object().unwrap()).await.unwrap();
    let vat22 = db.insert_secured(&tax, &su, &[], &[], json!({ "name": "VAT 22%", "amount_type": "percent", "amount": "22", "active": true }).as_object().unwrap()).await.unwrap();
    let cust = db.insert_secured(&partner, &su, &[], &[], json!({ "name": "ACME" }).as_object().unwrap()).await.unwrap();
    let prod = db.insert_secured(&product, &su, &[], &[], json!({ "name": "Widget", "list_price": 100.0 }).as_object().unwrap()).await.unwrap();
    // Make VAT the product's default tax (set on the variant's template).
    let tmpl = db.find_one_secured(&product, &su, &[], &[], prod).await.unwrap().unwrap()["product_tmpl_id"].as_i64().unwrap();
    db.update_secured(&template, &su, &[], &[], tmpl, json!({ "taxes_id": [vat22] }).as_object().unwrap()).await.unwrap();

    // A line with the product but NO explicit tax (no tax_ids, no tax_id).
    let oid = db.insert_secured(&order, &su, &[], &[], json!({ "name": "New", "partner_id": cust, "currency_id": cur }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&line, &su, &[], &[], json!({ "order_id": oid, "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0 }).as_object().unwrap()).await.unwrap();

    let seller = Ctx::new(1, vec!["sales.user".to_string()]);
    let (acls, rules) = (kigumi_mod_sales::ACLS, kigumi_mod_sales::RECORD_RULES);
    db.run_service(&order, &seller, acls, rules, oid, "apply_taxes", serde_json::Map::new()).await.unwrap();

    let after = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(money(&after, "amount_tax"), 22.0, "the line inherited the product's default VAT");
    assert_eq!(money(&after, "amount_total"), 122.0);
}
