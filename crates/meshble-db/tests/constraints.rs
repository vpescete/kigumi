//! Write correctness (M3): field defaults applied on create, single-column UNIQUE → Conflict (409),
//! and a CHECK constraint → BadInput (400). Live Postgres.

use meshble_core::{
    resolve, Ctx, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, ResolvedModel,
};
use meshble_db::{Db, DbError};

static ITEM: ModelDescriptor = ModelDescriptor {
    name: "cst.item",
    table: "cst_item",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: true, check: None },
        FieldDef { name: "qty", label: "Qty", kind: FieldKind::Integer, required: false, stored: true, compute: None, depends: &[], default: Some("1"), unique: false, check: Some("qty >= 0") },
        FieldDef { name: "state", label: "State", kind: FieldKind::Selection(&[("draft", "Draft"), ("done", "Done")]), required: false, stored: true, compute: None, depends: &[], default: Some("draft"), unique: false, check: None },
        FieldDef { name: "active", label: "Active", kind: FieldKind::Bool, required: false, stored: true, compute: None, depends: &[], default: Some("true"), unique: false, check: None },
    ],
};
fn item_desc() -> &'static ModelDescriptor {
    &ITEM
}
meshble_core::inventory::submit! { ModelRegistration { name: "cst.item", module: "test", descriptor: item_desc } }

fn model() -> ResolvedModel {
    resolve(&ITEM, &[]).unwrap()
}

#[tokio::test]
async fn defaults_unique_and_check() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let m = model();
    let su = Ctx::new(0, vec![]).sudo();
    db.drop_table(&m).await.unwrap();
    db.create_table(&m).await.unwrap();

    // Create with only the required field → defaults fill qty=1, state=draft, active=true.
    let id = db.insert_secured(&m, &su, &[], &[], serde_json::json!({ "name": "a" }).as_object().unwrap()).await.unwrap();
    let got = db.find_one_secured(&m, &su, &[], &[], id).await.unwrap().unwrap();
    assert_eq!(got["qty"].as_i64().unwrap(), 1, "default qty");
    assert_eq!(got["state"], "draft", "default state");
    assert_eq!(got["active"], true, "default active");

    // A duplicate of the UNIQUE name → Conflict (HTTP 409).
    let dup = db.insert_secured(&m, &su, &[], &[], serde_json::json!({ "name": "a" }).as_object().unwrap()).await;
    assert!(matches!(dup, Err(DbError::Conflict(_))), "duplicate unique value → Conflict, got {dup:?}");

    // A value violating the CHECK (qty >= 0) → BadInput (HTTP 400), not an opaque 500.
    let bad = db.insert_secured(&m, &su, &[], &[], serde_json::json!({ "name": "b", "qty": -5 }).as_object().unwrap()).await;
    assert!(matches!(bad, Err(DbError::BadInput(_))), "check violation → BadInput, got {bad:?}");

    // An explicit value overrides the default.
    let id2 = db.insert_secured(&m, &su, &[], &[], serde_json::json!({ "name": "c", "qty": 7, "state": "done" }).as_object().unwrap()).await.unwrap();
    let g2 = db.find_one_secured(&m, &su, &[], &[], id2).await.unwrap().unwrap();
    assert_eq!(g2["qty"].as_i64().unwrap(), 7);
    assert_eq!(g2["state"], "done");

    db.drop_table(&m).await.unwrap();
}
