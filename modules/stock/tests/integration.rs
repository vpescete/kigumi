//! M17.3: order → transfer integration on a real database. A confirmed purchase order creates a
//! receipt (Vendors → Stock) that raises on-hand; a confirmed sale order creates a delivery
//! (Stock → Customers) that lowers it. Draft orders and order-with-no-lines are rejected.
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

#[tokio::test]
async fn orders_create_transfers_that_move_stock() {
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

    let m = |n: &str| resolve_registered(n).unwrap();
    let (currency, company, partner, product, location, picking, mv) =
        (m("res.currency"), m("res.company"), m("res.partner"), m("product.product"),
         m("stock.location"), m("stock.picking"), m("stock.move"));
    let so = m("sale.order"); let sol = m("sale.order.line");
    let po = m("purchase.order"); let pol = m("purchase.order.line");

    let ins = |mdl: &ResolvedModel, v: serde_json::Value| {
        let db = &db; let su = &su; let mdl = mdl.clone();
        async move { db.insert_secured(&mdl, su, &[], &[], v.as_object().unwrap()).await.unwrap() }
    };

    let cur = ins(&currency, json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true })).await;
    let comp = ins(&company, json!({ "name": "Main", "currency_id": cur, "active": true })).await;
    let acme = ins(&partner, json!({ "name": "Acme" })).await;
    let prod = ins(&product, json!({ "name": "Widget", "list_price": 100.0 })).await;
    let stock = ins(&location, json!({ "name": "Stock", "usage": "internal", "company_id": comp })).await;
    let vendors = ins(&location, json!({ "name": "Vendors", "usage": "supplier", "company_id": comp })).await;
    let customers = ins(&location, json!({ "name": "Customers", "usage": "customer", "company_id": comp })).await;

    // -- Purchase → receipt (Vendors → Stock), 9 units --
    let porder = ins(&po, json!({ "partner_id": acme, "company_id": comp, "currency_id": cur, "state": "purchase" })).await;
    ins(&pol, json!({ "order_id": porder, "product_id": prod, "product_uom_qty": "9" })).await;
    let receipt = db.create_receipt(&su, &[], &[], porder).await.unwrap();

    let rp = db.find_one_secured(&picking, &su, &[], &[], receipt).await.unwrap().unwrap();
    assert_eq!(rp["picking_type"], "receipt");
    assert_eq!(rp["location_id"].as_i64(), Some(vendors));
    assert_eq!(rp["location_dest_id"].as_i64(), Some(stock));
    assert_eq!(rp["state"], "draft");
    assert_eq!(rp["partner_id"].as_i64(), Some(acme));
    let rmoves = db.find_secured(&mv, &su, &[], &[], Some(&Domain::field("picking_id").eq(receipt))).await.unwrap();
    assert_eq!(rmoves.len(), 1, "one move per goods line");
    assert_eq!(rmoves[0]["product_id"].as_i64(), Some(prod));
    assert_eq!(rmoves[0]["product_uom_qty"].as_str().and_then(|s| s.parse::<f64>().ok()), Some(9.0));

    db.run_service(&picking, &su, &[], &[], receipt, "validate", serde_json::Map::new()).await.unwrap();
    assert_eq!(on_hand(&db, &su, &product, prod).await, 9.0, "receipt raises on-hand to 9");

    // -- Sale → delivery (Stock → Customers), 4 units --
    let sorder = ins(&so, json!({ "partner_id": acme, "company_id": comp, "currency_id": cur, "state": "sale" })).await;
    ins(&sol, json!({ "order_id": sorder, "product_id": prod, "product_uom_qty": "4", "price_unit": "100" })).await;
    let delivery = db.create_delivery(&su, &[], &[], sorder).await.unwrap();

    let dp = db.find_one_secured(&picking, &su, &[], &[], delivery).await.unwrap().unwrap();
    assert_eq!(dp["picking_type"], "delivery");
    assert_eq!(dp["location_id"].as_i64(), Some(stock));
    assert_eq!(dp["location_dest_id"].as_i64(), Some(customers));
    let dmoves = db.find_secured(&mv, &su, &[], &[], Some(&Domain::field("picking_id").eq(delivery))).await.unwrap();
    assert_eq!(dmoves.len(), 1);
    assert_eq!(dmoves[0]["product_uom_qty"].as_str().and_then(|s| s.parse::<f64>().ok()), Some(4.0));

    db.run_service(&picking, &su, &[], &[], delivery, "validate", serde_json::Map::new()).await.unwrap();
    assert_eq!(on_hand(&db, &su, &product, prod).await, 5.0, "delivery lowers on-hand to 5");

    // -- A draft (unconfirmed) order cannot create a transfer --
    let draft = ins(&so, json!({ "partner_id": acme, "company_id": comp, "currency_id": cur, "state": "draft" })).await;
    ins(&sol, json!({ "order_id": draft, "product_id": prod, "product_uom_qty": "1", "price_unit": "1" })).await;
    assert!(db.create_delivery(&su, &[], &[], draft).await.is_err(), "a draft order cannot be delivered");

    // -- A confirmed order with no goods lines cannot create a transfer --
    let empty = ins(&so, json!({ "partner_id": acme, "company_id": comp, "currency_id": cur, "state": "sale" })).await;
    assert!(db.create_delivery(&su, &[], &[], empty).await.is_err(), "an order with no lines has nothing to deliver");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
