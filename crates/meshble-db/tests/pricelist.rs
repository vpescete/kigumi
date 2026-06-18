//! resolve_price: the most-specific applicable rule wins (variant > product > category > global), the
//! category match walks the product's category ancestry, min_quantity tiers apply, and the price is a
//! fixed amount or a percentage off the base (the variant's lst_price = list_price + price_extra, or
//! its cost). Synthetic models under the engine's exact names. Live Postgres.

use meshble_core::{resolve, Acl, Ctx, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, ResolvedModel};
use meshble_db::Db;
use rust_decimal::Decimal;
use serde_json::json;
use std::str::FromStr;

const fn txt(n: &'static str) -> FieldDef { FieldDef { name: n, label: n, kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None } }
const fn m2o(n: &'static str, t: &'static str) -> FieldDef { FieldDef { name: n, label: n, kind: FieldKind::Many2one { target: t }, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None } }
const fn dec(n: &'static str) -> FieldDef { FieldDef { name: n, label: n, kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[], default: Some("0"), unique: false, check: None } }
const fn sel(n: &'static str, opts: &'static [(&'static str, &'static str)], d: &'static str) -> FieldDef { FieldDef { name: n, label: n, kind: FieldKind::Selection(opts), required: false, stored: true, compute: None, depends: &[], default: Some(d), unique: false, check: None } }
const fn date(n: &'static str) -> FieldDef { FieldDef { name: n, label: n, kind: FieldKind::Date, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None } }

static CATEGORY: ModelDescriptor = ModelDescriptor { name: "product.category", table: "product_category", fields: &[txt("name"), m2o("parent_id", "product.category")] };
static TEMPLATE: ModelDescriptor = ModelDescriptor { name: "product.template", table: "product_template", fields: &[txt("name"), dec("list_price"), dec("standard_price"), m2o("categ_id", "product.category")] };
static VARIANT: ModelDescriptor = ModelDescriptor { name: "product.product", table: "product_product", fields: &[m2o("product_tmpl_id", "product.template"), dec("price_extra")] };
static PRICELIST: ModelDescriptor = ModelDescriptor { name: "product.pricelist", table: "product_pricelist", fields: &[txt("name")] };
static ITEM: ModelDescriptor = ModelDescriptor {
    name: "product.pricelist.item",
    table: "product_pricelist_item",
    fields: &[
        m2o("pricelist_id", "product.pricelist"),
        sel("applied_on", &[("0_product_variant", "V"), ("1_product", "P"), ("2_product_category", "C"), ("3_global", "G")], "3_global"),
        m2o("categ_id", "product.category"),
        m2o("product_tmpl_id", "product.template"),
        m2o("product_id", "product.product"),
        dec("min_quantity"),
        sel("compute_price", &[("fixed", "F"), ("percentage", "%")], "fixed"),
        dec("fixed_price"),
        dec("percent_price"),
        sel("base", &[("list_price", "L"), ("standard_price", "C")], "list_price"),
        date("date_start"),
        date("date_end"),
    ],
};
fn dc() -> &'static ModelDescriptor { &CATEGORY }
fn dt() -> &'static ModelDescriptor { &TEMPLATE }
fn dv() -> &'static ModelDescriptor { &VARIANT }
fn dp() -> &'static ModelDescriptor { &PRICELIST }
fn di() -> &'static ModelDescriptor { &ITEM }
meshble_core::inventory::submit! { ModelRegistration { name: "product.category", module: "test", descriptor: dc } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.template", module: "test", descriptor: dt } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.product", module: "test", descriptor: dv } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.pricelist", module: "test", descriptor: dp } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.pricelist.item", module: "test", descriptor: di } }

static ACLS: &[Acl] = &[
    Acl { model: "product.category", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "product.template", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "product.product", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "product.pricelist", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "product.pricelist.item", group: "u", read: true, write: true, create: true, delete: true },
];
fn m(d: &'static ModelDescriptor) -> ResolvedModel { resolve(d, &[]).unwrap() }
fn d(s: &str) -> Decimal { Decimal::from_str(s).unwrap() }

#[tokio::test]
async fn resolve_price_picks_the_most_specific_rule() {
    let url = match std::env::var("DATABASE_URL") { Ok(u) => u, Err(_) => { eprintln!("skipping"); return; } };
    let db = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();
    let (cat, tpl, var, pl, item) = (m(&CATEGORY), m(&TEMPLATE), m(&VARIANT), m(&PRICELIST), m(&ITEM));
    for x in [&item, &var, &tpl, &pl, &cat] { db.drop_table(x).await.unwrap(); }
    for x in [&cat, &tpl, &var, &pl, &item] { db.create_table(x).await.unwrap(); }
    let ins = |model: &ResolvedModel, v: serde_json::Value| {
        let model = model.clone();
        let db = &db;
        let su = &su;
        async move { db.insert_secured(&model, su, ACLS, &[], v.as_object().unwrap()).await.unwrap() }
    };

    let electronics = ins(&cat, json!({ "name": "Electronics" })).await;
    let phones = ins(&cat, json!({ "name": "Phones", "parent_id": electronics })).await;
    let phone = ins(&tpl, json!({ "name": "Phone", "list_price": "1000", "standard_price": "600", "categ_id": phones })).await;
    let v = ins(&var, json!({ "product_tmpl_id": phone, "price_extra": "50" })).await; // lst_price = 1050
    let plid = ins(&pl, json!({ "name": "Public" })).await;

    // Four overlapping rules, increasingly specific.
    let global = ins(&item, json!({ "pricelist_id": plid, "applied_on": "3_global", "compute_price": "percentage", "percent_price": "10", "base": "list_price" })).await; // 10% off lst_price
    let categ = ins(&item, json!({ "pricelist_id": plid, "applied_on": "2_product_category", "categ_id": electronics, "compute_price": "fixed", "fixed_price": "900" })).await; // matches via ancestor
    let product = ins(&item, json!({ "pricelist_id": plid, "applied_on": "1_product", "product_tmpl_id": phone, "compute_price": "fixed", "fixed_price": "800" })).await;
    let variant = ins(&item, json!({ "pricelist_id": plid, "applied_on": "0_product_variant", "product_id": v, "compute_price": "fixed", "fixed_price": "700" })).await;

    let today = db.today().await.unwrap();

    // Most specific = variant rule → 700.
    assert_eq!(db.resolve_price(plid, v, d("1"), &today).await.unwrap(), d("700"));
    // Drop the variant rule → product rule → 800.
    db.delete_secured(&item, &su, ACLS, &[], variant).await.unwrap();
    assert_eq!(db.resolve_price(plid, v, d("1"), &today).await.unwrap(), d("800"));
    // Drop product → category rule (matched through the Electronics ancestor of Phones) → 900.
    db.delete_secured(&item, &su, ACLS, &[], product).await.unwrap();
    assert_eq!(db.resolve_price(plid, v, d("1"), &today).await.unwrap(), d("900"));
    // Drop category → global percentage off lst_price (1050 * 0.9) = 945.
    db.delete_secured(&item, &su, ACLS, &[], categ).await.unwrap();
    assert_eq!(db.resolve_price(plid, v, d("1"), &today).await.unwrap(), d("945"));

    // A quantity tier: a global fixed rule at min_quantity 10 wins for qty >= 10, not below.
    let _ = global;
    ins(&item, json!({ "pricelist_id": plid, "applied_on": "3_global", "min_quantity": "10", "compute_price": "fixed", "fixed_price": "500" })).await;
    assert_eq!(db.resolve_price(plid, v, d("1"), &today).await.unwrap(), d("945"), "below the tier → percentage rule");
    assert_eq!(db.resolve_price(plid, v, d("10"), &today).await.unwrap(), d("500"), "at the tier → fixed 500");

    // No pricelist rule at all → the variant's own sales price (lst_price 1050).
    let empty = ins(&pl, json!({ "name": "Empty" })).await;
    assert_eq!(db.resolve_price(empty, v, d("1"), &today).await.unwrap(), d("1050"));

    for x in [&item, &var, &tpl, &pl, &cat] { db.drop_table(x).await.unwrap(); }
}
