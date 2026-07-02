//! Buy-side tax engine: a purchase line carries a Many2many tax set; apply_purchase_taxes materializes
//! the per-tax breakdown (and remaps via the order's fiscal position), and create_vendor_bill rolls it up
//! into one balanced GL DEBIT line per tax group. Mirror of the sale side. Requires DATABASE_URL.

use kigumi::prelude::*;
use kigumi_db::Db;
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
async fn purchase_taxes_roll_up_per_group_into_the_vendor_bill() {
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
    db.ensure_sequence_schema().await.unwrap();
    db.ensure_sequence("PO", "PO/", "", 5).await.unwrap();

    let (currency, partner, product, order, line, mv, account, journal, tax, group, fpos, fpostax) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("purchase.order").unwrap(),
        resolve_registered("purchase.order.line").unwrap(),
        resolve_registered("account.move").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
        resolve_registered("account.tax").unwrap(),
        resolve_registered("account.tax.group").unwrap(),
        resolve_registered("account.fiscal.position").unwrap(),
        resolve_registered("account.fiscal.position.tax").unwrap(),
    );
    let ins = |m: &ResolvedModel, v: serde_json::Value| {
        let db = &db; let su = &su; let m = m.clone();
        async move { db.insert_secured(&m, su, &[], &[], v.as_object().unwrap()).await.unwrap() }
    };

    let cur = ins(&currency, json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true })).await;
    let payable = ins(&account, json!({ "code": "211000", "name": "Payable", "account_type": "payable" })).await;
    let expense = ins(&account, json!({ "code": "600000", "name": "Expenses", "account_type": "expense" })).await;
    let taxacc = ins(&account, json!({ "code": "251000", "name": "Tax", "account_type": "tax" })).await;
    ins(&journal, json!({ "name": "Vendor Bills", "code": "BILL", "journal_type": "purchase", "sequence_code": "BILL" })).await;
    let g_vat = ins(&group, json!({ "name": "VAT", "sequence": 10 })).await;
    let g_eco = ins(&group, json!({ "name": "Eco", "sequence": 20 })).await;
    let vat22 = ins(&tax, json!({ "name": "VAT 22%", "amount_type": "percent", "amount": "22", "tax_group_id": g_vat, "active": true })).await;
    let vat10 = ins(&tax, json!({ "name": "VAT 10%", "amount_type": "percent", "amount": "10", "tax_group_id": g_vat, "active": true })).await;
    let eco5 = ins(&tax, json!({ "name": "Eco 5/unit", "amount_type": "fixed", "amount": "5", "tax_group_id": g_eco, "active": true })).await;
    let vendor = ins(&partner, json!({ "name": "ACME Supply" })).await;
    let prod = ins(&product, json!({ "name": "Raw part", "list_price": 100.0 })).await;

    // PO with two lines: one taxed by VAT, one by the Eco fee.
    let oid = db.insert_secured(&order, &su, &[], &[], json!({ "name": "New", "partner_id": vendor, "currency_id": cur }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&line, &su, &[], &[], json!({ "order_id": oid, "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0, "tax_ids": [vat22] }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&line, &su, &[], &[], json!({ "order_id": oid, "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0, "tax_ids": [eco5] }).as_object().unwrap()).await.unwrap();

    let buyer = Ctx::new(1, vec!["sales.user".to_string()]);
    let (acls, rules) = (kigumi_mod_sales::ACLS, kigumi_mod_sales::RECORD_RULES);
    db.run_service(&order, &buyer, acls, rules, oid, "apply_purchase_taxes", serde_json::Map::new()).await.unwrap();
    db.run_action(&order, &su, &[], &[], oid, "confirm").await.unwrap();
    let confirmed = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(money(&confirmed, "amount_untaxed"), 200.0);
    assert_eq!(money(&confirmed, "amount_tax"), 27.0, "22 VAT + 5 Eco");

    let move_id = db.run_service(&order, &buyer, acls, rules, oid, "create_vendor_bill", serde_json::Map::new()).await.unwrap()["bill"].as_i64().unwrap();
    let bill = db.find_one_secured(&mv, &su, &[], &[], move_id).await.unwrap().unwrap();
    assert_eq!(bill["state"], "posted");
    let lines = bill["line_ids"].as_array().unwrap();
    assert_eq!(lines.len(), 4, "expense + one tax line per group + payable");
    assert_eq!(lines.iter().map(|l| money(l, "balance")).sum::<f64>(), 0.0, "balanced");
    assert!(lines.iter().any(|l| l["account_id"].as_i64() == Some(expense) && money(l, "debit") == 200.0));
    assert!(lines.iter().any(|l| l["account_id"].as_i64() == Some(payable) && money(l, "credit") == 227.0));
    let tax_lines: Vec<_> = lines.iter().filter(|l| l["account_id"].as_i64() == Some(taxacc)).collect();
    assert_eq!(tax_lines.len(), 2, "one GL tax line per group");
    assert!(tax_lines.iter().any(|l| money(l, "debit") == 22.0 && l["name"] == "VAT"));
    assert!(tax_lines.iter().any(|l| money(l, "debit") == 5.0 && l["name"] == "Eco"));

    // Fiscal position on the buy side: remap VAT 22% -> VAT 10% on a fresh PO. tax_ids unchanged.
    let pos = ins(&fpos, json!({ "name": "Reduced", "active": true })).await;
    ins(&fpostax, json!({ "position_id": pos, "tax_src_id": vat22, "tax_dest_id": vat10 })).await;
    let o2 = db.insert_secured(&order, &su, &[], &[], json!({ "name": "New", "partner_id": vendor, "currency_id": cur, "fiscal_position_id": pos }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&line, &su, &[], &[], json!({ "order_id": o2, "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0, "tax_ids": [vat22] }).as_object().unwrap()).await.unwrap();
    db.run_service(&order, &buyer, acls, rules, o2, "apply_purchase_taxes", serde_json::Map::new()).await.unwrap();
    let mapped = db.find_one_secured(&order, &su, &[], &[], o2).await.unwrap().unwrap();
    assert_eq!(money(&mapped, "amount_tax"), 10.0, "VAT remapped 22% -> 10%");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
