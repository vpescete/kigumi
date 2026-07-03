//! The invoicing claim is a compare-and-set: two concurrent create_sale_invoice calls on the SAME order
//! produce EXACTLY ONE invoice (one wins the to_invoice -> invoiced flip, the other is rejected), closing
//! the read-then-claim double-invoice race. Requires DATABASE_URL.

use kigumi::prelude::*;
use serde_json::json;

fn link() {
    let _ = (
        &kigumi_mod_sales::MANIFEST,
        &kigumi_mod_base::MANIFEST,
        &kigumi_mod_mail::MANIFEST,
        &kigumi_mod_account::MANIFEST,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_invoicing_claims_exactly_once() {
    link();
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let su = kigumi_test::su();
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
    let (acls, rules) = (kigumi_mod_sales::ACLS, kigumi_mod_sales::RECORD_RULES);

    // Fire two invoicings of the SAME order concurrently.
    let (a, b) = tokio::join!(
        db.run_service(&order, &seller, acls, rules, oid, "create_invoice", serde_json::Map::new()),
        db.run_service(&order, &seller, acls, rules, oid, "create_invoice", serde_json::Map::new()),
    );
    let oks = [a.is_ok(), b.is_ok()].iter().filter(|x| **x).count();
    assert_eq!(oks, 1, "exactly one concurrent invoicing wins the claim (got a={:?} b={:?})", a.is_ok(), b.is_ok());

    // And exactly one invoice exists for the order.
    let invoices = db.find_secured(&mv, &su, &[], &[], Some(&Domain::field("move_type").eq("out_invoice"))).await.unwrap();
    assert_eq!(invoices.len(), 1, "exactly one invoice was posted, not a duplicate");
    assert_eq!(db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap()["invoice_status"], "invoiced");
}
