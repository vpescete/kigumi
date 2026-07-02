//! Lot/serial tracking: stock is held per (product, location, lot); on-hand sums across lots. A serial
//! is exactly one unit and must carry its serial number. Requires DATABASE_URL.

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

/// Quantity of the (product, location, lot) quant, or 0.0 if none.
async fn lot_qty(db: &Db, su: &Ctx, quant: &ResolvedModel, product: i64, location: i64, lot: i64) -> f64 {
    let dom = Domain::field("product_id").eq(product)
        .and(Domain::field("location_id").eq(location))
        .and(Domain::field("lot_id").eq(lot));
    match db.find_secured(quant, su, &[], &[], Some(&dom)).await.unwrap().first() {
        Some(q) => q["quantity"].as_str().and_then(|s| s.parse().ok()).unwrap(),
        None => 0.0,
    }
}

#[tokio::test]
async fn stock_is_tracked_per_lot_and_serial() {
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
    db.ensure_stock_indexes().await.unwrap();
    db.ensure_sequence_schema().await.unwrap();

    let (currency, company, product, template, location, picking, mv, quant, lot) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.company").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("product.template").unwrap(),
        resolve_registered("stock.location").unwrap(),
        resolve_registered("stock.picking").unwrap(),
        resolve_registered("stock.move").unwrap(),
        resolve_registered("stock.quant").unwrap(),
        resolve_registered("stock.lot").unwrap(),
    );
    let ins = |m: &ResolvedModel, v: serde_json::Value| {
        let db = &db; let su = &su; let m = m.clone();
        async move { db.insert_secured(&m, su, &[], &[], v.as_object().unwrap()).await.unwrap() }
    };

    let cur = ins(&currency, json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true })).await;
    let comp = ins(&company, json!({ "name": "Main", "currency_id": cur, "active": true })).await;
    let stock = ins(&location, json!({ "name": "Stock", "usage": "internal", "company_id": comp })).await;
    let vendors = ins(&location, json!({ "name": "Vendors", "usage": "supplier", "company_id": comp })).await;
    let customers = ins(&location, json!({ "name": "Customers", "usage": "customer", "company_id": comp })).await;

    // ---- Lot tracking ----
    let prod = ins(&product, json!({ "name": "Tracked Widget", "list_price": 100.0 })).await;
    let tmpl = db.find_one_secured(&product, &su, &[], &[], prod).await.unwrap().unwrap()["product_tmpl_id"].as_i64().unwrap();
    db.update_secured(&template, &su, &[], &[], tmpl, json!({ "tracking": "lot" }).as_object().unwrap()).await.unwrap();
    let lot_a = ins(&lot, json!({ "name": "LOT-A", "product_id": prod, "company_id": comp })).await;
    let lot_b = ins(&lot, json!({ "name": "LOT-B", "product_id": prod, "company_id": comp })).await;

    // Receive 10 of LOT-A and 5 of LOT-B in one transfer.
    let receipt = ins(&picking, json!({ "picking_type": "receipt", "location_id": vendors, "location_dest_id": stock, "company_id": comp })).await;
    ins(&mv, json!({ "picking_id": receipt, "product_id": prod, "product_uom_qty": "10", "lot_id": lot_a, "location_id": vendors, "location_dest_id": stock })).await;
    ins(&mv, json!({ "picking_id": receipt, "product_id": prod, "product_uom_qty": "5", "lot_id": lot_b, "location_id": vendors, "location_dest_id": stock })).await;
    db.run_service(&picking, &su, &[], &[], receipt, "validate", serde_json::Map::new()).await.unwrap();
    assert_eq!(lot_qty(&db, &su, &quant, prod, stock, lot_a).await, 10.0, "LOT-A quant is 10");
    assert_eq!(lot_qty(&db, &su, &quant, prod, stock, lot_b).await, 5.0, "LOT-B quant is 5");
    assert_eq!(on_hand(&db, &su, &product, prod).await, 15.0, "on-hand sums across lots");

    // Deliver 3 of LOT-A: only LOT-A drops.
    let deliver = ins(&picking, json!({ "picking_type": "delivery", "location_id": stock, "location_dest_id": customers, "company_id": comp })).await;
    ins(&mv, json!({ "picking_id": deliver, "product_id": prod, "product_uom_qty": "3", "lot_id": lot_a, "location_id": stock, "location_dest_id": customers })).await;
    db.run_service(&picking, &su, &[], &[], deliver, "validate", serde_json::Map::new()).await.unwrap();
    assert_eq!(lot_qty(&db, &su, &quant, prod, stock, lot_a).await, 7.0, "LOT-A dropped to 7");
    assert_eq!(lot_qty(&db, &su, &quant, prod, stock, lot_b).await, 5.0, "LOT-B untouched");
    assert_eq!(on_hand(&db, &su, &product, prod).await, 12.0);

    // ---- Serial tracking ----
    let serprod = ins(&product, json!({ "name": "Serial Gadget", "list_price": 50.0 })).await;
    let stmpl = db.find_one_secured(&product, &su, &[], &[], serprod).await.unwrap().unwrap()["product_tmpl_id"].as_i64().unwrap();
    db.update_secured(&template, &su, &[], &[], stmpl, json!({ "tracking": "serial" }).as_object().unwrap()).await.unwrap();
    let ser1 = ins(&lot, json!({ "name": "SER-1", "product_id": serprod, "company_id": comp })).await;

    // A valid serial receipt: exactly one unit, with its serial.
    let sin = ins(&picking, json!({ "picking_type": "receipt", "location_id": vendors, "location_dest_id": stock, "company_id": comp })).await;
    ins(&mv, json!({ "picking_id": sin, "product_id": serprod, "product_uom_qty": "1", "lot_id": ser1, "location_id": vendors, "location_dest_id": stock })).await;
    db.run_service(&picking, &su, &[], &[], sin, "validate", serde_json::Map::new()).await.unwrap();
    assert_eq!(on_hand(&db, &su, &product, serprod).await, 1.0);

    // A serial move of 2 units is rejected (a serial is exactly one).
    let bad_qty = ins(&picking, json!({ "picking_type": "receipt", "location_id": vendors, "location_dest_id": stock, "company_id": comp })).await;
    ins(&mv, json!({ "picking_id": bad_qty, "product_id": serprod, "product_uom_qty": "2", "lot_id": ser1, "location_id": vendors, "location_dest_id": stock })).await;
    assert!(db.run_service(&picking, &su, &[], &[], bad_qty, "validate", serde_json::Map::new()).await.is_err(), "a serial move of 2 is rejected");

    // A serial move with no serial number is rejected.
    let no_lot = ins(&picking, json!({ "picking_type": "receipt", "location_id": vendors, "location_dest_id": stock, "company_id": comp })).await;
    ins(&mv, json!({ "picking_id": no_lot, "product_id": serprod, "product_uom_qty": "1", "location_id": vendors, "location_dest_id": stock })).await;
    assert!(db.run_service(&picking, &su, &[], &[], no_lot, "validate", serde_json::Map::new()).await.is_err(), "a serial move without a serial is rejected");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
