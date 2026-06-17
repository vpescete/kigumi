//! Mail subsystem polymorphic-integrity fix: deleting a mailed record cleans up its thread rows
//! (`mail_message` and friends), keyed by `(res_model, res_id)` with no FK. This is the guarantee
//! Odoo leaves to hand-written `unlink` overrides; Meshble enforces it on its single delete path.
//! A thread table that does not exist (mail module not migrated) is tolerated, not an error. Live PG.

use meshble_core::{
    resolve, Acl, Ctx, FieldDef, FieldKind, MailedRegistration, ModelDescriptor, ModelRegistration,
    ResolvedModel,
};
use meshble_db::Db;
use serde_json::json;

static DOC: ModelDescriptor = ModelDescriptor {
    name: "chat.doc",
    table: "chat_doc",
    fields: &[FieldDef {
        name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true,
        compute: None, depends: &[], default: None, unique: false, check: None,
    }],
};
fn doc_desc() -> &'static ModelDescriptor { &DOC }
meshble_core::inventory::submit! { ModelRegistration { name: "chat.doc", module: "test", descriptor: doc_desc } }
// Opt chat.doc into the mail subsystem, so delete_secured runs the thread cleanup for it.
meshble_core::inventory::submit! { MailedRegistration { model: "chat.doc" } }

static ACLS: &[Acl] = &[Acl { model: "chat.doc", group: "u", read: true, write: true, create: true, delete: true }];

#[tokio::test]
async fn deleting_mailed_record_cleans_its_thread_and_tolerates_missing_tables() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let doc: ResolvedModel = resolve(&DOC, &[]).unwrap();
    let su = Ctx::new(0, vec![]).sudo();

    // Fresh schema: the host table + a minimal mail_message (only the columns the cleanup touches).
    db.drop_table(&doc).await.unwrap();
    sqlx::query("DROP TABLE IF EXISTS mail_message").execute(db.pool()).await.unwrap();
    sqlx::query("DROP TABLE IF EXISTS mail_activity").execute(db.pool()).await.unwrap(); // ensure absent
    sqlx::query("DROP TABLE IF EXISTS mail_follower").execute(db.pool()).await.unwrap(); // ensure absent
    db.create_table(&doc).await.unwrap();
    sqlx::query("CREATE TABLE mail_message (id bigserial PRIMARY KEY, res_model text NOT NULL, res_id bigint NOT NULL, body text)")
        .execute(db.pool()).await.unwrap();

    // Two documents; messages on both.
    let keep = db.insert_secured(&doc, &su, ACLS, &[], json!({ "name": "keep" }).as_object().unwrap()).await.unwrap();
    let gone = db.insert_secured(&doc, &su, ACLS, &[], json!({ "name": "gone" }).as_object().unwrap()).await.unwrap();
    for (rid, body) in [(gone, "g1"), (gone, "g2"), (keep, "k1")] {
        sqlx::query("INSERT INTO mail_message (res_model, res_id, body) VALUES ('chat.doc', $1, $2)")
            .bind(rid).bind(body).execute(db.pool()).await.unwrap();
    }

    // Deleting `gone` removes its 2 messages; `keep`'s message is untouched. mail_activity /
    // mail_follower don't exist — the cleanup must tolerate that (42P01), not fail the delete.
    let n = db.delete_secured(&doc, &su, ACLS, &[], gone).await.unwrap();
    assert_eq!(n, 1, "the host row was deleted");

    let gone_msgs: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_message WHERE res_model='chat.doc' AND res_id=$1")
        .bind(gone).fetch_one(db.pool()).await.unwrap();
    assert_eq!(gone_msgs, 0, "deleted record's thread was cleaned up");

    let keep_msgs: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_message WHERE res_model='chat.doc' AND res_id=$1")
        .bind(keep).fetch_one(db.pool()).await.unwrap();
    assert_eq!(keep_msgs, 1, "other records' threads are untouched");

    // Now drop mail_message entirely and delete `keep`: cleanup hits a fully-missing table and must
    // still succeed (nothing to clean), proving the tolerance covers the not-yet-migrated case.
    sqlx::query("DROP TABLE mail_message").execute(db.pool()).await.unwrap();
    let n = db.delete_secured(&doc, &su, ACLS, &[], keep).await.unwrap();
    assert_eq!(n, 1, "delete succeeds even when no thread table exists");

    // Db::now() returns the DB clock (used to stamp messages).
    assert!(!db.now().await.unwrap().is_empty());

    db.drop_table(&doc).await.unwrap();
}
