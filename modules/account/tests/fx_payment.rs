//! Multi-currency payment with realized FX: a foreign-currency invoice booked at the invoice-date rate
//! is paid later when the rate has moved. register_payment values the bank movement at the payment-date
//! rate and relieves the receivable at the invoice-date rate; the difference is a balancing FX line, so
//! the company-currency entry still nets to zero. Requires DATABASE_URL.

use meshble::prelude::*;
use meshble_db::Db;
use serde_json::json;

fn link() {
    let _ = (&meshble_mod_account::MANIFEST, &meshble_mod_base::MANIFEST, &meshble_mod_mail::MANIFEST);
}

fn money(v: &serde_json::Value, field: &str) -> f64 {
    v[field].as_str().unwrap_or_else(|| panic!("{field} not a string: {v:?}")).parse().unwrap()
}

#[tokio::test]
async fn payment_books_a_realized_fx_gain() {
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

    let (currency, rate, company, partner, mv, account, journal) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.currency.rate").unwrap(),
        resolve_registered("res.company").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("account.move").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
    );
    let ins = |m: &ResolvedModel, v: serde_json::Value| {
        let db = &db; let su = &su; let m = m.clone();
        async move { db.insert_secured(&m, su, &[], &[], v.as_object().unwrap()).await.unwrap() }
    };

    // Company keeps EUR (the base: no rate rows ⇒ rate 1.0). The invoice is in USD.
    let eur = ins(&currency, json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true })).await;
    let usd = ins(&currency, json!({ "name": "Dollar", "code": "USD", "symbol": "$", "decimal_places": 2, "rounding": 0.01, "position": "before", "active": true })).await;
    let comp = ins(&company, json!({ "name": "Main", "currency_id": eur, "active": true })).await;
    // USD weakens from 2.0 USD/EUR (invoice era) to 1.0 USD/EUR (payment era): 100 USD is worth 50 EUR
    // when invoiced, 100 EUR when paid — a 50 EUR realized gain.
    ins(&rate, json!({ "currency_id": usd, "name": "2020-01-01", "rate": "2.0", "company_id": comp })).await;
    ins(&rate, json!({ "currency_id": usd, "name": "2026-01-01", "rate": "1.0", "company_id": comp })).await;

    let recv = ins(&account, json!({ "code": "121000", "name": "Receivable", "account_type": "receivable", "company_id": comp })).await;
    let inc = ins(&account, json!({ "code": "400000", "name": "Sales", "account_type": "income", "company_id": comp })).await;
    let bank_acc = ins(&account, json!({ "code": "550000", "name": "Bank", "account_type": "bank_cash", "company_id": comp })).await;
    let sale_journal = ins(&journal, json!({ "name": "Customer Invoices", "code": "INV", "journal_type": "sale", "sequence_code": "INV" })).await;
    let bank_journal = ins(&journal, json!({ "name": "Bank", "code": "BNK", "journal_type": "bank", "sequence_code": "BNK", "default_account_id": bank_acc })).await;
    let cust = ins(&partner, json!({ "name": "ACME" })).await;

    // A USD invoice dated 2020-06-01, booked at that era's rate (100 USD = 50 EUR), residual 100 USD.
    let move_id = db.insert_secured(&mv, &su, &[], &[], json!({
        "move_type": "out_invoice", "journal_id": sale_journal, "partner_id": cust, "currency_id": usd,
        "company_id": comp, "date": "2020-06-01", "amount_residual": "100",
        "line_ids": [
            { "account_id": inc, "name": "Goods", "debit": "0", "credit": "50", "amount_currency": "-100", "partner_id": cust, "company_id": comp },
            { "account_id": recv, "name": "Receivable", "debit": "50", "credit": "0", "amount_currency": "100", "partner_id": cust, "company_id": comp }
        ]
    }).as_object().unwrap()).await.unwrap();
    db.post_move(&su, &[], &[], move_id).await.unwrap();

    // Pay the full 100 USD today (rate 1.0 ⇒ worth 100 EUR).
    let acct = Ctx::new(2, vec!["account.user".to_string()]).in_companies(comp, vec![comp]);
    let (a_acls, a_rules) = (meshble_mod_account::ACLS, meshble_mod_account::RECORD_RULES);
    let pay = db.register_payment(&acct, a_acls, a_rules, move_id, "100".parse().unwrap(), bank_journal).await.unwrap();

    let p = db.find_one_secured(&mv, &su, &[], &[], pay).await.unwrap().unwrap();
    assert_eq!(p["state"], "posted");
    let pl = p["line_ids"].as_array().unwrap();
    assert_eq!(pl.len(), 3, "bank + receivable + FX line");
    assert_eq!(pl.iter().map(|l| money(l, "balance")).sum::<f64>(), 0.0, "the company-currency entry balances");
    assert!(pl.iter().any(|l| l["account_id"].as_i64() == Some(bank_acc) && money(l, "debit") == 100.0), "bank debit = 100 EUR at the payment rate");
    assert!(pl.iter().any(|l| l["account_id"].as_i64() == Some(recv) && money(l, "credit") == 50.0), "receivable relieved at the 50 EUR it was booked");
    assert!(pl.iter().any(|l| l["account_id"].as_i64() == Some(inc) && money(l, "credit") == 50.0), "50 EUR realized FX gain to income");

    let after = db.find_one_secured(&mv, &su, &[], &[], move_id).await.unwrap().unwrap();
    assert_eq!(money(&after, "amount_residual"), 0.0, "residual drawn down in invoice currency");
    assert_eq!(after["payment_state"], "paid");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
