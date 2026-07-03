//! Followers: the composite UNIQUE index (res_model, res_id, user_id) makes following idempotent —
//! a duplicate subscribe surfaces as a typed Conflict (the follow endpoint treats it as success).
//! A record's followers are cleaned up when the record is deleted. Live Postgres.

use kigumi_core::{
    resolve, Acl, FieldDef, FieldKind, MailedRegistration, ModelDescriptor, ModelRegistration,
    ResolvedModel,
};
use kigumi_db::DbError;
use serde_json::json;

static DOC: ModelDescriptor = ModelDescriptor {
    name: "fol.doc",
    table: "fol_doc",
    fields: &[FieldDef {
        name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true,
        compute: None, depends: &[], default: None, unique: false, check: None,
    }],
};
fn doc_desc() -> &'static ModelDescriptor { &DOC }
kigumi_core::inventory::submit! { ModelRegistration { name: "fol.doc", module: "test", descriptor: doc_desc } }
kigumi_core::inventory::submit! { MailedRegistration { model: "fol.doc" } }

static FOLL: &[Acl] = &[Acl { model: "mail.follower", group: "u", read: true, write: true, create: true, delete: true }];
static DOCACL: &[Acl] = &[Acl { model: "fol.doc", group: "u", read: true, write: true, create: true, delete: true }];

#[tokio::test]
async fn following_is_idempotent_and_cleaned_up_on_delete() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let doc: ResolvedModel = resolve(&DOC, &[]).unwrap();
    // mail.follower as the server resolves it (the real model shape).
    let foll: ResolvedModel = resolve_registered_follower();
    let su = kigumi_test::su();

    sqlx::query("DROP TABLE IF EXISTS mail_follower").execute(db.pool()).await.unwrap();
    db.create_table(&foll).await.unwrap();
    db.ensure_mail_indexes().await.unwrap(); // creates the composite UNIQUE index

    let host = db.insert_secured(&doc, &su, DOCACL, &[], json!({ "name": "doc" }).as_object().unwrap()).await.unwrap();

    // First follow succeeds; an identical second follow violates the unique index → typed Conflict.
    let vals = json!({ "res_model": "fol.doc", "res_id": host, "user_id": 5 });
    db.insert_secured(&foll, &su, FOLL, &[], vals.as_object().unwrap()).await.unwrap();
    let dup = db.insert_secured(&foll, &su, FOLL, &[], vals.as_object().unwrap()).await;
    assert!(matches!(dup, Err(DbError::Conflict(_))), "duplicate follow is a Conflict, got {dup:?}");

    // A different user can follow the same record.
    db.insert_secured(&foll, &su, FOLL, &[], json!({ "res_model": "fol.doc", "res_id": host, "user_id": 6 }).as_object().unwrap()).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_follower WHERE res_model='fol.doc' AND res_id=$1")
        .bind(host).fetch_one(db.pool()).await.unwrap();
    assert_eq!(n, 2);

    // Deleting the record removes its followers (cleanup over the polymorphic link).
    db.delete_secured(&doc, &su, DOCACL, &[], host).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_follower WHERE res_model='fol.doc' AND res_id=$1")
        .bind(host).fetch_one(db.pool()).await.unwrap();
    assert_eq!(n, 0, "the deleted record's followers were cleaned up");

    sqlx::query("DROP TABLE IF EXISTS mail_follower").execute(db.pool()).await.unwrap();
}

/// The mail.follower model shape (mirrors modules/mail), resolved without linking the mail crate.
fn resolve_registered_follower() -> ResolvedModel {
    static M: ModelDescriptor = ModelDescriptor {
        name: "mail.follower",
        table: "mail_follower",
        fields: &[
            FieldDef { name: "res_model", label: "Document Model", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
            FieldDef { name: "res_id", label: "Document ID", kind: FieldKind::Integer, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
            FieldDef { name: "user_id", label: "Follower", kind: FieldKind::Integer, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        ],
    };
    resolve(&M, &[]).unwrap()
}
