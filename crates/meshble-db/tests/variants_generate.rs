//! Slice 2 of the variant engine: `Db::generate_variants` builds the cartesian product of a
//! template's attribute lines into `product.product` rows, reusing one join row (PTAV) per cell and
//! capping the batch. This binary registers synthetic models under the EXACT names the engine
//! resolves; `product.product` has no `inherits` registration here (the sales macro emits it), so the
//! test exercises the cartesian / PTAV-reuse / cap / atomicity mechanics — the real _inherits
//! no-duplicate-template behaviour is covered by the live smoke. Live Postgres.

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
const fn int(name: &'static str) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Integer, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}
const fn m2m(name: &'static str, target: &'static str, relation: &'static str, column: &'static str, target_column: &'static str) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Many2many { target, relation, column, target_column }, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None }
}

static ATTRIBUTE: ModelDescriptor = ModelDescriptor {
    name: "product.attribute",
    table: "product_attribute",
    fields: &[txt("name", true), FieldDef { name: "create_variant", label: "create_variant", kind: FieldKind::Selection(&[("always", "Instantly"), ("no_variant", "Never")]), required: false, stored: true, compute: None, depends: &[], default: Some("always"), unique: false, check: None }],
};
static ATTR_VALUE: ModelDescriptor = ModelDescriptor {
    name: "product.attribute.value",
    table: "product_attribute_value",
    fields: &[txt("name", true), m2o("attribute_id", "product.attribute", true), int("sequence")],
};
static TEMPLATE: ModelDescriptor = ModelDescriptor {
    name: "product.template",
    table: "product_template",
    fields: &[txt("name", true)],
};
static VARIANT: ModelDescriptor = ModelDescriptor {
    name: "product.product",
    table: "product_product",
    fields: &[
        m2o("product_tmpl_id", "product.template", true),
        FieldDef { name: "active", label: "active", kind: FieldKind::Bool, required: false, stored: true, compute: None, depends: &[], default: Some("true"), unique: false, check: None },
        FieldDef { name: "price_extra", label: "Extra", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[], default: Some("0"), unique: false, check: None },
        m2m("product_template_attribute_value_ids", "product.template.attribute.value", "variant_ptav_rel", "product_id", "ptav_id"),
    ],
};
static LINE: ModelDescriptor = ModelDescriptor {
    name: "product.template.attribute.line",
    table: "product_template_attribute_line",
    fields: &[
        m2o("product_tmpl_id", "product.template", true),
        m2o("attribute_id", "product.attribute", true),
        m2m("value_ids", "product.attribute.value", "ptal_value_rel", "line_id", "value_id"),
    ],
};
static PTAV: ModelDescriptor = ModelDescriptor {
    name: "product.template.attribute.value",
    table: "product_template_attribute_value",
    fields: &[
        m2o("product_tmpl_id", "product.template", true),
        m2o("attribute_line_id", "product.template.attribute.line", true),
        m2o("product_attribute_value_id", "product.attribute.value", true),
        FieldDef { name: "price_extra", label: "Extra", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[], default: Some("0"), unique: false, check: None },
    ],
};
fn attribute_desc() -> &'static ModelDescriptor { &ATTRIBUTE }
fn attr_value_desc() -> &'static ModelDescriptor { &ATTR_VALUE }
fn template_desc() -> &'static ModelDescriptor { &TEMPLATE }
fn variant_desc() -> &'static ModelDescriptor { &VARIANT }
fn line_desc() -> &'static ModelDescriptor { &LINE }
fn ptav_desc() -> &'static ModelDescriptor { &PTAV }
meshble_core::inventory::submit! { ModelRegistration { name: "product.attribute", module: "test", descriptor: attribute_desc } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.attribute.value", module: "test", descriptor: attr_value_desc } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.template", module: "test", descriptor: template_desc } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.product", module: "test", descriptor: variant_desc } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.template.attribute.line", module: "test", descriptor: line_desc } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.template.attribute.value", module: "test", descriptor: ptav_desc } }

fn m(d: &'static ModelDescriptor) -> ResolvedModel {
    resolve(d, &[]).unwrap()
}

async fn create(db: &Db, model: &ResolvedModel, su: &Ctx, v: serde_json::Value) -> i64 {
    db.insert_secured(model, su, &[], &[], v.as_object().unwrap()).await.unwrap()
}

/// Combo identity = the set of attribute-VALUE ids behind a variant's PTAV set (resolve each PTAV to
/// its product_attribute_value_id). Used to assert the 6 combos are distinct.
async fn combo_of(db: &Db, variant: &ResolvedModel, ptav: &ResolvedModel, su: &Ctx, vid: i64) -> BTreeSet<i64> {
    let row = db.find_one_secured(variant, su, &[], &[], vid).await.unwrap().unwrap();
    let ptavs = row["product_template_attribute_value_ids"].as_array().unwrap();
    let mut out = BTreeSet::new();
    for p in ptavs {
        let prow = db.find_one_secured(ptav, su, &[], &[], p.as_i64().unwrap()).await.unwrap().unwrap();
        out.insert(prow["product_attribute_value_id"].as_i64().unwrap());
    }
    out
}

#[tokio::test]
async fn generates_cartesian_product_reuses_join_rows_and_caps() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();
    let (attr, val, tmpl, variant, line, ptav) =
        (m(&ATTRIBUTE), m(&ATTR_VALUE), m(&TEMPLATE), m(&VARIANT), m(&LINE), m(&PTAV));

    for j in ["variant_ptav_rel", "ptal_value_rel"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS {j}")).execute(db.pool()).await.unwrap();
    }
    for d in [&ptav, &line, &variant, &tmpl, &val, &attr] {
        db.drop_table(d).await.unwrap();
    }
    for d in [&attr, &val, &tmpl, &variant, &line, &ptav] {
        db.create_table(d).await.unwrap();
    }
    db.create_m2m_relations(&variant).await.unwrap(); // variant_ptav_rel
    db.create_m2m_relations(&line).await.unwrap(); // ptal_value_rel

    // Two attributes: Color (3 values) x Size (2 values) → 6 variants, 5 distinct cells.
    let color = create(&db, &attr, &su, json!({ "name": "Color", "create_variant": "always" })).await;
    let size = create(&db, &attr, &su, json!({ "name": "Size", "create_variant": "always" })).await;
    let mut color_vals = Vec::new();
    for c in ["Red", "Green", "Blue"] {
        color_vals.push(create(&db, &val, &su, json!({ "name": c, "attribute_id": color })).await);
    }
    let mut size_vals = Vec::new();
    for s in ["S", "M"] {
        size_vals.push(create(&db, &val, &su, json!({ "name": s, "attribute_id": size })).await);
    }
    let t = create(&db, &tmpl, &su, json!({ "name": "Shirt" })).await;
    create(&db, &line, &su, json!({ "product_tmpl_id": t, "attribute_id": color, "value_ids": color_vals })).await;
    create(&db, &line, &su, json!({ "product_tmpl_id": t, "attribute_id": size, "value_ids": size_vals })).await;

    let out = db.generate_variants(&su, &[], &[], t).await.unwrap();
    assert_eq!(out.created.len(), 6, "3 x 2 = 6 variants");
    assert!(out.archived.is_empty() && out.kept.is_empty(), "slice 2 is create-only");

    // The template was not duplicated (the variants link the one existing template).
    let n_tmpl: i64 = sqlx::query_scalar("SELECT count(*) FROM product_template").fetch_one(db.pool()).await.unwrap();
    assert_eq!(n_tmpl, 1, "one template, no duplicate");

    // Join rows are reused per cell, not per variant: 3 + 2 = 5, not 6 x 2 = 12.
    let n_ptav: i64 = sqlx::query_scalar("SELECT count(*) FROM product_template_attribute_value").fetch_one(db.pool()).await.unwrap();
    assert_eq!(n_ptav, 5, "one PTAV per (line,value) cell, reused across combos");

    // Every variant points at the template, carries exactly two cells (one per attribute), and the 6
    // combos are all distinct.
    let mut combos = BTreeSet::new();
    for &vid in &out.created {
        let row = db.find_one_secured(&variant, &su, &[], &[], vid).await.unwrap().unwrap();
        assert_eq!(row["product_tmpl_id"].as_i64(), Some(t));
        assert_eq!(row["product_template_attribute_value_ids"].as_array().unwrap().len(), 2);
        combos.insert(combo_of(&db, &variant, &ptav, &su, vid).await);
    }
    assert_eq!(combos.len(), 6, "all six combinations are distinct");

    // Cap: a template whose product exceeds MAX_VARIANTS (1000) is rejected before any write.
    let big = create(&db, &tmpl, &su, json!({ "name": "Huge" })).await;
    for a in 0..3 {
        let attr_id = create(&db, &attr, &su, json!({ "name": format!("A{a}"), "create_variant": "always" })).await;
        let mut vals = Vec::new();
        for v in 0..11 {
            vals.push(create(&db, &val, &su, json!({ "name": format!("v{a}_{v}"), "attribute_id": attr_id })).await);
        }
        create(&db, &line, &su, json!({ "product_tmpl_id": big, "attribute_id": attr_id, "value_ids": vals })).await;
    }
    let err = db.generate_variants(&su, &[], &[], big).await; // 11^3 = 1331 > 1000
    assert!(err.is_err(), "over-cap generation is rejected");
    let n_big: i64 = sqlx::query_scalar("SELECT count(*) FROM product_product WHERE product_tmpl_id = $1").bind(big).fetch_one(db.pool()).await.unwrap();
    assert_eq!(n_big, 0, "nothing was created for the rejected template");

    for j in ["variant_ptav_rel", "ptal_value_rel"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS {j}")).execute(db.pool()).await.unwrap();
    }
    for d in [&ptav, &line, &variant, &tmpl, &val, &attr] {
        db.drop_table(d).await.unwrap();
    }
}
