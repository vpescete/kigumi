//! Partial transfers + backorders + the over-delivery guard. Validating a transfer short processes the
//! quantity_done, spills the remainder into a draft backorder, and an internal source never goes
//! negative (a delivery clamps to on-hand). Requires DATABASE_URL.

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

async fn quant_at(db: &Db, su: &Ctx, quant: &ResolvedModel, product: i64, location: i64) -> f64 {
    let dom = Domain::field("product_id").eq(product).and(Domain::field("location_id").eq(location));
    match db.find_secured(quant, su, &[], &[], Some(&dom)).await.unwrap().first() {
        Some(q) => q["quantity"].as_str().and_then(|s| s.parse().ok()).unwrap(),
        None => 0.0,
    }
}

#[tokio::test]
async fn partial_validate_backorders_the_remainder_and_never_goes_negative() {
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
        let db = &db; let su = &su; let m = m.clone();
        async move { db.insert_secured(&m, su, &[], &[], v.as_object().unwrap()).await.unwrap() }
    };
    let cur = ins(&currency, json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true })).await;
    let comp = ins(&company, json!({ "name": "Main", "currency_id": cur, "active": true })).await;
    let prod = ins(&product, json!({ "name": "Widget", "list_price": 100.0 })).await;
    let stock = ins(&location, json!({ "name": "Stock", "usage": "internal", "company_id": comp })).await;
    let vendors = ins(&location, json!({ "name": "Vendors", "usage": "supplier", "company_id": comp })).await;
    let customers = ins(&location, json!({ "name": "Customers", "usage": "customer", "company_id": comp })).await;

    // --- 1) Partial receipt: order 7, do 3 → backorder of 4 ---
    let receipt = ins(&picking, json!({ "picking_type": "receipt", "location_id": vendors, "location_dest_id": stock, "company_id": comp })).await;
    let m1 = ins(&mv, json!({ "picking_id": receipt, "product_id": prod, "product_uom_qty": "7", "location_id": vendors, "location_dest_id": stock })).await;
    db.update_secured(&mv, &su, &[], &[], m1, json!({ "quantity_done": "3" }).as_object().unwrap()).await.unwrap();
    db.run_service(&picking, &su, &[], &[], receipt, "validate", serde_json::Map::new()).await.unwrap();

    assert_eq!(quant_at(&db, &su, &quant, prod, stock).await, 3.0, "only the done 3 landed in Stock");
    assert_eq!(db.find_one_secured(&picking, &su, &[], &[], receipt).await.unwrap().unwrap()["state"], "done");
    // A backorder exists for the remaining 4, linked back to the receipt.
    let bos = db.find_secured(&picking, &su, &[], &[], Some(&Domain::field("backorder_id").eq(receipt))).await.unwrap();
    assert_eq!(bos.len(), 1, "one backorder created");
    let bo = &bos[0];
    assert_eq!(bo["state"], "draft", "the backorder is a fresh draft");
    let bo_moves = db.find_secured(&mv, &su, &[], &[], Some(&Domain::field("picking_id").eq(bo["id"].as_i64().unwrap()))).await.unwrap();
    assert_eq!(bo_moves.len(), 1);
    assert_eq!(bo_moves[0]["product_uom_qty"].as_str().unwrap().parse::<f64>().unwrap(), 4.0, "backorder carries the remaining 4");

    // --- 2) Over-delivery guard: on-hand 3, deliver 10 → clamp to 3, never negative, backorder 7 ---
    let delivery = ins(&picking, json!({ "picking_type": "delivery", "location_id": stock, "location_dest_id": customers, "company_id": comp })).await;
    ins(&mv, json!({ "picking_id": delivery, "product_id": prod, "product_uom_qty": "10", "location_id": stock, "location_dest_id": customers })).await;
    db.run_service(&picking, &su, &[], &[], delivery, "validate", serde_json::Map::new()).await.unwrap();

    assert_eq!(quant_at(&db, &su, &quant, prod, stock).await, 0.0, "Stock clamped to 0 — never negative");
    assert_eq!(quant_at(&db, &su, &quant, prod, customers).await, 3.0, "only the 3 on hand shipped");
    let dbo = db.find_secured(&picking, &su, &[], &[], Some(&Domain::field("backorder_id").eq(delivery))).await.unwrap();
    assert_eq!(dbo.len(), 1);
    let dbo_moves = db.find_secured(&mv, &su, &[], &[], Some(&Domain::field("picking_id").eq(dbo[0]["id"].as_i64().unwrap()))).await.unwrap();
    assert_eq!(dbo_moves[0]["product_uom_qty"].as_str().unwrap().parse::<f64>().unwrap(), 7.0, "the un-shippable 7 backordered");
}
