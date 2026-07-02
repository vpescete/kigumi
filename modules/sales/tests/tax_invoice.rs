//! The invoice GL rolls the per-line tax breakdown up into one balanced credit line PER tax group (not a
//! single lumped tax line), so an invoice with VAT + an Eco fee posts two distinct tax lines. The move
//! still balances (receivable = untaxed + Σ per-group tax). Requires DATABASE_URL.

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
async fn invoice_posts_one_tax_line_per_group() {
    link();
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let su = kigumi_test::su();
    db.ensure_sequence("SO", "SO/", "", 5).await.unwrap();

    let (currency, partner, product, order, line, mv, account, journal, tax, group) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("sale.order").unwrap(),
        resolve_registered("sale.order.line").unwrap(),
        resolve_registered("account.move").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
        resolve_registered("account.tax").unwrap(),
        resolve_registered("account.tax.group").unwrap(),
    );
    let ins = |m: &ResolvedModel, v: serde_json::Value| {
        let db = &db; let su = &su; let m = m.clone();
        async move { db.insert_secured(&m, su, &[], &[], v.as_object().unwrap()).await.unwrap() }
    };

    let cur = ins(&currency, json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true })).await;
    let recv = ins(&account, json!({ "code": "121000", "name": "Receivable", "account_type": "receivable" })).await;
    let inc = ins(&account, json!({ "code": "400000", "name": "Sales", "account_type": "income" })).await;
    let taxacc = ins(&account, json!({ "code": "251000", "name": "Tax", "account_type": "tax" })).await;
    ins(&journal, json!({ "name": "Customer Invoices", "code": "INV", "journal_type": "sale", "sequence_code": "INV" })).await;
    let g_vat = ins(&group, json!({ "name": "VAT", "sequence": 10 })).await;
    let g_eco = ins(&group, json!({ "name": "Eco", "sequence": 20 })).await;
    let vat22 = ins(&tax, json!({ "name": "VAT 22%", "amount_type": "percent", "amount": "22", "tax_group_id": g_vat, "active": true })).await;
    let eco5 = ins(&tax, json!({ "name": "Eco 5/unit", "amount_type": "fixed", "amount": "5", "tax_group_id": g_eco, "active": true })).await;
    let cust = ins(&partner, json!({ "name": "ACME" })).await;
    let prod = ins(&product, json!({ "name": "Widget", "list_price": 100.0 })).await;

    // Two lines: one taxed by VAT, one by the Eco fee — distinct tax groups.
    let oid = db.insert_secured(&order, &su, &[], &[], json!({ "name": "New", "partner_id": cust, "currency_id": cur }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&line, &su, &[], &[], json!({ "order_id": oid, "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0, "tax_ids": [vat22] }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&line, &su, &[], &[], json!({ "order_id": oid, "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0, "tax_ids": [eco5] }).as_object().unwrap()).await.unwrap();

    let seller = Ctx::new(1, vec!["sales.user".to_string()]);
    let (acls, rules) = (kigumi_mod_sales::ACLS, kigumi_mod_sales::RECORD_RULES);
    db.run_service(&order, &seller, acls, rules, oid, "apply_taxes", serde_json::Map::new()).await.unwrap();
    db.run_action(&order, &su, &[], &[], oid, "confirm").await.unwrap();
    let confirmed = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(money(&confirmed, "amount_untaxed"), 200.0);
    assert_eq!(money(&confirmed, "amount_tax"), 27.0, "22 VAT + 5 Eco");

    let move_id = db.run_service(&order, &seller, acls, rules, oid, "create_invoice", serde_json::Map::new()).await.unwrap()["invoice"].as_i64().unwrap();
    let inv = db.find_one_secured(&mv, &su, &[], &[], move_id).await.unwrap().unwrap();
    assert_eq!(inv["state"], "posted");
    let lines = inv["line_ids"].as_array().unwrap();
    // income(200) + VAT(22) + Eco(5) + receivable(227) = four lines, two of them to the tax account.
    assert_eq!(lines.len(), 4, "income + one tax line per group + receivable");
    assert_eq!(lines.iter().map(|l| money(l, "balance")).sum::<f64>(), 0.0, "balanced");
    assert!(lines.iter().any(|l| l["account_id"].as_i64() == Some(inc) && money(l, "credit") == 200.0));
    assert!(lines.iter().any(|l| l["account_id"].as_i64() == Some(recv) && money(l, "debit") == 227.0));
    let tax_lines: Vec<_> = lines.iter().filter(|l| l["account_id"].as_i64() == Some(taxacc)).collect();
    assert_eq!(tax_lines.len(), 2, "one GL tax line per group");
    assert!(tax_lines.iter().any(|l| money(l, "credit") == 22.0 && l["name"] == "VAT"));
    assert!(tax_lines.iter().any(|l| money(l, "credit") == 5.0 && l["name"] == "Eco"));

}
