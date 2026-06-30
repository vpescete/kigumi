//! General-ledger drill-down: every posted move line on one account, in date order, with a running
//! balance. A draft entry is excluded. Requires DATABASE_URL.

use meshble::prelude::*;
use meshble_db::Db;
use serde_json::json;

fn link() {
    let _ = (&meshble_mod_account::MANIFEST, &meshble_mod_base::MANIFEST, &meshble_mod_mail::MANIFEST);
}

fn money(r: &serde_json::Value, f: &str) -> f64 {
    r[f].as_str().unwrap_or_else(|| panic!("{f} not a string: {r:?}")).parse().unwrap()
}

#[tokio::test]
async fn general_ledger_lists_posted_lines_with_a_running_balance() {
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

    // Two POSTED invoices touching the receivable (debit 100, then debit 50) + one credit (50 paid back).
    for (d1, c1, d2, c2) in [("0", "100", "100", "0"), ("0", "50", "50", "0")] {
        let m = db.insert_secured(&mv, &su, &[], &[], json!({
            "move_type": "out_invoice", "journal_id": j, "date": "2026-01-10",
            "line_ids": [
                { "account_id": inc, "debit": d1, "credit": c1 },
                { "account_id": recv, "debit": d2, "credit": c2 }
            ]
        }).as_object().unwrap()).await.unwrap();
        db.run_service(&mv, &su, &[], &[], m, "post", serde_json::Map::new()).await.unwrap();
    }
    // A DRAFT entry on the receivable — must NOT appear.
    db.insert_secured(&mv, &su, &[], &[], json!({
        "move_type": "entry", "journal_id": j,
        "line_ids": [ { "account_id": inc, "debit": "0", "credit": "9" }, { "account_id": recv, "debit": "9", "credit": "0" } ]
    }).as_object().unwrap()).await.unwrap();

    let rows = db.general_ledger(&su, &[], &[], recv).await.unwrap();
    assert_eq!(rows.len(), 2, "two posted receivable lines (the draft is excluded)");
    assert_eq!(money(&rows[0], "debit"), 100.0);
    assert_eq!(money(&rows[0], "balance"), 100.0, "running balance after the first line");
    assert_eq!(money(&rows[1], "debit"), 50.0);
    assert_eq!(money(&rows[1], "balance"), 150.0, "running balance accumulates");

    // The income account's GL shows the credits, balance going negative (credit-natured).
    let inc_rows = db.general_ledger(&su, &[], &[], inc).await.unwrap();
    assert_eq!(inc_rows.len(), 2);
    assert_eq!(money(&inc_rows[1], "balance"), -150.0, "income runs to -150 (Σ debit - credit)");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
