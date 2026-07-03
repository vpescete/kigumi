//! Aged receivable: open posted invoices grouped by partner and bucketed by days past their due date.
//! Requires DATABASE_URL.

use kigumi::prelude::*;
use serde_json::json;

fn link() {
    let _ = (&kigumi_mod_account::MANIFEST, &kigumi_mod_base::MANIFEST, &kigumi_mod_mail::MANIFEST);
}

#[tokio::test]
async fn aged_balance_buckets_open_invoices_by_due_date() {
    link();
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let su = kigumi_test::su();

    let (partner, account, journal, mv) = (
        resolve_registered("res.partner").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
        resolve_registered("account.move").unwrap(),
    );
    let cust = db.insert_secured(&partner, &su, &[], &[], json!({ "name": "ACME" }).as_object().unwrap()).await.unwrap();
    let inc = db.insert_secured(&account, &su, &[], &[], json!({ "code": "400000", "name": "Sales", "account_type": "income" }).as_object().unwrap()).await.unwrap();
    let recv = db.insert_secured(&account, &su, &[], &[], json!({ "code": "121000", "name": "Receivable", "account_type": "receivable" }).as_object().unwrap()).await.unwrap();
    let j = db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Sales", "code": "INV", "journal_type": "sale", "sequence_code": "INV" }).as_object().unwrap()).await.unwrap();

    let invoice = |amount: &str, due: &str| {
        json!({ "move_type": "out_invoice", "journal_id": j, "partner_id": cust, "invoice_date_due": due,
            "amount_residual": amount,
            "line_ids": [ { "account_id": inc, "debit": "0", "credit": amount }, { "account_id": recv, "debit": amount, "credit": "0" } ] })
    };
    // Way overdue (2020) → 90+; due far in the future → current.
    let a = db.insert_secured(&mv, &su, &[], &[], invoice("200", "2020-01-01").as_object().unwrap()).await.unwrap();
    db.run_service(&mv, &su, &[], &[], a, "post", serde_json::Map::new()).await.unwrap();
    let b = db.insert_secured(&mv, &su, &[], &[], invoice("50", "2099-01-01").as_object().unwrap()).await.unwrap();
    db.run_service(&mv, &su, &[], &[], b, "post", serde_json::Map::new()).await.unwrap();
    // A DRAFT invoice must be excluded.
    db.insert_secured(&mv, &su, &[], &[], invoice("999", "2020-01-01").as_object().unwrap()).await.unwrap();

    let rows = db.run_ledger_report(&su, &[], "aged_balance", serde_json::json!({"kind": "receivable"}).as_object().unwrap().clone()).await.unwrap();
    assert_eq!(rows.len(), 1, "one partner with open receivables");
    let r = &rows[0];
    let f = |k: &str| r[k].as_str().unwrap().parse::<f64>().unwrap();
    assert_eq!(r["partner_name"], "ACME");
    assert_eq!(f("b90_plus"), 200.0, "the 2020 invoice ages to 90+");
    assert_eq!(f("current"), 50.0, "the future-dated invoice is current");
    assert_eq!(f("b31_60"), 0.0);
    assert_eq!(f("total"), 250.0, "only the two POSTED invoices (draft 999 excluded)");
}
