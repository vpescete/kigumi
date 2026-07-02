//! res.country / res.country.state master data: a partner references a structured country + state
//! (Many2one), not just free text. The models migrate (no FK cycle) and resolve on a partner. Requires
//! DATABASE_URL.

use kigumi::prelude::*;
use kigumi_db::Db;
use serde_json::json;

fn link() {
    let _ = &kigumi_mod_base::MANIFEST;
}

#[tokio::test]
async fn partner_references_a_structured_country_and_state() {
    link();

    // Registry facts (pure): the new models migrate.
    let migrated: Vec<&str> = migration_plan().unwrap().iter().map(|t| t.model.name).collect();
    assert!(migrated.contains(&"res.country"), "res.country is migrated");
    assert!(migrated.contains(&"res.country.state"), "res.country.state is migrated");

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

    let (country, state, partner) = (
        resolve_registered("res.country").unwrap(),
        resolve_registered("res.country.state").unwrap(),
        resolve_registered("res.partner").unwrap(),
    );

    let it = db.insert_secured(&country, &su, &[], &[], json!({ "name": "Italy", "code": "IT", "active": true }).as_object().unwrap()).await.unwrap();
    let lom = db.insert_secured(&state, &su, &[], &[], json!({ "name": "Lombardy", "code": "LOM", "country_id": it, "active": true }).as_object().unwrap()).await.unwrap();

    let p = db.insert_secured(&partner, &su, &[], &[], json!({
        "name": "ACME Italia", "country_id": it, "state_id": lom, "city": "Milan"
    }).as_object().unwrap()).await.unwrap();

    let read = db.find_one_secured(&partner, &su, &[], &[], p).await.unwrap().unwrap();
    assert_eq!(read["country_id"].as_i64(), Some(it), "partner carries the structured country");
    assert_eq!(read["state_id"].as_i64(), Some(lom), "and the state");

    // The state belongs to the country (referential link holds).
    let s = db.find_one_secured(&state, &su, &[], &[], lom).await.unwrap().unwrap();
    assert_eq!(s["country_id"].as_i64(), Some(it));

    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
