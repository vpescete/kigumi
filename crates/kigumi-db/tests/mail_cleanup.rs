//! Mail subsystem polymorphic-integrity fix: deleting a mailed record cleans up its thread rows
//! (`mail_message` and friends), keyed by `(res_model, res_id)` with no FK. This is the guarantee
//! Odoo leaves to hand-written `unlink` overrides; Kigumi enforces it on its single delete path.
//! A thread table that does not exist (mail module not migrated) is tolerated, not an error. Live PG.

use kigumi_core::{
    resolve, Acl, Ctx, FieldDef, FieldKind, MailedRegistration, ModelDescriptor, ModelRegistration,
    ResolvedModel,
};
use kigumi_db::Db;
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
kigumi_core::inventory::submit! { ModelRegistration { name: "chat.doc", module: "test", descriptor: doc_desc } }
// Opt chat.doc into the mail subsystem, so delete_secured runs the thread cleanup for it.
kigumi_core::inventory::submit! { MailedRegistration { model: "chat.doc" } }

static ACLS: &[Acl] = &[Acl { model: "chat.doc", group: "u", read: true, write: true, create: true, delete: true }];

/// Counts rows for `sql_prefix` + a bound `$1` id (e.g. `"SELECT count(*) FROM t WHERE res_id="`).
async fn count(pool: &sqlx::PgPool, sql_prefix: &str, id: i64) -> i64 {
    sqlx::query_scalar(&format!("{sql_prefix}$1")).bind(id).fetch_one(pool).await.unwrap()
}

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

    // Fresh schema: host table + minimal mail_message/mail_activity/mail_tracking (cleanup-touched
    // columns only). mail_follower is deliberately NOT created → exercises the 42P01 tolerance too.
    db.drop_table(&doc).await.unwrap();
    for t in ["mail_tracking", "mail_message", "mail_activity", "mail_follower"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS {t}")).execute(db.pool()).await.unwrap();
    }
    db.create_table(&doc).await.unwrap();
    sqlx::query("CREATE TABLE mail_message (id bigserial PRIMARY KEY, res_model text NOT NULL, res_id bigint NOT NULL, body text)").execute(db.pool()).await.unwrap();
    sqlx::query("CREATE TABLE mail_activity (id bigserial PRIMARY KEY, res_model text NOT NULL, res_id bigint NOT NULL, summary text)").execute(db.pool()).await.unwrap();
    sqlx::query("CREATE TABLE mail_tracking (id bigserial PRIMARY KEY, message_id bigint NOT NULL, field text NOT NULL, old_value text, new_value text)").execute(db.pool()).await.unwrap();

    // Two documents; messages, activities and (via a message) tracking on both.
    let keep = db.insert_secured(&doc, &su, ACLS, &[], json!({ "name": "keep" }).as_object().unwrap()).await.unwrap();
    let gone = db.insert_secured(&doc, &su, ACLS, &[], json!({ "name": "gone" }).as_object().unwrap()).await.unwrap();
    let mut gone_msg = 0i64;
    for (rid, body) in [(gone, "g1"), (gone, "g2"), (keep, "k1")] {
        let mid: i64 = sqlx::query_scalar("INSERT INTO mail_message (res_model, res_id, body) VALUES ('chat.doc', $1, $2) RETURNING id")
            .bind(rid).bind(body).fetch_one(db.pool()).await.unwrap();
        if rid == gone { gone_msg = mid; }
    }
    for rid in [gone, gone, keep] {
        sqlx::query("INSERT INTO mail_activity (res_model, res_id, summary) VALUES ('chat.doc', $1, 'todo')").bind(rid).execute(db.pool()).await.unwrap();
    }
    sqlx::query("INSERT INTO mail_tracking (message_id, field, old_value, new_value) VALUES ($1, 'state', 'a', 'b')").bind(gone_msg).execute(db.pool()).await.unwrap();

    // Deleting `gone` removes its messages, activities AND tracking (the last via its messages);
    // `keep`'s rows are untouched. mail_follower doesn't exist — cleanup must tolerate that (42P01).
    let n = db.delete_secured(&doc, &su, ACLS, &[], gone).await.unwrap();
    assert_eq!(n, 1, "the host row was deleted");

    let gone_msgs = count(db.pool(), "SELECT count(*) FROM mail_message WHERE res_model='chat.doc' AND res_id=", gone).await;
    assert_eq!(gone_msgs, 0, "deleted record's messages were cleaned up");
    let gone_acts = count(db.pool(), "SELECT count(*) FROM mail_activity WHERE res_model='chat.doc' AND res_id=", gone).await;
    assert_eq!(gone_acts, 0, "deleted record's activities were cleaned up");
    let track_total: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_tracking").fetch_one(db.pool()).await.unwrap();
    assert_eq!(track_total, 0, "tracking rows removed via the deleted record's messages");

    let keep_msgs = count(db.pool(), "SELECT count(*) FROM mail_message WHERE res_model='chat.doc' AND res_id=", keep).await;
    assert_eq!(keep_msgs, 1, "other records' messages are untouched");
    let keep_acts = count(db.pool(), "SELECT count(*) FROM mail_activity WHERE res_model='chat.doc' AND res_id=", keep).await;
    assert_eq!(keep_acts, 1, "other records' activities are untouched");

    // Now drop mail_message entirely and delete `keep`: cleanup hits a fully-missing table and must
    // still succeed (nothing to clean), proving the tolerance covers the not-yet-migrated case.
    sqlx::query("DROP TABLE mail_message").execute(db.pool()).await.unwrap();
    let n = db.delete_secured(&doc, &su, ACLS, &[], keep).await.unwrap();
    assert_eq!(n, 1, "delete succeeds even when no thread table exists");

    // Db::now() returns the DB clock (used to stamp messages).
    assert!(!db.now().await.unwrap().is_empty());

    db.drop_table(&doc).await.unwrap();
}
