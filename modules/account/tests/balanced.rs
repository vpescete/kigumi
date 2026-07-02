//! M16.2: the balanced double-entry invariant on a real database. A move whose lines balance (Σ debit
//! == Σ credit) saves and rolls up its total + per-line balance; an unbalanced move is rejected in-tx
//! by the check_balanced @api.constrains and leaves nothing behind. Requires DATABASE_URL.

use kigumi::prelude::*;
use kigumi_db::Db;
use serde_json::json;

fn link() {
    let _ = (&kigumi_mod_account::MANIFEST, &kigumi_mod_base::MANIFEST, &kigumi_mod_mail::MANIFEST);
}

fn money(v: &serde_json::Value, field: &str) -> f64 {
    v[field].as_str().unwrap_or_else(|| panic!("{field} not a string: {v:?}")).parse().unwrap()
}

#[tokio::test]
async fn moves_must_balance_debit_equals_credit() {
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

    let (currency, company, account, journal, mv) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.company").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
        resolve_registered("account.move").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({
        "name": "Euro", "code": "EUR", "symbol": "€", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
    }).as_object().unwrap()).await.unwrap();
    let comp = db.insert_secured(&company, &su, &[], &[], json!({ "name": "Main", "currency_id": cur, "active": true }).as_object().unwrap()).await.unwrap();
    let recv = db.insert_secured(&account, &su, &[], &[], json!({ "code": "121000", "name": "Receivable", "account_type": "receivable", "company_id": comp }).as_object().unwrap()).await.unwrap();
    let inc = db.insert_secured(&account, &su, &[], &[], json!({ "code": "400000", "name": "Sales", "account_type": "income", "company_id": comp }).as_object().unwrap()).await.unwrap();
    let jid = db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Misc", "code": "MISC", "journal_type": "general", "company_id": comp, "sequence_code": "MISC" }).as_object().unwrap()).await.unwrap();

    // A balanced entry: 100 debit to receivable, 100 credit to income → saves.
    let mid = db.insert_secured(&mv, &su, &[], &[], json!({
        "move_type": "entry", "journal_id": jid, "currency_id": cur, "company_id": comp,
        "line_ids": [
            { "account_id": recv, "name": "Receivable", "debit": "100", "credit": "0" },
            { "account_id": inc, "name": "Income", "debit": "0", "credit": "100" }
        ]
    }).as_object().unwrap()).await.unwrap();
    let got = db.find_one_secured(&mv, &su, &[], &[], mid).await.unwrap().unwrap();
    assert_eq!(money(&got, "amount_total"), 100.0, "move total rolls up from the debit side");
    // Per-line balance is derived on read (debit − credit) and the two sides net to zero.
    let lines = got["line_ids"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    let net: f64 = lines.iter().map(|l| money(l, "balance")).sum();
    assert_eq!(net, 0.0, "balanced: the line balances net to zero");
    assert!(lines.iter().any(|l| money(l, "balance") == 100.0), "receivable line balance = +100");
    assert!(lines.iter().any(|l| money(l, "balance") == -100.0), "income line balance = -100");

    // An unbalanced entry (100 debit vs 50 credit) → rejected by check_balanced, rolled back.
    let bad = db.insert_secured(&mv, &su, &[], &[], json!({
        "move_type": "entry", "journal_id": jid, "company_id": comp,
        "line_ids": [
            { "account_id": recv, "debit": "100", "credit": "0" },
            { "account_id": inc, "debit": "0", "credit": "50" }
        ]
    }).as_object().unwrap()).await;
    assert!(bad.is_err(), "unbalanced move rejected by check_balanced");
    assert_eq!(db.count_secured(&mv, &su, &[], &[], None).await.unwrap(), 1, "the unbalanced move rolled back — only the balanced one remains");

    // Multi-company coherence (check_line_companies): a balanced move in company A with a line that
    // explicitly belongs to company B is rejected (no mixing companies in one entry).
    let comp_b = db.insert_secured(&company, &su, &[], &[], json!({ "name": "Other", "currency_id": cur, "active": true }).as_object().unwrap()).await.unwrap();
    let cross = db.insert_secured(&mv, &su, &[], &[], json!({
        "move_type": "entry", "journal_id": jid, "company_id": comp,
        "line_ids": [
            { "account_id": recv, "debit": "100", "credit": "0", "company_id": comp },
            { "account_id": inc, "debit": "0", "credit": "100", "company_id": comp_b }
        ]
    }).as_object().unwrap()).await;
    assert!(cross.is_err(), "a line of another company is rejected by check_line_companies");
    assert_eq!(db.count_secured(&mv, &su, &[], &[], None).await.unwrap(), 1, "the cross-company move rolled back");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
