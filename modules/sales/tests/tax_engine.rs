//! The full tax engine through apply_taxes: a line carries a Many2many tax set; apply_taxes runs the
//! engine and materializes one sale.order.line.tax breakdown row per tax, which the line/order computes
//! roll up. Covers multiple taxes with distinct groups, idempotency, and fiscal-position remap + drop.
//! Requires DATABASE_URL.

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
async fn apply_taxes_materializes_a_multi_tax_breakdown() {
    link();
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let su = kigumi_test::su();

    let (currency, partner, product, order, line, tax, group, fpos, fpostax, breakdown) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("sale.order").unwrap(),
        resolve_registered("sale.order.line").unwrap(),
        resolve_registered("account.tax").unwrap(),
        resolve_registered("account.tax.group").unwrap(),
        resolve_registered("account.fiscal.position").unwrap(),
        resolve_registered("account.fiscal.position.tax").unwrap(),
        resolve_registered("sale.order.line.tax").unwrap(),
    );
    let ins = |m: &ResolvedModel, v: serde_json::Value| {
        let db = &db; let su = &su; let m = m.clone();
        async move { db.insert_secured(&m, su, &[], &[], v.as_object().unwrap()).await.unwrap() }
    };

    let cur = ins(&currency, json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true })).await;
    let g_vat = ins(&group, json!({ "name": "VAT", "sequence": 10 })).await;
    let g_eco = ins(&group, json!({ "name": "Eco", "sequence": 20 })).await;
    let vat22 = ins(&tax, json!({ "name": "VAT 22%", "amount_type": "percent", "amount": "22", "sequence": 20, "tax_group_id": g_vat, "active": true })).await;
    let eco5 = ins(&tax, json!({ "name": "Eco 5/unit", "amount_type": "fixed", "amount": "5", "sequence": 10, "tax_group_id": g_eco, "active": true })).await;
    let vat10 = ins(&tax, json!({ "name": "VAT 10%", "amount_type": "percent", "amount": "10", "sequence": 20, "tax_group_id": g_vat, "active": true })).await;
    let cust = ins(&partner, json!({ "name": "ACME" })).await;
    let prod = ins(&product, json!({ "name": "Widget", "list_price": 100.0 })).await;

    // A line carrying BOTH taxes via the Many2many set (inserted top-level so the M2M is applied).
    let oid = db.insert_secured(&order, &su, &[], &[], json!({
        "name": "New", "partner_id": cust, "currency_id": cur
    }).as_object().unwrap()).await.unwrap();
    let lid = db.insert_secured(&line, &su, &[], &[], json!({
        "order_id": oid, "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0, "tax_ids": [vat22, eco5]
    }).as_object().unwrap()).await.unwrap();

    let seller = Ctx::new(1, vec!["sales.user".to_string()]);
    let (acls, rules) = (kigumi_mod_sales::ACLS, kigumi_mod_sales::RECORD_RULES);

    db.run_service(&order, &seller, acls, rules, oid, "apply_taxes", serde_json::Map::new()).await.unwrap();
    let after = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(money(&after, "amount_untaxed"), 100.0);
    assert_eq!(money(&after, "amount_tax"), 27.0, "22% (22) + fixed 5 = 27");
    assert_eq!(money(&after, "amount_total"), 127.0);

    let rows = db.find_secured(&breakdown, &su, &[], &[], Some(&Domain::field("line_id").eq(lid))).await.unwrap();
    assert_eq!(rows.len(), 2, "one breakdown row per tax");
    assert!(rows.iter().any(|r| r["tax_id"].as_i64() == Some(vat22) && money(r, "tax_amount") == 22.0 && r["tax_group_id"].as_i64() == Some(g_vat)));
    assert!(rows.iter().any(|r| r["tax_id"].as_i64() == Some(eco5) && money(r, "tax_amount") == 5.0 && r["tax_group_id"].as_i64() == Some(g_eco)));

    // Idempotent: a second run re-derives the same two rows and totals (never doubles).
    db.run_service(&order, &seller, acls, rules, oid, "apply_taxes", serde_json::Map::new()).await.unwrap();
    let again = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(money(&again, "amount_tax"), 27.0, "re-running apply_taxes is idempotent");
    assert_eq!(db.find_secured(&breakdown, &su, &[], &[], Some(&Domain::field("line_id").eq(lid))).await.unwrap().len(), 2);

    // Fiscal position: remap VAT 22% -> VAT 10% (Eco passes through). tax_ids stays the original set.
    let pos = ins(&fpos, json!({ "name": "Reduced", "active": true })).await;
    ins(&fpostax, json!({ "position_id": pos, "tax_src_id": vat22, "tax_dest_id": vat10 })).await;
    db.update_secured(&order, &su, &[], &[], oid, json!({ "fiscal_position_id": pos }).as_object().unwrap()).await.unwrap();
    db.run_service(&order, &seller, acls, rules, oid, "apply_taxes", serde_json::Map::new()).await.unwrap();
    let mapped = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(money(&mapped, "amount_tax"), 15.0, "VAT remapped to 10% (10) + Eco 5 = 15");
    assert_eq!(money(&mapped, "amount_total"), 115.0);
    let mrows = db.find_secured(&breakdown, &su, &[], &[], Some(&Domain::field("line_id").eq(lid))).await.unwrap();
    assert!(mrows.iter().any(|r| r["tax_id"].as_i64() == Some(vat10)), "breakdown now shows the destination tax");
    assert!(!mrows.iter().any(|r| r["tax_id"].as_i64() == Some(vat22)), "source tax no longer in the breakdown");

    // Fiscal drop: remap VAT 22% -> NULL (removed). Only Eco remains.
    let exp = ins(&fpos, json!({ "name": "Export", "active": true })).await;
    ins(&fpostax, json!({ "position_id": exp, "tax_src_id": vat22 })).await;
    db.update_secured(&order, &su, &[], &[], oid, json!({ "fiscal_position_id": exp }).as_object().unwrap()).await.unwrap();
    db.run_service(&order, &seller, acls, rules, oid, "apply_taxes", serde_json::Map::new()).await.unwrap();
    let dropped = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(money(&dropped, "amount_tax"), 5.0, "VAT dropped, only the fixed Eco remains");
    assert_eq!(money(&dropped, "amount_total"), 105.0);
}
