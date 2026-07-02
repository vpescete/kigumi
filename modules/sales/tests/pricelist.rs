//! resolve_price (now a module-owned service helper, formerly Db::resolve_price): the most-specific
//! applicable rule wins (variant > product > category > global), the category match walks the product's
//! category ancestry, min_quantity tiers apply, and the price is a fixed amount or a percentage off the
//! base (the variant's lst_price = list_price + price_extra). Exercised against the REAL sales product
//! models, through the pool the way the apply_pricelist service calls it. Requires DATABASE_URL.

use kigumi::prelude::*;
use kigumi_db::Db;
use rust_decimal::Decimal;
use serde_json::json;
use std::str::FromStr;

fn link() {
    let _ = (&kigumi_mod_sales::MANIFEST, &kigumi_mod_base::MANIFEST, &kigumi_mod_mail::MANIFEST);
}

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

#[tokio::test]
async fn resolve_price_picks_the_most_specific_rule() {
    link();
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let db = Db::connect(&url).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap(); // ServiceCtx::pool() analogue for the helper
    let su = Ctx::new(0, vec![]).sudo();

    let plan = migration_plan().unwrap();
    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
    for t in &plan { db.create_table(&t.model).await.unwrap(); }
    for t in &plan { db.create_m2m_relations(&t.model).await.unwrap(); }

    let (currency, category, product, pricelist, item) = (
        resolve_registered("res.currency").unwrap(),
        resolve_registered("product.category").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("product.pricelist").unwrap(),
        resolve_registered("product.pricelist.item").unwrap(),
    );
    let ins = |model: ResolvedModel, v: serde_json::Value| {
        let db = &db;
        let su = &su;
        async move { db.insert_secured(&model, su, &[], &[], v.as_object().unwrap()).await.unwrap() }
    };

    let cur = ins(currency.clone(), json!({ "name": "Euro", "code": "EUR", "symbol": "€", "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true })).await;
    let electronics = ins(category.clone(), json!({ "name": "Electronics" })).await;
    let phones = ins(category.clone(), json!({ "name": "Phones", "parent_id": electronics })).await;
    // A variant auto-creates its template; categ_id/list_price/standard_price delegate to it, price_extra
    // is the variant's own (base.system, writable here as su). lst_price = 1000 + 50 = 1050.
    let v = ins(product.clone(), json!({ "name": "Phone", "list_price": "1000", "standard_price": "600", "categ_id": phones, "price_extra": "50" })).await;
    let phone_tmpl = db.find_one_secured(&product, &su, &[], &[], v).await.unwrap().unwrap()
        .get("product_tmpl_id").and_then(|x| x.as_i64()).unwrap();
    let plid = ins(pricelist.clone(), json!({ "name": "Public", "currency_id": cur })).await;

    // Four overlapping rules, increasingly specific.
    ins(item.clone(), json!({ "pricelist_id": plid, "applied_on": "3_global", "compute_price": "percentage", "percent_price": "10", "base": "list_price" })).await;
    let categ = ins(item.clone(), json!({ "pricelist_id": plid, "applied_on": "2_product_category", "categ_id": electronics, "compute_price": "fixed", "fixed_price": "900" })).await; // via the Phones->Electronics ancestor
    let prod = ins(item.clone(), json!({ "pricelist_id": plid, "applied_on": "1_product", "product_tmpl_id": phone_tmpl, "compute_price": "fixed", "fixed_price": "800" })).await;
    let variant = ins(item.clone(), json!({ "pricelist_id": plid, "applied_on": "0_product_variant", "product_id": v, "compute_price": "fixed", "fixed_price": "700" })).await;

    let today = db.today().await.unwrap();
    let price = |plid: i64, qty: Decimal, today: String| {
        let pool = &pool;
        async move { kigumi_mod_sales::services::resolve_price(pool, plid, v, qty, &today).await.unwrap() }
    };

    // Most specific = variant rule → 700.
    assert_eq!(price(plid, d("1"), today.clone()).await, d("700"));
    // Drop the variant rule → product rule → 800.
    db.delete_secured(&item, &su, &[], &[], variant).await.unwrap();
    assert_eq!(price(plid, d("1"), today.clone()).await, d("800"));
    // Drop product → category rule (matched through the Electronics ancestor of Phones) → 900.
    db.delete_secured(&item, &su, &[], &[], prod).await.unwrap();
    assert_eq!(price(plid, d("1"), today.clone()).await, d("900"));
    // Drop category → global percentage off lst_price (1050 * 0.9) = 945.
    db.delete_secured(&item, &su, &[], &[], categ).await.unwrap();
    assert_eq!(price(plid, d("1"), today.clone()).await, d("945"));

    // A quantity tier: a global fixed rule at min_quantity 10 wins for qty >= 10, not below.
    ins(item.clone(), json!({ "pricelist_id": plid, "applied_on": "3_global", "min_quantity": "10", "compute_price": "fixed", "fixed_price": "500" })).await;
    assert_eq!(price(plid, d("1"), today.clone()).await, d("945"), "below the tier → percentage rule");
    assert_eq!(price(plid, d("10"), today.clone()).await, d("500"), "at the tier → fixed 500");

    // No pricelist rule at all → the variant's own sales price (lst_price 1050).
    let empty = ins(pricelist.clone(), json!({ "name": "Empty", "currency_id": cur })).await;
    assert_eq!(price(empty, d("1"), today.clone()).await, d("1050"));

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
