//! Reservation at confirm: reserve_picking claims available on-hand for a draft transfer's moves, and
//! validate honors those claims so a later transfer cannot steal stock a reserved one already holds —
//! even when the later transfer validates first. Stock never goes negative; reservations never leak.
//! Requires DATABASE_URL.

use meshble::prelude::*;
use meshble_db::Db;
use serde_json::json;

fn link() {
    let _ = (
        &meshble_mod_stock::MANIFEST,
        &meshble_mod_sales::MANIFEST,
        &meshble_mod_base::MANIFEST,
        &meshble_mod_mail::MANIFEST,
    );
}

async fn on_hand(db: &Db, su: &Ctx, product: &ResolvedModel, id: i64) -> f64 {
    db.find_one_secured(product, su, &[], &[], id).await.unwrap().unwrap()["qty_available"]
        .as_str().and_then(|s| s.parse().ok()).unwrap()
}

/// A field of the (product, location) quant, or 0.0 if no quant exists.
async fn quant_field(db: &Db, su: &Ctx, quant: &ResolvedModel, product: i64, location: i64, field: &str) -> f64 {
    let dom = Domain::field("product_id").eq(product).and(Domain::field("location_id").eq(location));
    match db.find_secured(quant, su, &[], &[], Some(&dom)).await.unwrap().first() {
        Some(q) => q[field].as_str().and_then(|s| s.parse().ok()).unwrap(),
        None => 0.0,
    }
}

#[tokio::test]
async fn reservation_protects_the_first_transfer() {
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

    // Receive 10 widgets into Stock.
    let receipt = ins(&picking, json!({ "picking_type": "receipt", "location_id": vendors, "location_dest_id": stock, "company_id": comp })).await;
    ins(&mv, json!({ "picking_id": receipt, "product_id": prod, "product_uom_qty": "10", "location_id": vendors, "location_dest_id": stock })).await;
    db.validate_picking(&su, &[], &[], receipt).await.unwrap();
    assert_eq!(on_hand(&db, &su, &product, prod).await, 10.0);

    // Two deliveries each demand 7 (combined 14 > 10 on hand).
    let del_a = ins(&picking, json!({ "picking_type": "delivery", "location_id": stock, "location_dest_id": customers, "company_id": comp })).await;
    let mv_a = ins(&mv, json!({ "picking_id": del_a, "product_id": prod, "product_uom_qty": "7", "location_id": stock, "location_dest_id": customers })).await;
    let del_b = ins(&picking, json!({ "picking_type": "delivery", "location_id": stock, "location_dest_id": customers, "company_id": comp })).await;
    let mv_b = ins(&mv, json!({ "picking_id": del_b, "product_id": prod, "product_uom_qty": "7", "location_id": stock, "location_dest_id": customers })).await;

    // A reserves first → gets its full 7. B reserves next → only 3 left.
    assert_eq!(db.reserve_picking(&su, &[], &[], del_a).await.unwrap(), 1, "A reserves one move");
    assert_eq!(quant_field(&db, &su, &quant, prod, stock, "reserved_quantity").await, 7.0, "Stock has 7 reserved after A");
    assert_eq!(db.reserve_picking(&su, &[], &[], del_b).await.unwrap(), 1, "B reserves one move");
    assert_eq!(quant_field(&db, &su, &quant, prod, stock, "reserved_quantity").await, 10.0, "Stock fully reserved after B");

    let mv_a_reserved = db.find_one_secured(&mv, &su, &[], &[], mv_a).await.unwrap().unwrap()["reserved_qty"].as_str().unwrap().parse::<f64>().unwrap();
    let mv_b_reserved = db.find_one_secured(&mv, &su, &[], &[], mv_b).await.unwrap().unwrap()["reserved_qty"].as_str().unwrap().parse::<f64>().unwrap();
    assert_eq!(mv_a_reserved, 7.0, "move A holds 7");
    assert_eq!(mv_b_reserved, 3.0, "move B holds only the remaining 3");

    // Re-reserving A is a no-op (already at its demand): idempotent.
    assert_eq!(db.reserve_picking(&su, &[], &[], del_a).await.unwrap(), 0, "re-reserve grants nothing new");
    assert_eq!(quant_field(&db, &su, &quant, prod, stock, "reserved_quantity").await, 10.0, "reservation unchanged");

    // B validates FIRST. Without reservation it would grab 7 and starve A; with it, B gets only its 3.
    db.validate_picking(&su, &[], &[], del_b).await.unwrap();
    assert_eq!(on_hand(&db, &su, &product, prod).await, 7.0, "B shipped only 3 -> 7 left");
    assert_eq!(quant_field(&db, &su, &quant, prod, stock, "reserved_quantity").await, 7.0, "A's 7 reservation survives B");

    // A validates and gets its full reserved 7.
    db.validate_picking(&su, &[], &[], del_a).await.unwrap();
    assert_eq!(on_hand(&db, &su, &product, prod).await, 0.0, "A shipped its full 7 -> 0 left");
    assert_eq!(quant_field(&db, &su, &quant, prod, stock, "reserved_quantity").await, 0.0, "no reservation leaks");
    assert_eq!(quant_field(&db, &su, &quant, prod, stock, "quantity").await, 0.0, "stock never went negative");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
