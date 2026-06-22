//! Payments + reconciliation: a posted invoice carries an open `amount_residual`; register_payment
//! draws it down, books a balanced bank/receivable entry, and flips payment_state not_paid -> partial
//! -> paid (reconciled). Requires DATABASE_URL.

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
async fn register_payment_draws_down_the_residual() {
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

    let (currency, partner, mv, account, journal) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("account.move").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({
        "name": "Euro", "code": "EUR", "symbol": "€", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
    }).as_object().unwrap()).await.unwrap();
    let recv = db.insert_secured(&account, &su, &[], &[], json!({ "code": "121000", "name": "Receivable", "account_type": "receivable" }).as_object().unwrap()).await.unwrap();
    let inc = db.insert_secured(&account, &su, &[], &[], json!({ "code": "400000", "name": "Sales", "account_type": "income" }).as_object().unwrap()).await.unwrap();
    let bank_acc = db.insert_secured(&account, &su, &[], &[], json!({ "code": "550000", "name": "Bank", "account_type": "bank_cash" }).as_object().unwrap()).await.unwrap();
    let sale_journal = db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Customer Invoices", "code": "INV", "journal_type": "sale", "sequence_code": "INV" }).as_object().unwrap()).await.unwrap();
    let bank_journal = db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Bank", "code": "BNK", "journal_type": "bank", "sequence_code": "BNK", "default_account_id": bank_acc }).as_object().unwrap()).await.unwrap();
    let cust = db.insert_secured(&partner, &su, &[], &[], json!({ "name": "ACME" }).as_object().unwrap()).await.unwrap();

    // A balanced customer invoice for 100 (income credit 100 / receivable debit 100), residual seeded.
    let move_id = db.insert_secured(&mv, &su, &[], &[], json!({
        "move_type": "out_invoice", "journal_id": sale_journal, "partner_id": cust, "currency_id": cur,
        "amount_residual": "100",
        "line_ids": [
            { "account_id": inc, "name": "Goods", "debit": "0", "credit": "100", "partner_id": cust },
            { "account_id": recv, "name": "Receivable", "debit": "100", "credit": "0", "partner_id": cust }
        ]
    }).as_object().unwrap()).await.unwrap();
    db.post_move(&su, &[], &[], move_id).await.unwrap();

    let inv = db.find_one_secured(&mv, &su, &[], &[], move_id).await.unwrap().unwrap();
    assert_eq!(inv["state"], "posted");
    assert_eq!(money(&inv, "amount_residual"), 100.0, "residual seeded = total");
    assert_eq!(inv["payment_state"], "not_paid");

    // Pay as an accountant (gates on account.move write). First a partial 40.
    let acct = Ctx::new(2, vec!["account.user".to_string()]);
    let (a_acls, a_rules) = (meshble_mod_account::ACLS, meshble_mod_account::RECORD_RULES);
    let pay1 = db.register_payment(&acct, a_acls, a_rules, move_id, "40".parse().unwrap(), bank_journal).await.unwrap();

    let p1 = db.find_one_secured(&mv, &su, &[], &[], pay1).await.unwrap().unwrap();
    assert_eq!(p1["state"], "posted", "payment is posted");
    assert!(p1["name"].as_str().unwrap().starts_with("BNK/"), "numbered from the bank journal");
    let pl = p1["line_ids"].as_array().unwrap();
    assert_eq!(pl.len(), 2);
    assert_eq!(pl.iter().map(|l| money(l, "balance")).sum::<f64>(), 0.0, "payment is balanced");
    assert!(pl.iter().any(|l| l["account_id"].as_i64() == Some(bank_acc) && money(l, "debit") == 40.0), "bank debit 40");

    let after1 = db.find_one_secured(&mv, &su, &[], &[], move_id).await.unwrap().unwrap();
    assert_eq!(money(&after1, "amount_residual"), 60.0);
    assert_eq!(after1["payment_state"], "partial");
    assert_eq!(after1["reconciled"], false);

    // Settle the rest.
    db.register_payment(&acct, a_acls, a_rules, move_id, "60".parse().unwrap(), bank_journal).await.unwrap();
    let after2 = db.find_one_secured(&mv, &su, &[], &[], move_id).await.unwrap().unwrap();
    assert_eq!(money(&after2, "amount_residual"), 0.0);
    assert_eq!(after2["payment_state"], "paid");
    assert_eq!(after2["reconciled"], true);

    // Overpayment / paying a settled invoice is rejected.
    assert!(db.register_payment(&acct, a_acls, a_rules, move_id, "1".parse().unwrap(), bank_journal).await.is_err(), "cannot pay beyond the residual");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
