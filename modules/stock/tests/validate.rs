//! M17.2: the validate mechanism on a real database. A receipt raises on-hand, a delivery lowers it,
//! the quants move atomically (src down, dst up), and a validated transfer cannot be validated twice.
//! Requires DATABASE_URL.

use kigumi::prelude::*;
use kigumi_db::Db;
use serde_json::json;

fn link() {
    let _ = (
        &kigumi_mod_stock::MANIFEST,
        &kigumi_mod_sales::MANIFEST,
        &kigumi_mod_base::MANIFEST,
        &kigumi_mod_mail::MANIFEST,
    );
}

/// On-hand of a product, read back from the materialized `qty_available`.
async fn on_hand(db: &Db, su: &Ctx, product: &ResolvedModel, id: i64) -> f64 {
    db.find_one_secured(product, su, &[], &[], id)
        .await
        .unwrap()
        .unwrap()["qty_available"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap()
}

/// Quantity of a quant at (product, location), or 0.0 if none exists.
async fn quant_at(db: &Db, su: &Ctx, quant: &ResolvedModel, product: i64, location: i64) -> f64 {
    let dom = Domain::field("product_id").eq(product).and(Domain::field("location_id").eq(location));
    match db.find_secured(quant, su, &[], &[], Some(&dom)).await.unwrap().first() {
        Some(q) => q["quantity"].as_str().and_then(|s| s.parse().ok()).unwrap(),
        None => 0.0,
    }
}

#[tokio::test]
async fn validate_moves_stock_and_is_single_shot() {
    link();
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let su = kigumi_test::su();

    let (currency, company, product, location, picking, mv, quant) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.company").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("stock.location").unwrap(),
        resolve_registered("stock.picking").unwrap(),
        resolve_registered("stock.move").unwrap(),
        resolve_registered("stock.quant").unwrap(),
    );
    let ins = |m: &ResolvedModel, v: serde_json::Value| {
        let db = &db; let su = &su;
        let m = m.clone();
        async move { db.insert_secured(&m, su, &[], &[], v.as_object().unwrap()).await.unwrap() }
    };

    let cur = ins(&currency, json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true })).await;
    let comp = ins(&company, json!({ "name": "Main", "currency_id": cur, "active": true })).await;
    let prod = ins(&product, json!({ "name": "Widget", "list_price": 100.0 })).await;

    // On-hand is read-only: an explicit write of qty_available is rejected (only the validate mechanism
    // sets it). Even sudo is refused — this is a field-writability rule, not an access check.
    let ro = db.insert_secured(&product, &su, &[], &[], json!({ "name": "RO", "qty_available": "5" }).as_object().unwrap()).await;
    assert!(ro.is_err(), "writing the read-only on-hand must be rejected");

    let stock = ins(&location, json!({ "name": "Stock", "usage": "internal", "company_id": comp })).await;
    let vendors = ins(&location, json!({ "name": "Vendors", "usage": "supplier", "company_id": comp })).await;
    let customers = ins(&location, json!({ "name": "Customers", "usage": "customer", "company_id": comp })).await;

    // A receipt of 7 widgets: Vendors -> Stock.
    let receipt = ins(&picking, json!({ "picking_type": "receipt", "location_id": vendors, "location_dest_id": stock, "company_id": comp })).await;
    ins(&mv, json!({ "picking_id": receipt, "product_id": prod, "product_uom_qty": "7", "location_id": vendors, "location_dest_id": stock })).await;

    assert_eq!(on_hand(&db, &su, &product, prod).await, 0.0, "on-hand starts at zero");
    let n1 = db.run_service(&picking, &su, &[], &[], receipt, "validate", serde_json::Map::new()).await.unwrap()["validated"].as_str().unwrap().to_string();
    assert!(n1.starts_with("IN/"), "receipt is numbered from the IN sequence, got {n1}");
    assert_eq!(on_hand(&db, &su, &product, prod).await, 7.0, "receipt raises on-hand to 7");
    assert_eq!(quant_at(&db, &su, &quant, prod, stock).await, 7.0, "Stock quant gained 7");
    assert_eq!(quant_at(&db, &su, &quant, prod, vendors).await, -7.0, "Vendors quant lost 7");
    assert_eq!(db.find_one_secured(&picking, &su, &[], &[], receipt).await.unwrap().unwrap()["state"], "done");

    // Re-validating the same transfer is rejected (it is no longer a draft).
    assert!(db.run_service(&picking, &su, &[], &[], receipt, "validate", serde_json::Map::new()).await.is_err(), "a done transfer cannot be validated again");

    // A delivery of 3 widgets: Stock -> Customers.
    let delivery = ins(&picking, json!({ "picking_type": "delivery", "location_id": stock, "location_dest_id": customers, "company_id": comp })).await;
    ins(&mv, json!({ "picking_id": delivery, "product_id": prod, "product_uom_qty": "3", "location_id": stock, "location_dest_id": customers })).await;
    let n2 = db.run_service(&picking, &su, &[], &[], delivery, "validate", serde_json::Map::new()).await.unwrap()["validated"].as_str().unwrap().to_string();
    assert!(n2.starts_with("OUT/"), "delivery is numbered from the OUT sequence, got {n2}");
    assert_eq!(on_hand(&db, &su, &product, prod).await, 4.0, "delivery lowers on-hand to 4");
    assert_eq!(quant_at(&db, &su, &quant, prod, stock).await, 4.0, "Stock quant is now 4");
    assert_eq!(quant_at(&db, &su, &quant, prod, customers).await, 3.0, "Customers quant gained 3 (the sink fills as we ship)");

    // Validating an unknown id errors (does not panic).
    assert!(db.run_service(&picking, &su, &[], &[], 999_999, "validate", serde_json::Map::new()).await.is_err(), "unknown transfer id errors");

    // A transfer with no moves cannot be validated (no phantom done picking).
    let empty = ins(&picking, json!({ "picking_type": "internal", "location_id": stock, "location_dest_id": customers, "company_id": comp })).await;
    assert!(db.run_service(&picking, &su, &[], &[], empty, "validate", serde_json::Map::new()).await.is_err(), "an empty transfer cannot be validated");
    assert_eq!(db.find_one_secured(&picking, &su, &[], &[], empty).await.unwrap().unwrap()["state"], "draft", "the empty transfer stays draft");
}
