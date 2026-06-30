//! A confirmed purchase order generates a posted, balanced vendor bill (account.move, in_invoice): an
//! expense debit (untaxed) + tax debit + payable credit (total), the buy-side mirror of the sale invoice.
//! Driven by a plain sales.user with no account groups; the GL posting runs elevated. The bill is then
//! paid via register_payment (in_invoice -> payable). Requires DATABASE_URL.

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

fn money(v: &serde_json::Value, field: &str) -> f64 {
    v[field].as_str().unwrap_or_else(|| panic!("{field} not a string: {v:?}")).parse().unwrap()
}

#[tokio::test]
async fn confirmed_purchase_generates_a_posted_balanced_bill() {
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
    db.ensure_sequence("PO", "PO/", "", 5).await.unwrap();

    let (currency, partner, product, order, mv, account, journal) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("purchase.order").unwrap(),
        resolve_registered("account.move").unwrap(),
        resolve_registered("account.account").unwrap(),
        resolve_registered("account.journal").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({
        "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
    }).as_object().unwrap()).await.unwrap();
    let payable = db.insert_secured(&account, &su, &[], &[], json!({ "code": "211000", "name": "Payable", "account_type": "payable" }).as_object().unwrap()).await.unwrap();
    let expense = db.insert_secured(&account, &su, &[], &[], json!({ "code": "600000", "name": "Expenses", "account_type": "expense" }).as_object().unwrap()).await.unwrap();
    let taxacc = db.insert_secured(&account, &su, &[], &[], json!({ "code": "251000", "name": "Tax", "account_type": "tax" }).as_object().unwrap()).await.unwrap();
    let bank_acc = db.insert_secured(&account, &su, &[], &[], json!({ "code": "550000", "name": "Bank", "account_type": "bank_cash" }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Vendor Bills", "code": "BILL", "journal_type": "purchase", "sequence_code": "BILL" }).as_object().unwrap()).await.unwrap();
    let bank_journal = db.insert_secured(&journal, &su, &[], &[], json!({ "name": "Bank", "code": "BNK", "journal_type": "bank", "sequence_code": "BNK", "default_account_id": bank_acc }).as_object().unwrap()).await.unwrap();
    let vendor = db.insert_secured(&partner, &su, &[], &[], json!({ "name": "ACME Supply" }).as_object().unwrap()).await.unwrap();
    let prod = db.insert_secured(&product, &su, &[], &[], json!({ "name": "Widget", "list_price": 100.0 }).as_object().unwrap()).await.unwrap();

    // A draft PO with a taxed line: 1 x 100 @ 22% -> untaxed 100, tax 22, total 122.
    let oid = db.insert_secured(&order, &su, &[], &[], json!({
        "name": "New", "partner_id": vendor, "currency_id": cur,
        "line_ids": [{ "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0, "tax_rate": "22" }]
    }).as_object().unwrap()).await.unwrap();
    db.run_action(&order, &su, &[], &[], oid, "confirm").await.unwrap();
    let confirmed = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(confirmed["invoice_status"], "to_invoice", "confirm marks the order to bill");
    assert_eq!(money(&confirmed, "amount_total"), 122.0);

    // Bill it as a plain sales.user (gates on purchase.order write; GL posting runs elevated).
    let buyer = Ctx::new(1, vec!["sales.user".to_string()]);
    let (acls, rules) = (meshble_mod_sales::ACLS, meshble_mod_sales::RECORD_RULES);
    let move_id = db.run_service(&order, &buyer, acls, rules, oid, "create_vendor_bill", serde_json::Map::new()).await.unwrap()["bill"].as_i64().unwrap();

    let bill = db.find_one_secured(&mv, &su, &[], &[], move_id).await.unwrap().unwrap();
    assert_eq!(bill["state"], "posted", "the bill is posted");
    assert_eq!(bill["move_type"], "in_invoice");
    assert_eq!(money(&bill, "amount_total"), 122.0);
    let lines = bill["line_ids"].as_array().unwrap();
    assert_eq!(lines.len(), 3, "expense + tax + payable");
    assert_eq!(lines.iter().map(|l| money(l, "balance")).sum::<f64>(), 0.0, "the bill is balanced");
    assert!(lines.iter().any(|l| l["account_id"].as_i64() == Some(expense) && money(l, "debit") == 100.0), "expense debit 100");
    assert!(lines.iter().any(|l| l["account_id"].as_i64() == Some(taxacc) && money(l, "debit") == 22.0), "tax debit 22");
    assert!(lines.iter().any(|l| l["account_id"].as_i64() == Some(payable) && money(l, "credit") == 122.0), "payable credit 122");
    assert_eq!(money(&bill, "amount_residual"), 122.0);

    let after = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(after["invoice_status"], "invoiced", "the order is now billed");

    // Re-billing is rejected (the order is no longer to_invoice).
    assert!(db.run_service(&order, &buyer, acls, rules, oid, "create_vendor_bill", serde_json::Map::new()).await.is_err(), "cannot bill twice");

    // Pay the vendor bill: in_invoice draws down the payable, crediting the bank.
    let acct = Ctx::new(2, vec!["account.user".to_string()]);
    let (a_acls, a_rules) = (meshble_mod_account::ACLS, meshble_mod_account::RECORD_RULES);
    db.run_service(&mv, &acct, a_acls, a_rules, move_id, "register_payment", serde_json::json!({"amount": "122", "journal_id": bank_journal}).as_object().unwrap().clone()).await.unwrap();
    let paid = db.find_one_secured(&mv, &su, &[], &[], move_id).await.unwrap().unwrap();
    assert_eq!(money(&paid, "amount_residual"), 0.0, "the bill is settled");
    assert_eq!(paid["payment_state"], "paid");

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
