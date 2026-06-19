//! M16.3: posting workflow + per-journal numbering + posted-entry immutability, on a real database.
//! Posting a balanced draft entry numbers it from its journal's sequence and flips it to posted; a
//! posted entry's lines are then frozen for a non-superuser until it is reset to draft. Requires
//! DATABASE_URL.

use meshble::prelude::*;
use meshble_db::Db;
use serde_json::json;

fn link() {
    let _ = (&meshble_mod_account::MANIFEST, &meshble_mod_base::MANIFEST, &meshble_mod_mail::MANIFEST);
}

#[tokio::test]
async fn post_numbers_then_freezes_the_entry() {
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

    let (currency, account, journal, mv, line) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
        resolve_registered("account.move").unwrap(),
        resolve_registered("account.move.line").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({
        "name": "Euro", "code": "EUR", "symbol": "€", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
    }).as_object().unwrap()).await.unwrap();
    let recv = db.insert_secured(&account, &su, &[], &[], json!({ "code": "121000", "name": "Receivable", "account_type": "receivable" }).as_object().unwrap()).await.unwrap();
    let inc = db.insert_secured(&account, &su, &[], &[], json!({ "code": "400000", "name": "Sales", "account_type": "income" }).as_object().unwrap()).await.unwrap();
    let jid = db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Customer Invoices", "code": "INV", "journal_type": "sale", "sequence_code": "INV" }).as_object().unwrap()).await.unwrap();

    // A balanced draft entry (shared / no company so a company-less clerk can see its lines below).
    let mid = db.insert_secured(&mv, &su, &[], &[], json!({
        "move_type": "entry", "journal_id": jid, "currency_id": cur,
        "line_ids": [
            { "account_id": recv, "name": "AR", "debit": "100", "credit": "0" },
            { "account_id": inc, "name": "Income", "debit": "0", "credit": "100" }
        ]
    }).as_object().unwrap()).await.unwrap();

    // Post: numbered from the INV journal sequence, state posted.
    let number = db.post_move(&su, &[], &[], mid).await.unwrap();
    assert!(number.starts_with("INV/"), "entry numbered from the journal sequence, got {number}");
    let posted = db.find_one_secured(&mv, &su, &[], &[], mid).await.unwrap().unwrap();
    assert_eq!(posted["state"], "posted");
    assert_eq!(posted["name"].as_str(), Some(number.as_str()), "the assigned number is stored as the entry name");
    // Re-posting is rejected (not a draft).
    assert!(db.post_move(&su, &[], &[], mid).await.is_err(), "a posted entry cannot be posted again");

    // Posted immutability: a non-superuser cannot write the posted entry's lines.
    let clerk = Ctx::new(1, vec!["account.user".to_string(), "account.manager".to_string()]);
    let (acls, rules) = (meshble_mod_account::ACLS, meshble_mod_account::RECORD_RULES);
    let lines = db.find_secured(&line, &su, &[], &[], Some(&Domain::field("move_id").eq(mid))).await.unwrap();
    let lid = lines[0]["id"].as_i64().unwrap();
    let frozen = db.update_secured(&line, &clerk, acls, rules, lid, json!({ "name": "tampered" }).as_object().unwrap()).await.unwrap();
    assert_eq!(frozen, 0, "a posted entry's line is frozen for a non-superuser");

    // Reset to draft → the line is writable again.
    db.run_action(&mv, &su, &[], &[], mid, "button_draft").await.unwrap();
    assert_eq!(db.find_one_secured(&mv, &su, &[], &[], mid).await.unwrap().unwrap()["state"], "draft");
    let editable = db.update_secured(&line, &clerk, acls, rules, lid, json!({ "name": "corrected" }).as_object().unwrap()).await.unwrap();
    assert_eq!(editable, 1, "after un-posting, the line is writable again");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
