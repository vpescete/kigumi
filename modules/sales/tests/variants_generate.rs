//! The variant generation engine (relocated to modules/sales as the `generate_variants` service on
//! product.template): building a template's attribute lines into `product.product` rows is the cartesian
//! product, reusing one join row (PTAV) per cell and capping the batch. Exercised against the REAL sales
//! product schema (product.product _inherits product.template), through `Db::run_service` the way the HTTP
//! route dispatches it. Requires DATABASE_URL.

use std::collections::BTreeSet;

use meshble::prelude::*;
use meshble_db::Db;
use serde_json::json;

fn link() {
    let _ = (&meshble_mod_sales::MANIFEST, &meshble_mod_base::MANIFEST, &meshble_mod_mail::MANIFEST);
}

async fn create(db: &Db, model: &ResolvedModel, su: &Ctx, v: serde_json::Value) -> i64 {
    db.insert_secured(model, su, &[], &[], v.as_object().unwrap()).await.unwrap()
}

/// Runs the generate_variants service on the template, returning its `{created,archived,kept}` JSON.
async fn generate(db: &Db, tmpl: &ResolvedModel, su: &Ctx, t: i64) -> serde_json::Value {
    db.run_service(tmpl, su, &[], &[], t, "generate_variants", serde_json::Map::new()).await.unwrap()
}
fn ids(out: &serde_json::Value, key: &str) -> Vec<i64> {
    out[key].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect()
}

/// Combo identity = the set of attribute-VALUE ids behind a variant's PTAV set. Used to assert the 6 combos
/// are distinct.
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
    link();
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();

    let plan = migration_plan().unwrap();
    for t in plan.iter().rev() {
        db.drop_table(&t.model).await.unwrap();
    }
    for t in &plan {
        db.create_table(&t.model).await.unwrap();
    }
    for t in &plan {
        db.create_m2m_relations(&t.model).await.unwrap();
    }

    let (attr, val, tmpl, variant, line, ptav) = (
        resolve_registered("product.attribute").unwrap(),
        resolve_registered("product.attribute.value").unwrap(),
        resolve_registered("product.template").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("product.template.attribute.line").unwrap(),
        resolve_registered("product.template.attribute.value").unwrap(),
    );

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

    let out = generate(&db, &tmpl, &su, t).await;
    let created = ids(&out, "created");
    assert_eq!(created.len(), 6, "3 x 2 = 6 variants");
    assert!(ids(&out, "archived").is_empty() && ids(&out, "kept").is_empty(), "first generation is create-only");

    // The template was not duplicated (the variants link the one existing template via _inherits).
    let n_tmpl: i64 = sqlx::query_scalar("SELECT count(*) FROM product_template").fetch_one(db.pool()).await.unwrap();
    assert_eq!(n_tmpl, 1, "one template, no duplicate");

    // Join rows are reused per cell, not per variant: 3 + 2 = 5, not 6 x 2 = 12.
    let n_ptav: i64 = sqlx::query_scalar("SELECT count(*) FROM product_template_attribute_value").fetch_one(db.pool()).await.unwrap();
    assert_eq!(n_ptav, 5, "one PTAV per (line,value) cell, reused across combos");

    // Every variant points at the template, carries exactly two cells (one per attribute), and the 6
    // combos are all distinct.
    let mut combos = BTreeSet::new();
    for &vid in &created {
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
    let err = db
        .run_service(&tmpl, &su, &[], &[], big, "generate_variants", serde_json::Map::new())
        .await; // 11^3 = 1331 > 1000
    assert!(err.is_err(), "over-cap generation is rejected");
    let n_big: i64 = sqlx::query_scalar("SELECT count(*) FROM product_product WHERE product_tmpl_id = $1").bind(big).fetch_one(db.pool()).await.unwrap();
    assert_eq!(n_big, 0, "nothing was created for the rejected template");
}
