//! Multi-currency conversion: convert_amount uses the latest res.currency.rate on or before the date;
//! the base currency (no rate rows) is 1.0; an unknown historical rate errors. Requires DATABASE_URL.

use kigumi::prelude::*;
use serde_json::json;

fn link() {
    let _ = (&kigumi_mod_account::MANIFEST, &kigumi_mod_base::MANIFEST, &kigumi_mod_mail::MANIFEST);
}

#[tokio::test]
async fn convert_amount_uses_the_dated_rate() {
    link();
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let pool = db.pool().clone(); // ServiceCtx::pool() analogue for the relocated FX helper
    let su = kigumi_test::su();

    let (currency, rate) = (resolve_registered("res.currency").unwrap(), resolve_registered("res.currency.rate").unwrap());
    let eur = db.insert_secured(&currency, &su, &[], &[], json!({ "name": "Euro", "code": "EUR", "symbol": "E", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true }).as_object().unwrap()).await.unwrap();
    let usd = db.insert_secured(&currency, &su, &[], &[], json!({ "name": "Dollar", "code": "USD", "symbol": "$", "decimal_places": 2, "rounding": 0.01, "position": "before", "active": true }).as_object().unwrap()).await.unwrap();
    let gbp = db.insert_secured(&currency, &su, &[], &[], json!({ "name": "Pound", "code": "GBP", "symbol": "L", "decimal_places": 2, "rounding": 0.01, "position": "before", "active": true }).as_object().unwrap()).await.unwrap();
    // EUR is the base (no rate rows). 1.25 USD per 1 EUR, effective 2020.
    db.insert_secured(&rate, &su, &[], &[], json!({ "currency_id": usd, "name": "2020-01-01", "rate": "1.25" }).as_object().unwrap()).await.unwrap();
    // GBP only has a FUTURE rate → unknown for an earlier date.
    db.insert_secured(&rate, &su, &[], &[], json!({ "currency_id": gbp, "name": "2099-12-31", "rate": "0.9" }).as_object().unwrap()).await.unwrap();

    let f = |d: rust_decimal::Decimal| d.to_string().parse::<f64>().unwrap();

    // 125 USD -> 100 EUR (divide by the USD rate; EUR base = 1.0).
    assert_eq!(f(kigumi_mod_account::services::convert_amount(&pool, "125".parse().unwrap(), usd, eur, "2099-01-01").await.unwrap()), 100.0);
    // 100 EUR -> 125 USD (multiply by the USD rate).
    assert_eq!(f(kigumi_mod_account::services::convert_amount(&pool, "100".parse().unwrap(), eur, usd, "2099-01-01").await.unwrap()), 125.0);
    // Same currency is identity.
    assert_eq!(f(kigumi_mod_account::services::convert_amount(&pool, "50".parse().unwrap(), eur, eur, "2099-01-01").await.unwrap()), 50.0);
    // GBP has rates but none on or before 2020 → error (never silently 1.0).
    assert!(kigumi_mod_account::services::convert_amount(&pool, "100".parse().unwrap(), gbp, eur, "2020-06-01").await.is_err(), "unknown historical rate errors");
}
