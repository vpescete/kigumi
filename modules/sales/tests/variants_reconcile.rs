//! Idempotent reconciliation of the generate_variants service: regenerating after editing a template's
//! attribute lines keeps matching variants (same ids), creates only the missing combinations, ARCHIVES
//! (never deletes) the ones no longer selected, and REACTIVATES an archived variant if its combination is
//! re-selected. Against the REAL sales schema (product.product carries its own `active`, shadowing
//! product.template.active). Requires DATABASE_URL.

use std::collections::BTreeSet;

use kigumi::prelude::*;
use kigumi_db::Db;
use serde_json::json;

fn link() {
    let _ = (&kigumi_mod_sales::MANIFEST, &kigumi_mod_base::MANIFEST, &kigumi_mod_mail::MANIFEST);
}

async fn create(db: &Db, model: &ResolvedModel, su: &Ctx, v: serde_json::Value) -> i64 {
    db.insert_secured(model, su, &[], &[], v.as_object().unwrap()).await.unwrap()
}
async fn generate(db: &Db, tmpl: &ResolvedModel, su: &Ctx, t: i64) -> serde_json::Value {
    db.run_service(tmpl, su, &[], &[], t, "generate_variants", serde_json::Map::new()).await.unwrap()
}
fn n(out: &serde_json::Value, key: &str) -> usize {
    out[key].as_array().unwrap().len()
}
async fn ids_where(db: &Db, t: i64, extra: &str) -> BTreeSet<i64> {
    let sql = format!("SELECT id FROM product_product WHERE product_tmpl_id = $1 {extra}");
    sqlx::query_scalar::<_, i64>(&sql).bind(t).fetch_all(db.pool()).await.unwrap().into_iter().collect()
}

#[tokio::test]
async fn reconciles_keep_create_archive_reactivate() {
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

    let (attr, val, tmpl, line) = (
        resolve_registered("product.attribute").unwrap(),
        resolve_registered("product.attribute.value").unwrap(),
        resolve_registered("product.template").unwrap(),
        resolve_registered("product.template.attribute.line").unwrap(),
    );

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
    let g1 = generate(&db, &tmpl, &su, t).await;
    assert_eq!(n(&g1, "created"), 6);
    let original = ids_where(&db, t, "").await;
    assert_eq!(original.len(), 6);

    // Idempotent: regenerate unchanged → nothing created/archived, all 6 kept, same ids.
    let g2 = generate(&db, &tmpl, &su, t).await;
    assert!(n(&g2, "created") == 0 && n(&g2, "archived") == 0, "no-op regeneration");
    assert_eq!(n(&g2, "kept"), 6);
    assert_eq!(ids_where(&db, t, "").await, original, "no variant added or removed");

    // Add Size=L → only the 3 new (Color x L) combinations are created; the 6 originals untouched.
    db.update_secured(&line, &su, &[], &[], size_line, json!({ "value_ids": [s, mz, l] }).as_object().unwrap()).await.unwrap();
    let g3 = generate(&db, &tmpl, &su, t).await;
    assert_eq!(n(&g3, "created"), 3, "three new Color x L variants");
    assert_eq!(n(&g3, "kept"), 6);
    assert!(n(&g3, "archived") == 0);
    assert_eq!(ids_where(&db, t, "AND active").await.len(), 9);
    assert!(original.is_subset(&ids_where(&db, t, "").await), "original ids preserved");

    // Remove Color=Blue → the 3 variants with Blue (Blue x S/M/L) are ARCHIVED, not deleted; ids kept.
    db.update_secured(&line, &su, &[], &[], color_line, json!({ "value_ids": [red, green] }).as_object().unwrap()).await.unwrap();
    let g4 = generate(&db, &tmpl, &su, t).await;
    assert_eq!(n(&g4, "archived"), 3, "the three Blue variants archived");
    assert!(n(&g4, "created") == 0);
    assert_eq!(n(&g4, "kept"), 6);
    assert_eq!(ids_where(&db, t, "AND active").await.len(), 6, "six active remain");
    let archived = ids_where(&db, t, "AND NOT active").await;
    assert_eq!(archived.len(), 3, "three archived, still present (not deleted)");
    let total_after_archive = ids_where(&db, t, "").await.len();
    assert_eq!(total_after_archive, 9, "archived rows preserved");

    // Re-add Color=Blue → the 3 archived variants REACTIVATE (same ids); nothing new created.
    db.update_secured(&line, &su, &[], &[], color_line, json!({ "value_ids": [red, green, blue] }).as_object().unwrap()).await.unwrap();
    let g5 = generate(&db, &tmpl, &su, t).await;
    assert!(n(&g5, "created") == 0, "reactivation, not creation");
    assert!(n(&g5, "archived") == 0);
    assert_eq!(ids_where(&db, t, "AND active").await.len(), 9, "all nine active again");
    assert_eq!(ids_where(&db, t, "").await.len(), 9, "no new rows — the archived three were reused");
    assert!(archived.is_subset(&ids_where(&db, t, "AND active").await), "the same archived ids are active again");
}
