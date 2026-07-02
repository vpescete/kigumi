//! Variant pricing end to end: the generate_variants service MATERIALIZES product.product.price_extra =
//! sum of its combo PTAVs' price_extra (a Many2many aggregate stored, since the compute engine can't do it
//! on read); editing a PTAV's price_extra REFRESHES every variant that includes it (now via the module's
//! registered write trigger, formerly the in-core M15.1 hook); and lst_price is a same-record on-read
//! compute = delegated template list_price + the variant's own materialized price_extra. Against the REAL
//! sales schema, through Db::run_service. Requires DATABASE_URL.

use kigumi::prelude::*;
use kigumi_db::Db;
use serde_json::json;

fn link() {
    let _ = (&kigumi_mod_sales::MANIFEST, &kigumi_mod_base::MANIFEST, &kigumi_mod_mail::MANIFEST);
}

async fn create(db: &Db, model: &ResolvedModel, su: &Ctx, v: serde_json::Value) -> i64 {
    db.insert_secured(model, su, &[], &[], v.as_object().unwrap()).await.unwrap()
}

#[tokio::test]
async fn price_extra_materializes_and_lst_price_derives() {
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

    let (tmpl, variant, attr, val, line, ptav) = (
        resolve_registered("product.template").unwrap(),
        resolve_registered("product.product").unwrap(),
        resolve_registered("product.attribute").unwrap(),
        resolve_registered("product.attribute.value").unwrap(),
        resolve_registered("product.template.attribute.line").unwrap(),
        resolve_registered("product.template.attribute.value").unwrap(),
    );

    let color = create(&db, &attr, &su, json!({ "name": "Color" })).await;
    let red = create(&db, &val, &su, json!({ "name": "Red", "attribute_id": color })).await;
    let blue = create(&db, &val, &su, json!({ "name": "Blue", "attribute_id": color })).await;
    // Template with a base list_price; the variant delegates it via _inherits.
    let t = create(&db, &tmpl, &su, json!({ "name": "Shirt", "list_price": "100" })).await;
    create(&db, &line, &su, json!({ "product_tmpl_id": t, "attribute_id": color, "value_ids": [red, blue] })).await;

    // Generate: 2 variants, each with one PTAV at price_extra 0, so materialized price_extra = 0.
    let g = db.run_service(&tmpl, &su, &[], &[], t, "generate_variants", serde_json::Map::new()).await.unwrap();
    let created: Vec<i64> = g["created"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
    assert_eq!(created.len(), 2);
    for vid in &created {
        let row = db.find_one_secured(&variant, &su, &[], &[], *vid).await.unwrap().unwrap();
        assert_eq!(row["price_extra"].as_str(), Some("0"), "fresh PTAVs => 0 extra");
        // lst_price = delegated list_price (100) + price_extra (0).
        assert_eq!(row["lst_price"].as_str(), Some("100"), "lst_price = list_price + extra");
        assert_eq!(row["list_price"].as_str(), Some("100"), "delegated base price");
    }

    // Edit the RED PTAV's price_extra to 15 — the write trigger re-materializes the Red variant's price_extra.
    let red_ptav: i64 = sqlx::query_scalar(
        "SELECT p.id FROM product_template_attribute_value p WHERE p.product_attribute_value_id = $1"
    ).bind(red).fetch_one(db.pool()).await.unwrap();
    db.update_secured(&ptav, &su, &[], &[], red_ptav, json!({ "price_extra": "15" }).as_object().unwrap()).await.unwrap();

    // The variant that includes the Red PTAV now has price_extra 15 and lst_price 115; the Blue one is unchanged.
    let red_variant: i64 = sqlx::query_scalar("SELECT product_id FROM variant_ptav_rel WHERE ptav_id = $1").bind(red_ptav).fetch_one(db.pool()).await.unwrap();
    let row = db.find_one_secured(&variant, &su, &[], &[], red_variant).await.unwrap().unwrap();
    assert_eq!(row["price_extra"].as_str(), Some("15"), "write trigger refreshed the Red variant");
    assert_eq!(row["lst_price"].as_str(), Some("115"), "lst_price tracks the new extra");

    // Invariant: every variant's stored price_extra equals the SUM of its combo PTAVs' price_extra.
    let mismatches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM product_product v WHERE v.price_extra <> COALESCE(\
            (SELECT SUM(p.price_extra) FROM variant_ptav_rel r JOIN product_template_attribute_value p ON p.id = r.ptav_id WHERE r.product_id = v.id), 0)"
    ).fetch_one(db.pool()).await.unwrap();
    assert_eq!(mismatches, 0, "materialization invariant holds for every variant");

    // Regeneration is a full refresh and idempotent (price_extra stays correct, no drift).
    db.run_service(&tmpl, &su, &[], &[], t, "generate_variants", serde_json::Map::new()).await.unwrap();
    let row = db.find_one_secured(&variant, &su, &[], &[], red_variant).await.unwrap().unwrap();
    assert_eq!(row["price_extra"].as_str(), Some("15"), "regeneration keeps the extra (idempotent)");
}
