//! `_inherits` slice 3: transparent WRITE through to the parent. Creating a child without the `via`
//! FK auto-creates the parent (with the delegated fields) first, atomically; an existing `via` updates
//! that shared parent; updating a child's delegated field writes the shared template (every sibling
//! sees it). Required delegated fields are enforced at the child create boundary. Live Postgres.

use meshble_core::{
    resolve_registered, Acl, Ctx, FieldDef, FieldKind, InheritsRegistration, ModelDescriptor,
    ModelRegistration, ResolvedModel,
};
use meshble_db::{Db, DbError};
use serde_json::json;

static TPL: ModelDescriptor = ModelDescriptor {
    name: "wr.tpl",
    table: "wr_tpl",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "list_price", label: "Price", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
static VAR: ModelDescriptor = ModelDescriptor {
    name: "wr.var",
    table: "wr_var",
    fields: &[
        FieldDef { name: "tpl_id", label: "Template", kind: FieldKind::Many2one { target: "wr.tpl" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "default_code", label: "Ref", kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
fn tpl() -> &'static ModelDescriptor { &TPL }
fn var() -> &'static ModelDescriptor { &VAR }
meshble_core::inventory::submit! { ModelRegistration { name: "wr.tpl", module: "test", descriptor: tpl } }
meshble_core::inventory::submit! { ModelRegistration { name: "wr.var", module: "test", descriptor: var } }
meshble_core::inventory::submit! { InheritsRegistration { model: "wr.var", parent: "wr.tpl", via: "tpl_id" } }

static ACLS: &[Acl] = &[
    Acl { model: "wr.tpl", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "wr.var", group: "u", read: true, write: true, create: true, delete: true },
];

async fn count(db: &Db, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}")).fetch_one(db.pool()).await.unwrap()
}

#[tokio::test]
async fn write_splits_to_parent_and_auto_creates() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let (tpl, var): (ResolvedModel, ResolvedModel) =
        (resolve_registered("wr.tpl").unwrap(), resolve_registered("wr.var").unwrap());
    let su = Ctx::new(0, vec![]).sudo();

    db.drop_table(&var).await.unwrap();
    db.drop_table(&tpl).await.unwrap();
    db.create_table(&tpl).await.unwrap();
    db.create_table(&var).await.unwrap();

    // 1) Create a variant WITHOUT tpl_id → the parent template is auto-created with the delegated
    //    fields, atomically; the variant's via points at it.
    let v1 = db.insert_secured(&var, &su, ACLS, &[], json!({
        "default_code": "V1", "name": "Widget", "list_price": 9.99
    }).as_object().unwrap()).await.unwrap();
    assert_eq!(count(&db, "wr_tpl").await, 1, "exactly one template auto-created");
    let row = db.find_one_secured(&var, &su, ACLS, &[], v1).await.unwrap().unwrap();
    assert_eq!(row["name"], json!("Widget"));
    assert_eq!(row["list_price"], json!("9.99"));
    let tpl_id = row["tpl_id"].as_i64().unwrap();

    // 2) Create a SECOND variant pointing at the same template (via given, delegated absent).
    let v2 = db.insert_secured(&var, &su, ACLS, &[], json!({
        "default_code": "V2", "tpl_id": tpl_id
    }).as_object().unwrap()).await.unwrap();
    assert_eq!(count(&db, "wr_tpl").await, 1, "no new template — shared");
    assert_eq!(db.find_one_secured(&var, &su, ACLS, &[], v2).await.unwrap().unwrap()["name"], json!("Widget"));

    // 3) Update v1's delegated field (delegated-only write) → writes the SHARED template; v2 sees it.
    db.update_secured(&var, &su, ACLS, &[], v1, json!({ "name": "Widget Pro" }).as_object().unwrap()).await.unwrap();
    assert_eq!(db.find_one_secured(&var, &su, ACLS, &[], v2).await.unwrap().unwrap()["name"], json!("Widget Pro"), "shared template update visible on the sibling");

    // 4) Mixed write: own field + delegated field in one update.
    db.update_secured(&var, &su, ACLS, &[], v1, json!({ "default_code": "V1b", "list_price": 12.5 }).as_object().unwrap()).await.unwrap();
    let row = db.find_one_secured(&var, &su, ACLS, &[], v1).await.unwrap().unwrap();
    assert_eq!(row["default_code"], json!("V1b"), "own field updated");
    assert_eq!(row["list_price"], json!("12.5"), "delegated field updated on the template");

    // 5) Required delegated field missing on auto-create → clean error, nothing inserted.
    let before = count(&db, "wr_tpl").await;
    let err = db.insert_secured(&var, &su, ACLS, &[], json!({ "default_code": "V3" }).as_object().unwrap()).await;
    assert!(matches!(err, Err(DbError::BadInput(_))), "required parent field enforced: {err:?}");
    assert_eq!(count(&db, "wr_tpl").await, before, "failed create rolled back — no orphan template");

    db.drop_table(&var).await.unwrap();
    db.drop_table(&tpl).await.unwrap();
}
