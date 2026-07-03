//! M17.1: locations + quants on a real database. The composite-unique index allows one quant per
//! (product, location) and rejects a duplicate; locations and warehouses round-trip. Requires DATABASE_URL.

use kigumi::prelude::*;
use serde_json::json;

fn link() {
    let _ = (
        &kigumi_mod_stock::MANIFEST,
        &kigumi_mod_sales::MANIFEST,
        &kigumi_mod_base::MANIFEST,
        &kigumi_mod_mail::MANIFEST,
    );
}

#[tokio::test]
async fn locations_and_quants_enforce_one_per_product_location() {
    link();
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let su = kigumi_test::su();

    let (currency, company, product, location, warehouse, quant) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.company").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("stock.location").unwrap(),
        resolve_registered("stock.warehouse").unwrap(),
        resolve_registered("stock.quant").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({
        "name": "Euro", "code": "EUR", "symbol": "€", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
    }).as_object().unwrap()).await.unwrap();
    let comp = db.insert_secured(&company, &su, &[], &[], json!({ "name": "Main", "currency_id": cur, "active": true }).as_object().unwrap()).await.unwrap();
    let prod = db.insert_secured(&product, &su, &[], &[], json!({ "name": "Widget", "list_price": 100.0 }).as_object().unwrap()).await.unwrap();

    let stock = db.insert_secured(&location, &su, &[], &[], json!({ "name": "Stock", "usage": "internal", "company_id": comp }).as_object().unwrap()).await.unwrap();
    let wh = db.insert_secured(&warehouse, &su, &[], &[], json!({ "name": "Main", "code": "WH", "location_id": stock, "company_id": comp }).as_object().unwrap()).await.unwrap();
    assert_eq!(db.find_one_secured(&warehouse, &su, &[], &[], wh).await.unwrap().unwrap()["code"], "WH");

    // A quant for (product, Stock) is created.
    let q = db.insert_secured(&quant, &su, &[], &[], json!({ "product_id": prod, "location_id": stock, "quantity": "5" }).as_object().unwrap()).await.unwrap();
    assert_eq!(db.find_one_secured(&quant, &su, &[], &[], q).await.unwrap().unwrap()["quantity"].as_str().and_then(|s| s.parse::<f64>().ok()), Some(5.0));

    // A second quant for the SAME (product, location) is rejected by the composite-unique index.
    let dup = db.insert_secured(&quant, &su, &[], &[], json!({ "product_id": prod, "location_id": stock, "quantity": "3" }).as_object().unwrap()).await;
    assert!(dup.is_err(), "duplicate (product, location) quant must be rejected");
    assert_eq!(db.count_secured(&quant, &su, &[], &[], None).await.unwrap(), 1, "only the first quant exists");
}
