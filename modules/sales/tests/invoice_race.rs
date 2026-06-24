//! The invoicing claim is a compare-and-set: two concurrent create_sale_invoice calls on the SAME order
//! produce EXACTLY ONE invoice (one wins the to_invoice -> invoiced flip, the other is rejected), closing
//! the read-then-claim double-invoice race. Requires DATABASE_URL.

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_invoicing_claims_exactly_once() {
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

    let (currency, partner, product, order, mv, account, journal) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("sale.order").unwrap(),
        resolve_registered("account.move").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&account, &su, &[], &[], json!({ "code": "121000", "name": "Receivable", "account_type": "receivable" }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&account, &su, &[], &[], json!({ "code": "400000", "name": "Sales", "account_type": "income" }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Customer Invoices", "code": "INV", "journal_type": "sale", "sequence_code": "INV" }).as_object().unwrap()).await.unwrap();
    let cust = db.insert_secured(&partner, &su, &[], &[], json!({ "name": "ACME" }).as_object().unwrap()).await.unwrap();
    let prod = db.insert_secured(&product, &su, &[], &[], json!({ "name": "Widget", "list_price": 100.0 }).as_object().unwrap()).await.unwrap();

    let oid = db.insert_secured(&order, &su, &[], &[], json!({
        "name": "New", "partner_id": cust, "currency_id": cur,
        "line_ids": [{ "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0 }]
    }).as_object().unwrap()).await.unwrap();
    db.run_action(&order, &su, &[], &[], oid, "confirm").await.unwrap();

    let seller = Ctx::new(1, vec!["sales.user".to_string()]);
    let (acls, rules) = (meshble_mod_sales::ACLS, meshble_mod_sales::RECORD_RULES);

    // Fire two invoicings of the SAME order concurrently.
    let (a, b) = tokio::join!(
        db.create_sale_invoice(&seller, acls, rules, oid),
        db.create_sale_invoice(&seller, acls, rules, oid),
    );
    let oks = [a.is_ok(), b.is_ok()].iter().filter(|x| **x).count();
    assert_eq!(oks, 1, "exactly one concurrent invoicing wins the claim (got a={:?} b={:?})", a.is_ok(), b.is_ok());

    // And exactly one invoice exists for the order.
    let invoices = db.find_secured(&mv, &su, &[], &[], Some(&Domain::field("move_type").eq("out_invoice"))).await.unwrap();
    assert_eq!(invoices.len(), 1, "exactly one invoice was posted, not a duplicate");
    assert_eq!(db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap()["invoice_status"], "invoiced");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
