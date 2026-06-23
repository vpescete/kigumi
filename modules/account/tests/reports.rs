//! Trial balance: per-account totals over POSTED entries, balanced (Σ debit == Σ credit). A draft entry
//! is excluded. Requires DATABASE_URL.

use meshble::prelude::*;
use meshble_db::Db;
use serde_json::json;

fn link() {
    let _ = (&meshble_mod_account::MANIFEST, &meshble_mod_base::MANIFEST, &meshble_mod_mail::MANIFEST);
}

#[tokio::test]
async fn trial_balance_aggregates_posted_entries() {
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

    let (account, journal, mv) = (
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
        resolve_registered("account.move").unwrap(),
    );
    let recv = db.insert_secured(&account, &su, &[], &[], json!({ "code": "121000", "name": "Receivable", "account_type": "receivable" }).as_object().unwrap()).await.unwrap();
    let inc = db.insert_secured(&account, &su, &[], &[], json!({ "code": "400000", "name": "Sales", "account_type": "income" }).as_object().unwrap()).await.unwrap();
    let j = db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Sales", "code": "INV", "journal_type": "sale", "sequence_code": "INV" }).as_object().unwrap()).await.unwrap();

    // A POSTED invoice for 100 (income credit / receivable debit).
    let posted = db.insert_secured(&mv, &su, &[], &[], json!({
        "move_type": "out_invoice", "journal_id": j,
        "line_ids": [
            { "account_id": inc, "debit": "0", "credit": "100" },
            { "account_id": recv, "debit": "100", "credit": "0" }
        ]
    }).as_object().unwrap()).await.unwrap();
    db.post_move(&su, &[], &[], posted).await.unwrap();

    // A DRAFT entry for 50 — must NOT appear in the trial balance.
    db.insert_secured(&mv, &su, &[], &[], json!({
        "move_type": "entry", "journal_id": j,
        "line_ids": [
            { "account_id": inc, "debit": "0", "credit": "50" },
            { "account_id": recv, "debit": "50", "credit": "0" }
        ]
    }).as_object().unwrap()).await.unwrap();

    let rows = db.trial_balance(&su, &[], &[]).await.unwrap();
    let money = |r: &serde_json::Value, f: &str| -> f64 { r[f].as_str().unwrap().parse().unwrap() };
    let by_code = |code: &str| rows.iter().find(|r| r["code"] == code).cloned();

    assert_eq!(rows.len(), 2, "only the two accounts with posted activity");
    let r = by_code("121000").unwrap();
    assert_eq!(money(&r, "debit"), 100.0);
    assert_eq!(money(&r, "balance"), 100.0, "receivable balance reflects only the posted 100, not the draft 50");
    let i = by_code("400000").unwrap();
    assert_eq!(money(&i, "credit"), 100.0);
    assert_eq!(money(&i, "balance"), -100.0);
    let total_debit: f64 = rows.iter().map(|r| money(r, "debit")).sum();
    let total_credit: f64 = rows.iter().map(|r| money(r, "credit")).sum();
    assert_eq!(total_debit, total_credit, "the trial balance is balanced");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
