//! First-class Many2many: a junction table holds the N↔N membership; the field reads as an array of
//! target ids and writes with SET semantics (the array replaces the membership). FK ON DELETE CASCADE
//! cleans the junction when a target is removed. Live Postgres.

use kigumi_core::{resolve, Acl, Ctx, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, ResolvedModel};
use kigumi_db::Db;
use serde_json::json;

static POST: ModelDescriptor = ModelDescriptor {
    name: "mm.post",
    table: "mm_post",
    fields: &[
        FieldDef { name: "title", label: "Title", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "tag_ids", label: "Tags", kind: FieldKind::Many2many { target: "mm.tag", relation: "mm_post_tag_rel", column: "post_id", target_column: "tag_id" }, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
static TAG: ModelDescriptor = ModelDescriptor {
    name: "mm.tag",
    table: "mm_tag",
    fields: &[FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
};
fn post_desc() -> &'static ModelDescriptor { &POST }
fn tag_desc() -> &'static ModelDescriptor { &TAG }
kigumi_core::inventory::submit! { ModelRegistration { name: "mm.post", module: "test", descriptor: post_desc } }
kigumi_core::inventory::submit! { ModelRegistration { name: "mm.tag", module: "test", descriptor: tag_desc } }

static ACLS: &[Acl] = &[
    Acl { model: "mm.post", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "mm.tag", group: "u", read: true, write: true, create: true, delete: true },
];

fn m(d: &'static ModelDescriptor) -> ResolvedModel {
    resolve(d, &[]).unwrap()
}

fn ids(v: &serde_json::Value) -> Vec<i64> {
    v["tag_ids"].as_array().unwrap().iter().map(|x| x.as_i64().unwrap()).collect()
}

#[tokio::test]
async fn many2many_set_read_modify_and_cascade() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let (post, tag) = (m(&POST), m(&TAG));
    let su = Ctx::new(0, vec![]).sudo();

    sqlx::query("DROP TABLE IF EXISTS mm_post_tag_rel").execute(db.pool()).await.unwrap();
    db.drop_table(&post).await.unwrap();
    db.drop_table(&tag).await.unwrap();
    db.create_table(&tag).await.unwrap();
    db.create_table(&post).await.unwrap();
    db.create_m2m_relations(&post).await.unwrap(); // junction needs both tables to exist

    let t1 = db.insert_secured(&tag, &su, ACLS, &[], json!({ "name": "a" }).as_object().unwrap()).await.unwrap();
    let t2 = db.insert_secured(&tag, &su, ACLS, &[], json!({ "name": "b" }).as_object().unwrap()).await.unwrap();
    let t3 = db.insert_secured(&tag, &su, ACLS, &[], json!({ "name": "c" }).as_object().unwrap()).await.unwrap();

    // Create with a set of tags.
    let pid = db.insert_secured(&post, &su, ACLS, &[], json!({ "title": "p", "tag_ids": [t1, t2] }).as_object().unwrap()).await.unwrap();
    assert_eq!(ids(&db.find_one_secured(&post, &su, ACLS, &[], pid).await.unwrap().unwrap()), vec![t1, t2]);

    // SET semantics: replace membership (t1 removed, t3 added).
    db.update_secured(&post, &su, ACLS, &[], pid, json!({ "tag_ids": [t2, t3] }).as_object().unwrap()).await.unwrap();
    assert_eq!(ids(&db.find_one_secured(&post, &su, ACLS, &[], pid).await.unwrap().unwrap()), vec![t2, t3]);

    // A scalar-only update leaves the relation untouched.
    db.update_secured(&post, &su, ACLS, &[], pid, json!({ "title": "p2" }).as_object().unwrap()).await.unwrap();
    assert_eq!(ids(&db.find_one_secured(&post, &su, ACLS, &[], pid).await.unwrap().unwrap()), vec![t2, t3]);

    // Empty array clears the relation.
    db.update_secured(&post, &su, ACLS, &[], pid, json!({ "tag_ids": [] }).as_object().unwrap()).await.unwrap();
    assert!(ids(&db.find_one_secured(&post, &su, ACLS, &[], pid).await.unwrap().unwrap()).is_empty());

    // A non-existent target id is rejected (FK).
    assert!(db.update_secured(&post, &su, ACLS, &[], pid, json!({ "tag_ids": [999999] }).as_object().unwrap()).await.is_err());

    // ON DELETE CASCADE: deleting a tag removes it from every post's membership.
    db.update_secured(&post, &su, ACLS, &[], pid, json!({ "tag_ids": [t2, t3] }).as_object().unwrap()).await.unwrap();
    db.delete_secured(&tag, &su, ACLS, &[], t2).await.unwrap();
    assert_eq!(ids(&db.find_one_secured(&post, &su, ACLS, &[], pid).await.unwrap().unwrap()), vec![t3]);

    sqlx::query("DROP TABLE IF EXISTS mm_post_tag_rel").execute(db.pool()).await.unwrap();
    db.drop_table(&post).await.unwrap();
    db.drop_table(&tag).await.unwrap();
}
