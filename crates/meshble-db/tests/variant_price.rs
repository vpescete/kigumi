//! M15.1 variant pricing end to end: the generation engine MATERIALIZES product.product.price_extra =
//! sum of its combo PTAVs' price_extra (a Many2many aggregate stored, since the compute engine can't do
//! it on read); editing a PTAV's price_extra REFRESHES every variant that includes it (P4 hook); and
//! lst_price is a same-record on-read compute = delegated template list_price + the variant's own
//! materialized price_extra. Uses the exact product model names the engine resolves. Live Postgres.

use meshble_core::{Acl, ComputeInput, Ctx, FieldDef, FieldKind, InheritsRegistration, ModelDescriptor, ModelRegistration, Value};
use meshble_db::Db;
use serde_json::json;

fn vt_lst_price(i: &ComputeInput) -> Value {
    Value::Decimal(i.decimal("list_price") + i.decimal("price_extra"))
}
meshble_core::inventory::submit! { meshble_core::ComputeRegistration { name: "vt_lst_price", func: vt_lst_price } }

const fn txt(name: &'static str, required: bool) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Text, required, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}
const fn m2o(name: &'static str, target: &'static str, required: bool) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Many2one { target }, required, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}
const fn dec(name: &'static str, default: Option<&'static str>) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[], default, unique: false, check: None }
}
const fn m2m(name: &'static str, target: &'static str, relation: &'static str, column: &'static str, target_column: &'static str) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Many2many { target, relation, column, target_column }, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None }
}

static TEMPLATE: ModelDescriptor = ModelDescriptor { name: "product.template", table: "product_template", fields: &[txt("name", true), dec("list_price", Some("0"))] };
static VARIANT: ModelDescriptor = ModelDescriptor {
    name: "product.product",
    table: "product_product",
    fields: &[
        m2o("product_tmpl_id", "product.template", true),
        FieldDef { name: "active", label: "active", kind: FieldKind::Bool, required: false, stored: true, compute: None, depends: &[], default: Some("true"), unique: false, check: None },
        dec("price_extra", Some("0")),
        FieldDef { name: "lst_price", label: "lst_price", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: false, compute: Some("vt_lst_price"), depends: &["list_price", "price_extra"], default: None, unique: false, check: None },
        m2m("product_template_attribute_value_ids", "product.template.attribute.value", "variant_ptav_rel", "product_id", "ptav_id"),
    ],
};
static ATTRIBUTE: ModelDescriptor = ModelDescriptor { name: "product.attribute", table: "product_attribute", fields: &[txt("name", true), FieldDef { name: "create_variant", label: "create_variant", kind: FieldKind::Selection(&[("always", "A"), ("no_variant", "N")]), required: false, stored: true, compute: None, depends: &[], default: Some("always"), unique: false, check: None }] };
static ATTR_VALUE: ModelDescriptor = ModelDescriptor { name: "product.attribute.value", table: "product_attribute_value", fields: &[txt("name", true), m2o("attribute_id", "product.attribute", true)] };
static LINE: ModelDescriptor = ModelDescriptor {
    name: "product.template.attribute.line", table: "product_template_attribute_line",
    fields: &[m2o("product_tmpl_id", "product.template", true), m2o("attribute_id", "product.attribute", true), m2m("value_ids", "product.attribute.value", "ptal_value_rel", "line_id", "value_id")],
};
static PTAV: ModelDescriptor = ModelDescriptor {
    name: "product.template.attribute.value", table: "product_template_attribute_value",
    fields: &[m2o("product_tmpl_id", "product.template", true), m2o("attribute_line_id", "product.template.attribute.line", true), m2o("product_attribute_value_id", "product.attribute.value", true), dec("price_extra", Some("0"))],
};
fn dt() -> &'static ModelDescriptor { &TEMPLATE }
fn dv() -> &'static ModelDescriptor { &VARIANT }
fn da() -> &'static ModelDescriptor { &ATTRIBUTE }
fn dav() -> &'static ModelDescriptor { &ATTR_VALUE }
fn dl() -> &'static ModelDescriptor { &LINE }
fn dp() -> &'static ModelDescriptor { &PTAV }
meshble_core::inventory::submit! { ModelRegistration { name: "product.template", module: "test", descriptor: dt } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.product", module: "test", descriptor: dv } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.attribute", module: "test", descriptor: da } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.attribute.value", module: "test", descriptor: dav } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.template.attribute.line", module: "test", descriptor: dl } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.template.attribute.value", module: "test", descriptor: dp } }
meshble_core::inventory::submit! { InheritsRegistration { model: "product.product", parent: "product.template", via: "product_tmpl_id" } }

static ACLS: &[Acl] = &[
    Acl { model: "product.template", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "product.product", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "product.attribute", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "product.attribute.value", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "product.template.attribute.line", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "product.template.attribute.value", group: "u", read: true, write: true, create: true, delete: true },
];

fn m(d: &'static ModelDescriptor) -> meshble_core::ResolvedModel { meshble_core::resolve(d, &[]).unwrap() }

#[tokio::test]
async fn price_extra_materializes_and_lst_price_derives() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let db = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();
    let (tmpl, variant, attr, val, line, ptav) = (m(&TEMPLATE), m(&VARIANT), m(&ATTRIBUTE), m(&ATTR_VALUE), m(&LINE), m(&PTAV));

    for j in ["variant_ptav_rel", "ptal_value_rel"] { sqlx::query(&format!("DROP TABLE IF EXISTS {j}")).execute(db.pool()).await.unwrap(); }
    for d in [&ptav, &line, &variant, &tmpl, &val, &attr] { db.drop_table(d).await.unwrap(); }
    for d in [&attr, &val, &tmpl, &variant, &line, &ptav] { db.create_table(d).await.unwrap(); }
    db.create_m2m_relations(&variant).await.unwrap();
    db.create_m2m_relations(&line).await.unwrap();

    let color = db.insert_secured(&attr, &su, ACLS, &[], json!({ "name": "Color" }).as_object().unwrap()).await.unwrap();
    let red = db.insert_secured(&val, &su, ACLS, &[], json!({ "name": "Red", "attribute_id": color }).as_object().unwrap()).await.unwrap();
    let blue = db.insert_secured(&val, &su, ACLS, &[], json!({ "name": "Blue", "attribute_id": color }).as_object().unwrap()).await.unwrap();
    // Template with a base list_price; the variant delegates it.
    let t = db.insert_secured(&tmpl, &su, ACLS, &[], json!({ "name": "Shirt", "list_price": "100" }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&line, &su, ACLS, &[], json!({ "product_tmpl_id": t, "attribute_id": color, "value_ids": [red, blue] }).as_object().unwrap()).await.unwrap();

    // Generate: 2 variants, each with one PTAV at price_extra 0, so materialized price_extra = 0.
    let g = db.generate_variants(&su, ACLS, &[], t).await.unwrap();
    assert_eq!(g.created.len(), 2);
    for vid in &g.created {
        let row = db.find_one_secured(&variant, &su, ACLS, &[], *vid).await.unwrap().unwrap();
        assert_eq!(row["price_extra"].as_str(), Some("0"), "fresh PTAVs => 0 extra");
        // lst_price = delegated list_price (100) + price_extra (0).
        assert_eq!(row["lst_price"].as_str(), Some("100"), "lst_price = list_price + extra");
        assert_eq!(row["list_price"].as_str(), Some("100"), "delegated base price");
    }

    // Edit the RED PTAV's price_extra to 15 — the P4 hook re-materializes the Red variant's price_extra.
    let red_ptav: i64 = sqlx::query_scalar(
        "SELECT p.id FROM product_template_attribute_value p WHERE p.product_attribute_value_id = $1"
    ).bind(red).fetch_one(db.pool()).await.unwrap();
    db.update_secured(&ptav, &su, ACLS, &[], red_ptav, json!({ "price_extra": "15" }).as_object().unwrap()).await.unwrap();

    // The variant that includes the Red PTAV now has price_extra 15 and lst_price 115; the Blue one is unchanged.
    let red_variant: i64 = sqlx::query_scalar("SELECT product_id FROM variant_ptav_rel WHERE ptav_id = $1").bind(red_ptav).fetch_one(db.pool()).await.unwrap();
    let row = db.find_one_secured(&variant, &su, ACLS, &[], red_variant).await.unwrap().unwrap();
    assert_eq!(row["price_extra"].as_str(), Some("15"), "P4 hook refreshed the Red variant");
    assert_eq!(row["lst_price"].as_str(), Some("115"), "lst_price tracks the new extra");

    // Invariant: every variant's stored price_extra equals the SUM of its combo PTAVs' price_extra.
    let mismatches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM product_product v WHERE v.price_extra <> COALESCE(\
            (SELECT SUM(p.price_extra) FROM variant_ptav_rel r JOIN product_template_attribute_value p ON p.id = r.ptav_id WHERE r.product_id = v.id), 0)"
    ).fetch_one(db.pool()).await.unwrap();
    assert_eq!(mismatches, 0, "materialization invariant holds for every variant");

    // Regeneration is a full refresh and idempotent (price_extra stays correct, no drift).
    db.generate_variants(&su, ACLS, &[], t).await.unwrap();
    let row = db.find_one_secured(&variant, &su, ACLS, &[], red_variant).await.unwrap().unwrap();
    assert_eq!(row["price_extra"].as_str(), Some("15"), "regeneration keeps the extra (idempotent)");

    for j in ["variant_ptav_rel", "ptal_value_rel"] { sqlx::query(&format!("DROP TABLE IF EXISTS {j}")).execute(db.pool()).await.unwrap(); }
    for d in [&ptav, &line, &variant, &tmpl, &val, &attr] { db.drop_table(d).await.unwrap(); }
}
