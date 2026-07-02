//! Concurrent posting is serialized on the move row: two concurrent `post` services on the SAME draft
//! entry produce EXACTLY ONE posted entry with ONE number — the loser blocks on the FOR UPDATE state
//! read until the winner commits, re-reads state='posted', and fails BEFORE consuming a sequence number
//! (no orphaned number, no name overwrite). The posting twin of the invoice_race claim test. Requires
//! DATABASE_URL.

use meshble::prelude::*;
use meshble_db::Db;
use serde_json::json;

fn link() {
    let _ = (&meshble_mod_account::MANIFEST, &meshble_mod_base::MANIFEST, &meshble_mod_mail::MANIFEST);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_posts_win_exactly_once_and_gap_nothing() {
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

    let (currency, account, journal, mv) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
        resolve_registered("account.move").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({
        "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
    }).as_object().unwrap()).await.unwrap();
    let recv = db.insert_secured(&account, &su, &[], &[], json!({ "code": "121000", "name": "Receivable", "account_type": "receivable" }).as_object().unwrap()).await.unwrap();
    let inc = db.insert_secured(&account, &su, &[], &[], json!({ "code": "400000", "name": "Sales", "account_type": "income" }).as_object().unwrap()).await.unwrap();
    let jid = db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Customer Invoices", "code": "INV", "journal_type": "sale", "sequence_code": "INV" }).as_object().unwrap()).await.unwrap();

    let draft = |db: &Db| {
        let mv = mv.clone();
        let su = su.clone();
        let db = db.clone();
        async move {
            db.insert_secured(&mv, &su, &[], &[], json!({
                "move_type": "entry", "journal_id": jid, "currency_id": cur,
                "line_ids": [
                    { "account_id": recv, "name": "AR", "debit": "100", "credit": "0" },
                    { "account_id": inc, "name": "Income", "debit": "0", "credit": "100" }
                ]
            }).as_object().unwrap()).await.unwrap()
        }
    };
    let mid = draft(&db).await;

    // Race: two concurrent posts of the same draft — exactly one wins.
    let (a, b) = tokio::join!(
        db.run_service(&mv, &su, &[], &[], mid, "post", serde_json::Map::new()),
        db.run_service(&mv, &su, &[], &[], mid, "post", serde_json::Map::new()),
    );
    let oks = [a.is_ok(), b.is_ok()].iter().filter(|x| **x).count();
    assert_eq!(oks, 1, "exactly one concurrent post wins (got a={:?} b={:?})", a.is_ok(), b.is_ok());
    let winner = if a.is_ok() { a.unwrap() } else { b.unwrap() };
    let number = winner["posted"].as_str().unwrap().to_string();

    // The entry is posted once, named with the WINNER's number (no overwrite by the loser).
    let posted = db.find_one_secured(&mv, &su, &[], &[], mid).await.unwrap().unwrap();
    assert_eq!(posted["state"], "posted");
    assert_eq!(posted["name"].as_str(), Some(number.as_str()), "the loser did not overwrite the entry number");

    // The loser failed BEFORE claiming a number: the next post in this journal takes the immediate
    // successor (no gap from the race).
    let n: i64 = number.rsplit('/').next().unwrap().parse().unwrap();
    let mid2 = draft(&db).await;
    let second = db.run_service(&mv, &su, &[], &[], mid2, "post", serde_json::Map::new()).await.unwrap()["posted"]
        .as_str().unwrap().to_string();
    assert_eq!(second, format!("INV/{:05}", n + 1), "no sequence gap from the losing post");
}
