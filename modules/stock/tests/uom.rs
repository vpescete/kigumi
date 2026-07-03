//! UoM conversion on moves: a move expressed in a non-reference unit (e.g. a dozen, factor 12) is
//! converted to the product reference unit before it touches a quant, so on-hand is always in the
//! reference unit. Reserve converts the demand too (reserved_qty is reference). Requires DATABASE_URL.

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

async fn on_hand(db: &Db, su: &Ctx, product: &ResolvedModel, id: i64) -> f64 {
    db.find_one_secured(product, su, &[], &[], id).await.unwrap().unwrap()["qty_available"]
        .as_str().and_then(|s| s.parse().ok()).unwrap()
}

async fn quant_field(db: &Db, su: &Ctx, quant: &ResolvedModel, product: i64, location: i64, field: &str) -> f64 {
    let dom = Domain::field("product_id").eq(product).and(Domain::field("location_id").eq(location));
    match db.find_secured(quant, su, &[], &[], Some(&dom)).await.unwrap().first() {
        Some(q) => q[field].as_str().and_then(|s| s.parse().ok()).unwrap(),
        None => 0.0,
    }
}

#[tokio::test]
async fn moves_convert_their_uom_to_the_reference_unit() {
    link();
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let su = kigumi_test::su();

    let (currency, company, product, location, picking, mv, quant, uom) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.company").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("stock.location").unwrap(),
        resolve_registered("stock.picking").unwrap(),
        resolve_registered("stock.move").unwrap(),
        resolve_registered("stock.quant").unwrap(),
        resolve_registered("uom.uom").unwrap(),
    );
    let ins = |m: &ResolvedModel, v: serde_json::Value| {
        let db = &db; let su = &su; let m = m.clone();
        async move { db.insert_secured(&m, su, &[], &[], v.as_object().unwrap()).await.unwrap() }
    };

    let cur = ins(&currency, json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true })).await;
    let comp = ins(&company, json!({ "name": "Main", "currency_id": cur, "active": true })).await;
    let prod = ins(&product, json!({ "name": "Widget", "list_price": 100.0 })).await;
    // A "Dozen" unit: 1 dozen = 12 reference units.
    let dozen = ins(&uom, json!({ "name": "Dozen", "uom_type": "bigger", "factor": 12.0, "rounding": 0.01, "active": true })).await;
    let stock = ins(&location, json!({ "name": "Stock", "usage": "internal", "company_id": comp })).await;
    let vendors = ins(&location, json!({ "name": "Vendors", "usage": "supplier", "company_id": comp })).await;
    let customers = ins(&location, json!({ "name": "Customers", "usage": "customer", "company_id": comp })).await;

    // Receive 2 DOZEN -> 24 reference units on hand.
    let receipt = ins(&picking, json!({ "picking_type": "receipt", "location_id": vendors, "location_dest_id": stock, "company_id": comp })).await;
    ins(&mv, json!({ "picking_id": receipt, "product_id": prod, "product_uom_qty": "2", "product_uom_id": dozen, "location_id": vendors, "location_dest_id": stock })).await;
    db.run_service(&picking, &su, &[], &[], receipt, "validate", serde_json::Map::new()).await.unwrap();
    assert_eq!(on_hand(&db, &su, &product, prod).await, 24.0, "2 dozen = 24 reference units");
    assert_eq!(quant_field(&db, &su, &quant, prod, stock, "quantity").await, 24.0);

    // Deliver 1 DOZEN -> 12 left.
    let d1 = ins(&picking, json!({ "picking_type": "delivery", "location_id": stock, "location_dest_id": customers, "company_id": comp })).await;
    ins(&mv, json!({ "picking_id": d1, "product_id": prod, "product_uom_qty": "1", "product_uom_id": dozen, "location_id": stock, "location_dest_id": customers })).await;
    db.run_service(&picking, &su, &[], &[], d1, "validate", serde_json::Map::new()).await.unwrap();
    assert_eq!(on_hand(&db, &su, &product, prod).await, 12.0, "delivering 1 dozen removes 12");

    // Deliver 10 REFERENCE units (no uom) -> 2 left. Mixing units against the same reference quant works.
    let d2 = ins(&picking, json!({ "picking_type": "delivery", "location_id": stock, "location_dest_id": customers, "company_id": comp })).await;
    ins(&mv, json!({ "picking_id": d2, "product_id": prod, "product_uom_qty": "10", "location_id": stock, "location_dest_id": customers })).await;
    db.run_service(&picking, &su, &[], &[], d2, "validate", serde_json::Map::new()).await.unwrap();
    assert_eq!(on_hand(&db, &su, &product, prod).await, 2.0, "delivering 10 units removes 10");

    // Reserve a half-dozen (6 reference) against the 2 on hand: reserved is clamped to availability, in
    // the reference unit. A delivery move of 0.5 dozen reserves only the available 2.
    let d3 = ins(&picking, json!({ "picking_type": "delivery", "location_id": stock, "location_dest_id": customers, "company_id": comp })).await;
    let m3 = ins(&mv, json!({ "picking_id": d3, "product_id": prod, "product_uom_qty": "0.5", "product_uom_id": dozen, "location_id": stock, "location_dest_id": customers })).await;
    db.run_service(&picking, &su, &[], &[], d3, "reserve", serde_json::Map::new()).await.unwrap();
    let m3r = db.find_one_secured(&mv, &su, &[], &[], m3).await.unwrap().unwrap()["reserved_qty"].as_str().unwrap().parse::<f64>().unwrap();
    assert_eq!(m3r, 2.0, "0.5 dozen = 6 wanted, only 2 free -> reserved 2 reference units");
    assert_eq!(quant_field(&db, &su, &quant, prod, stock, "reserved_quantity").await, 2.0);
}
