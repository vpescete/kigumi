//! Field tracking: changing a `#[field(tracked)]` field on a mailed model records a `notification`
//! message + a typed `mail.tracking` row (old → new). No-op writes and non-tracked fields record
//! nothing; tracking is best-effort (a missing mail schema never fails the business write). Live PG.

use meshble_core::{
    resolve, Acl, Ctx, FieldDef, FieldKind, MailedRegistration, ModelDescriptor, ModelRegistration,
    ResolvedModel, TrackedFieldRegistration,
};
use meshble_db::Db;
use serde_json::json;

static DOC: ModelDescriptor = ModelDescriptor {
    name: "tr.doc",
    table: "tr_doc",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "state", label: "State", kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "due_at", label: "When", kind: FieldKind::Datetime, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
fn doc_desc() -> &'static ModelDescriptor { &DOC }
meshble_core::inventory::submit! { ModelRegistration { name: "tr.doc", module: "test", descriptor: doc_desc } }
meshble_core::inventory::submit! { MailedRegistration { model: "tr.doc" } }
// `state` and `due_at` are tracked; `name` is not.
meshble_core::inventory::submit! { TrackedFieldRegistration { model: "tr.doc", field: "state" } }
meshble_core::inventory::submit! { TrackedFieldRegistration { model: "tr.doc", field: "due_at" } }

static ACLS: &[Acl] = &[Acl { model: "tr.doc", group: "u", read: true, write: true, create: true, delete: true }];

async fn count(db: &Db, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

#[tokio::test]
async fn tracked_field_change_records_a_notification_and_tracking_row() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let doc: ResolvedModel = resolve(&DOC, &[]).unwrap();
    let su = Ctx::new(7, vec![]).sudo(); // uid 7 → author of the tracking message

    db.drop_table(&doc).await.unwrap();
    sqlx::query("DROP TABLE IF EXISTS mail_tracking").execute(db.pool()).await.unwrap();
    sqlx::query("DROP TABLE IF EXISTS mail_message").execute(db.pool()).await.unwrap();
    db.create_table(&doc).await.unwrap();
    sqlx::query("CREATE TABLE mail_message (id bigserial PRIMARY KEY, res_model text NOT NULL, res_id bigint NOT NULL, author_id bigint, message_type text, body text, date timestamptz, parent_id bigint)")
        .execute(db.pool()).await.unwrap();
    sqlx::query("CREATE TABLE mail_tracking (id bigserial PRIMARY KEY, message_id bigint NOT NULL, field text NOT NULL, old_value text, new_value text)")
        .execute(db.pool()).await.unwrap();

    let id = db.insert_secured(&doc, &su, ACLS, &[], json!({ "name": "Doc", "state": "draft", "due_at": "2026-06-17T10:00:00Z" }).as_object().unwrap()).await.unwrap();

    // 1) Change the tracked field → one notification message + one tracking row (draft → sale).
    db.update_secured(&doc, &su, ACLS, &[], id, json!({ "state": "sale" }).as_object().unwrap()).await.unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM mail_message WHERE res_model='tr.doc' AND message_type='notification'").await, 1);
    assert_eq!(count(&db, "SELECT count(*) FROM mail_tracking").await, 1);
    let (field, old, new): (String, Option<String>, Option<String>) =
        sqlx::query_as("SELECT field, old_value, new_value FROM mail_tracking ORDER BY id DESC LIMIT 1")
            .fetch_one(db.pool()).await.unwrap();
    assert_eq!(field, "state");
    assert_eq!(old.as_deref(), Some("draft"));
    assert_eq!(new.as_deref(), Some("sale"));
    // The message author is the writer (uid 7).
    assert_eq!(count(&db, "SELECT count(*) FROM mail_message WHERE author_id=7 AND message_type='notification'").await, 1);

    // 2) No-op write of the same value → no new audit entry.
    db.update_secured(&doc, &su, ACLS, &[], id, json!({ "state": "sale" }).as_object().unwrap()).await.unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM mail_message WHERE message_type='notification'").await, 1, "unchanged value records nothing");

    // 3) Writing a NON-tracked field → no audit entry.
    db.update_secured(&doc, &su, ACLS, &[], id, json!({ "name": "Renamed" }).as_object().unwrap()).await.unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM mail_message WHERE message_type='notification'").await, 1, "non-tracked field records nothing");

    // 3b) Datetime no-op: re-writing the SAME instant records nothing. Old (`::text`) and new (re-read
    //     `::text`) go through identical Postgres rendering, so `2026-06-17T10:00:00Z` does not look
    //     "changed" vs the stored `2026-06-17 10:00:00+00` (the review's HIGH false-positive bug).
    db.update_secured(&doc, &su, ACLS, &[], id, json!({ "due_at": "2026-06-17T10:00:00Z" }).as_object().unwrap()).await.unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM mail_message WHERE message_type='notification'").await, 1, "unchanged datetime records nothing");
    //     A real datetime change DOES record one tracking row.
    db.update_secured(&doc, &su, ACLS, &[], id, json!({ "due_at": "2026-06-18T12:30:00Z" }).as_object().unwrap()).await.unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM mail_message WHERE message_type='notification'").await, 2, "changed datetime records one entry");
    assert_eq!(count(&db, "SELECT count(*) FROM mail_tracking WHERE field='due_at'").await, 1);

    // 4) Best-effort: with the mail schema gone, a tracked change still succeeds (no audit, no error).
    sqlx::query("DROP TABLE mail_tracking").execute(db.pool()).await.unwrap();
    sqlx::query("DROP TABLE mail_message").execute(db.pool()).await.unwrap();
    let n = db.update_secured(&doc, &su, ACLS, &[], id, json!({ "state": "done" }).as_object().unwrap()).await.unwrap();
    assert_eq!(n, 1, "the business write commits even when tracking can't be recorded");

    db.drop_table(&doc).await.unwrap();
}
