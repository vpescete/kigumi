//! Slice 3: idempotent reconciliation. Regenerating after editing a template's attribute lines keeps
//! matching variants (same ids), creates only the missing combinations, ARCHIVES (never deletes) the
//! ones no longer selected, and REACTIVATES an archived variant if its combination is re-selected.
//! Synthetic models under the engine's exact names; `product.product` carries its own `active` (the
//! shadow of `product.template.active`). Live Postgres.

use std::collections::BTreeSet;

use meshble_core::{resolve, Ctx, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, ResolvedModel};
use meshble_db::Db;
use serde_json::json;

const fn txt(name: &'static str, required: bool) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Text, required, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}
const fn m2o(name: &'static str, target: &'static str, required: bool) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Many2one { target }, required, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}
const fn m2m(name: &'static str, target: &'static str, relation: &'static str, column: &'static str, target_column: &'static str) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Many2many { target, relation, column, target_column }, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None }
}

static ATTRIBUTE: ModelDescriptor = ModelDescriptor { name: "product.attribute", table: "product_attribute", fields: &[txt("name", true), FieldDef { name: "create_variant", label: "create_variant", kind: FieldKind::Selection(&[("always", "Instantly"), ("no_variant", "Never")]), required: false, stored: true, compute: None, depends: &[], default: Some("always"), unique: false, check: None }] };
static ATTR_VALUE: ModelDescriptor = ModelDescriptor { name: "product.attribute.value", table: "product_attribute_value", fields: &[txt("name", true), m2o("attribute_id", "product.attribute", true)] };
static TEMPLATE: ModelDescriptor = ModelDescriptor { name: "product.template", table: "product_template", fields: &[txt("name", true)] };
static VARIANT: ModelDescriptor = ModelDescriptor {
    name: "product.product",
    table: "product_product",
    fields: &[
        m2o("product_tmpl_id", "product.template", true),
        FieldDef { name: "active", label: "active", kind: FieldKind::Bool, required: false, stored: true, compute: None, depends: &[], default: Some("true"), unique: false, check: None },
        m2m("product_template_attribute_value_ids", "product.template.attribute.value", "variant_ptav_rel", "product_id", "ptav_id"),
    ],
};
static LINE: ModelDescriptor = ModelDescriptor {
    name: "product.template.attribute.line",
    table: "product_template_attribute_line",
    fields: &[m2o("product_tmpl_id", "product.template", true), m2o("attribute_id", "product.attribute", true), m2m("value_ids", "product.attribute.value", "ptal_value_rel", "line_id", "value_id")],
};
static PTAV: ModelDescriptor = ModelDescriptor {
    name: "product.template.attribute.value",
    table: "product_template_attribute_value",
    fields: &[m2o("product_tmpl_id", "product.template", true), m2o("attribute_line_id", "product.template.attribute.line", true), m2o("product_attribute_value_id", "product.attribute.value", true)],
};
fn d_attr() -> &'static ModelDescriptor { &ATTRIBUTE }
fn d_val() -> &'static ModelDescriptor { &ATTR_VALUE }
fn d_tmpl() -> &'static ModelDescriptor { &TEMPLATE }
fn d_var() -> &'static ModelDescriptor { &VARIANT }
fn d_line() -> &'static ModelDescriptor { &LINE }
fn d_ptav() -> &'static ModelDescriptor { &PTAV }
meshble_core::inventory::submit! { ModelRegistration { name: "product.attribute", module: "test", descriptor: d_attr } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.attribute.value", module: "test", descriptor: d_val } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.template", module: "test", descriptor: d_tmpl } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.product", module: "test", descriptor: d_var } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.template.attribute.line", module: "test", descriptor: d_line } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.template.attribute.value", module: "test", descriptor: d_ptav } }

fn m(d: &'static ModelDescriptor) -> ResolvedModel { resolve(d, &[]).unwrap() }

async fn create(db: &Db, model: &ResolvedModel, su: &Ctx, v: serde_json::Value) -> i64 {
    db.insert_secured(model, su, &[], &[], v.as_object().unwrap()).await.unwrap()
}
async fn ids_where(db: &Db, t: i64, extra: &str) -> BTreeSet<i64> {
    let sql = format!("SELECT id FROM product_product WHERE product_tmpl_id = $1 {extra}");
    sqlx::query_scalar::<_, i64>(&sql).bind(t).fetch_all(db.pool()).await.unwrap().into_iter().collect()
}

#[tokio::test]
async fn reconciles_keep_create_archive_reactivate() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let db = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();
    let (attr, val, tmpl, variant, line, ptav) = (m(&ATTRIBUTE), m(&ATTR_VALUE), m(&TEMPLATE), m(&VARIANT), m(&LINE), m(&PTAV));

    for j in ["variant_ptav_rel", "ptal_value_rel"] { sqlx::query(&format!("DROP TABLE IF EXISTS {j}")).execute(db.pool()).await.unwrap(); }
    for d in [&ptav, &line, &variant, &tmpl, &val, &attr] { db.drop_table(d).await.unwrap(); }
    for d in [&attr, &val, &tmpl, &variant, &line, &ptav] { db.create_table(d).await.unwrap(); }
    db.create_m2m_relations(&variant).await.unwrap();
    db.create_m2m_relations(&line).await.unwrap();

    let color = create(&db, &attr, &su, json!({ "name": "Color" })).await;
    let size = create(&db, &attr, &su, json!({ "name": "Size" })).await;
    let red = create(&db, &val, &su, json!({ "name": "Red", "attribute_id": color })).await;
    let green = create(&db, &val, &su, json!({ "name": "Green", "attribute_id": color })).await;
    let blue = create(&db, &val, &su, json!({ "name": "Blue", "attribute_id": color })).await;
    let s = create(&db, &val, &su, json!({ "name": "S", "attribute_id": size })).await;
    let mz = create(&db, &val, &su, json!({ "name": "M", "attribute_id": size })).await;
    let l = create(&db, &val, &su, json!({ "name": "L", "attribute_id": size })).await;
    let t = create(&db, &tmpl, &su, json!({ "name": "Shirt" })).await;
    let color_line = create(&db, &line, &su, json!({ "product_tmpl_id": t, "attribute_id": color, "value_ids": [red, green, blue] })).await;
    let size_line = create(&db, &line, &su, json!({ "product_tmpl_id": t, "attribute_id": size, "value_ids": [s, mz] })).await;

    // First generation: 3 x 2 = 6.
    let g1 = db.generate_variants(&su, &[], &[], t).await.unwrap();
    assert_eq!(g1.created.len(), 6);
    let original = ids_where(&db, t, "").await;
    assert_eq!(original.len(), 6);

    // Idempotent: regenerate unchanged → nothing created/archived, all 6 kept, same ids.
    let g2 = db.generate_variants(&su, &[], &[], t).await.unwrap();
    assert!(g2.created.is_empty() && g2.archived.is_empty(), "no-op regeneration");
    assert_eq!(g2.kept.len(), 6);
    assert_eq!(ids_where(&db, t, "").await, original, "no variant added or removed");

    // Add Size=L → only the 3 new (Color x L) combinations are created; the 6 originals untouched.
    db.update_secured(&line, &su, &[], &[], size_line, json!({ "value_ids": [s, mz, l] }).as_object().unwrap()).await.unwrap();
    let g3 = db.generate_variants(&su, &[], &[], t).await.unwrap();
    assert_eq!(g3.created.len(), 3, "three new Color x L variants");
    assert_eq!(g3.kept.len(), 6);
    assert!(g3.archived.is_empty());
    assert_eq!(ids_where(&db, t, "AND active").await.len(), 9);
    assert!(original.is_subset(&ids_where(&db, t, "").await), "original ids preserved");

    // Remove Color=Blue → the 3 variants with Blue (Blue x S/M/L) are ARCHIVED, not deleted; ids kept.
    db.update_secured(&line, &su, &[], &[], color_line, json!({ "value_ids": [red, green] }).as_object().unwrap()).await.unwrap();
    let g4 = db.generate_variants(&su, &[], &[], t).await.unwrap();
    assert_eq!(g4.archived.len(), 3, "the three Blue variants archived");
    assert!(g4.created.is_empty());
    assert_eq!(g4.kept.len(), 6);
    assert_eq!(ids_where(&db, t, "AND active").await.len(), 6, "six active remain");
    let archived = ids_where(&db, t, "AND NOT active").await;
    assert_eq!(archived.len(), 3, "three archived, still present (not deleted)");
    let total_after_archive = ids_where(&db, t, "").await.len();
    assert_eq!(total_after_archive, 9, "archived rows preserved");

    // Re-add Color=Blue → the 3 archived variants REACTIVATE (same ids); nothing new created.
    db.update_secured(&line, &su, &[], &[], color_line, json!({ "value_ids": [red, green, blue] }).as_object().unwrap()).await.unwrap();
    let g5 = db.generate_variants(&su, &[], &[], t).await.unwrap();
    assert!(g5.created.is_empty(), "reactivation, not creation");
    assert!(g5.archived.is_empty());
    assert_eq!(ids_where(&db, t, "AND active").await.len(), 9, "all nine active again");
    assert_eq!(ids_where(&db, t, "").await.len(), 9, "no new rows — the archived three were reused");
    assert!(archived.is_subset(&ids_where(&db, t, "AND active").await), "the same archived ids are active again");

    for j in ["variant_ptav_rel", "ptal_value_rel"] { sqlx::query(&format!("DROP TABLE IF EXISTS {j}")).execute(db.pool()).await.unwrap(); }
    for d in [&ptav, &line, &variant, &tmpl, &val, &attr] { db.drop_table(d).await.unwrap(); }
}
