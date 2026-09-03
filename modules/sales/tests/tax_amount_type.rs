//! An `account.tax` carrying an `amount_type` the engine does not implement must REFUSE to compute,
//! not quietly fall through to the percent branch and produce a wrong number on a money path.
//! Requires DATABASE_URL.

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

#[tokio::test]
async fn an_unknown_amount_type_is_refused_instead_of_computed_as_percent() {
    link();
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let su = kigumi_test::su();

    let (currency, partner, product, order, tax) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("res.partner").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("sale.order").unwrap(),
        resolve_registered("account.tax").unwrap(),
    );
    let cur = db.insert_secured(&currency, &su, &[], &[], json!({
        "name": "Euro", "code": "EUR", "symbol": "€", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
    }).as_object().unwrap()).await.unwrap();
    let bad_tax = db.insert_secured(&tax, &su, &[], &[], json!({
        "name": "Withholding 20%", "type_tax_use": "sale", "amount_type": "percent", "amount": "20", "active": true
    }).as_object().unwrap()).await.unwrap();
    let cust = db.insert_secured(&partner, &su, &[], &[], json!({ "name": "ACME" }).as_object().unwrap()).await.unwrap();
    let prod = db.insert_secured(&product, &su, &[], &[], json!({ "name": "Widget", "list_price": 100.0 }).as_object().unwrap()).await.unwrap();

    let oid = db.insert_secured(&order, &su, &[], &[], json!({
        "name": "New", "partner_id": cust, "currency_id": cur,
        "line_ids": [{ "product_id": prod, "product_uom_qty": 1, "price_unit": 100.0, "tax_id": bad_tax }]
    }).as_object().unwrap()).await.unwrap();

    // Plant the unsupported kind the way it really arrives: not through the write API (the Selection
    // field is the front door) but by drift — a seed, a hand-run migration, direct SQL.
    sqlx::query("UPDATE account_tax SET amount_type = 'withholding' WHERE id = $1")
        .bind(bad_tax)
        .execute(t.db.pool())
        .await
        .unwrap();

    let seller = Ctx::new(1, vec!["sales.user".to_string()]);
    let (acls, rules) = (kigumi_mod_sales::ACLS, kigumi_mod_sales::RECORD_RULES);
    let err = db
        .run_service(&order, &seller, acls, rules, oid, "apply_taxes", serde_json::Map::new())
        .await
        .expect_err("an unsupported amount_type must not compute");

    match &err {
        DbError::Invalid { message, fields } => {
            assert!(message.contains("withholding"), "names the offending value: {message}");
            assert!(fields.iter().any(|(f, _)| f == "amount_type"), "attributed to the field: {fields:?}");
        }
        other => panic!("expected DbError::Invalid (HTTP 400), got {other:?}"),
    }

    // And the order is untouched: refusing beats half-applying.
    let after = db.find_one_secured(&order, &su, &[], &[], oid).await.unwrap().unwrap();
    assert_eq!(after["amount_tax"].as_str().unwrap().parse::<f64>().unwrap(), 0.0);
}
