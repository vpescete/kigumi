//! Multi-currency invoicing: a foreign-currency order posts a GL whose lines are in the COMPANY
//! currency (debit/credit) with the invoice amount kept as an amount_currency memo. The receivable is
//! the SUM of the already-rounded parts, never a fresh convert(total), so the entry balances exactly
//! even when per-part rounding would otherwise diverge by a cent. Requires DATABASE_URL.

use meshble::prelude::*;
use meshble_db::Db;
use serde_json::json;

fn link() {
    let _ = (
        &meshble_mod_sales::MANIFEST,
        &meshble_mod_base::MANIFEST,
        &meshble_mod_mail::MANIFEST,
        &meshble_mod_account::MANIFEST,
    );
}

fn money(v: &serde_json::Value, field: &str) -> f64 {
    v[field].as_str().unwrap_or_else(|| panic!("{field} not a string: {v:?}")).parse().unwrap()
}

#[tokio::test]
async fn foreign_invoice_posts_company_currency_lines_that_balance() {
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
    db.ensure_sequence("SO", "SO/", "", 5).await.unwrap();

    let (currency, rate, company, partner, product, order, mv, account, journal) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.currency.rate").unwrap(),
        resolve_registered("res.company").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("sale.order").unwrap(),
        resolve_registered("account.move").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
    );
    let ins = |m: &ResolvedModel, v: serde_json::Value| {
        let db = &db; let su = &su; let m = m.clone();
        async move { db.insert_secured(&m, su, &[], &[], v.as_object().unwrap()).await.unwrap() }
    };

    // Company in EUR (base). Order in USD at 3.0 USD/EUR — a rate chosen so the per-part conversions
    // round to a cent apart from converting the total: 10/3 -> 3.33 twice (sum 6.66), but 20/3 -> 6.67.
    let eur = ins(&currency, json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true })).await;
    let usd = ins(&currency, json!({ "name": "Dollar", "code": "USD", "symbol": "$", "decimal_places": 2, "rounding": 0.01, "position": "before", "active": true })).await;
    let comp = ins(&company, json!({ "name": "Main", "currency_id": eur, "active": true })).await;
    ins(&rate, json!({ "currency_id": usd, "name": "2020-01-01", "rate": "3.0", "company_id": comp })).await;

    ins(&account, json!({ "code": "121000", "name": "Receivable", "account_type": "receivable", "company_id": comp })).await;
    let inc = ins(&account, json!({ "code": "400000", "name": "Sales", "account_type": "income", "company_id": comp })).await;
    let taxacc = ins(&account, json!({ "code": "251000", "name": "Tax", "account_type": "tax", "company_id": comp })).await;
    ins(&journal, json!({ "name": "Customer Invoices", "code": "INV", "journal_type": "sale", "sequence_code": "INV", "company_id": comp })).await;
    let cust = ins(&partner, json!({ "name": "ACME" })).await;
    let prod = ins(&product, json!({ "name": "Widget", "list_price": 10.0 })).await;

    // 1 x 10 USD @ 100% tax → untaxed 10, tax 10, total 20 USD.
    let oid = db.insert_secured(&order, &su, &[], &[], json!({
        "name": "New", "partner_id": cust, "currency_id": usd, "company_id": comp,
        "line_ids": [{ "product_id": prod, "product_uom_qty": 1, "price_unit": 10.0, "tax_rate": "100" }]
    }).as_object().unwrap()).await.unwrap();
    db.run_action(&order, &su, &[], &[], oid, "confirm").await.unwrap();

    let seller = Ctx::new(1, vec!["sales.user".to_string()]).in_companies(comp, vec![comp]);
    let (acls, rules) = (meshble_mod_sales::ACLS, meshble_mod_sales::RECORD_RULES);
    let move_id = db.create_sale_invoice(&seller, acls, rules, oid).await.unwrap();

    let inv = db.find_one_secured(&mv, &su, &[], &[], move_id).await.unwrap().unwrap();
    assert_eq!(inv["state"], "posted");
    assert_eq!(money(&inv, "amount_residual"), 20.0, "residual stays in the invoice currency (USD)");
    assert_eq!(money(&inv, "amount_total_company"), 6.66, "company total = sum of the rounded parts");

    let lines = inv["line_ids"].as_array().unwrap();
    assert_eq!(lines.len(), 3, "income + tax + receivable");
    assert_eq!(lines.iter().map(|l| money(l, "balance")).sum::<f64>(), 0.0, "balances exactly (no rounding drift)");
    // Company-currency debit/credit, with the USD amount kept as the signed memo.
    assert!(lines.iter().any(|l| l["account_id"].as_i64() == Some(inc) && money(l, "credit") == 3.33 && money(l, "amount_currency") == -10.0), "income 3.33 EUR / -10 USD");
    assert!(lines.iter().any(|l| l["account_id"].as_i64() == Some(taxacc) && money(l, "credit") == 3.33 && money(l, "amount_currency") == -10.0), "tax 3.33 EUR / -10 USD");
    assert!(lines.iter().any(|l| money(l, "debit") == 6.66 && money(l, "amount_currency") == 20.0), "receivable 6.66 EUR / +20 USD (sum of parts, not 6.67)");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
