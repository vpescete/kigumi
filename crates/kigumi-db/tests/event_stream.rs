//! The live-event read surface: gap-safe cursor reads (an open writer transaction hides later ids
//! until it resolves, so a cursor can never skip a late-committing row) and per-caller visibility
//! filtering (Read ACL, record rules, deleted-event company gate, and D6 stripping of restricted
//! field names from change summaries). Requires DATABASE_URL.

use kigumi_core::{
    resolve, Acl, Ctx, FieldDef, FieldGroupRegistration, FieldKind, ModelDescriptor,
    ModelRegistration, Operation, RecordRule, ResolvedModel, RuleDomain,
};
use kigumi_db::{Db, OutboxEvent};
use serde_json::json;

const fn txt(name: &'static str, required: bool) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Text, required, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}
const fn boolean(name: &'static str) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Bool, required: false, stored: true, compute: None, depends: &[], default: Some("true"), unique: false, check: None }
}

static DOC: ModelDescriptor = ModelDescriptor {
    name: "ev.doc",
    table: "ev_doc",
    fields: &[txt("name", true), txt("secret", false), boolean("active")],
};
fn f_doc() -> &'static ModelDescriptor { &DOC }
kigumi_core::inventory::submit! { ModelRegistration { name: "ev.doc", module: "test", descriptor: f_doc } }
// `secret` is admin-only (D6): a plain reader must not learn it changed.
kigumi_core::inventory::submit! { FieldGroupRegistration { model: "ev.doc", field: "secret", groups: &["admin"] } }

fn active_only() -> kigumi_core::Domain {
    kigumi_core::Domain::field("active").eq(true)
}

static ACLS: &[Acl] = &[
    Acl { model: "ev.doc", group: "reader", read: true, write: true, create: true, delete: true },
    Acl { model: "ev.doc", group: "admin", read: true, write: true, create: true, delete: true },
];
// Readers see only ACTIVE docs; admins are unrestricted (no rule targets them... rules are global
// AND group OR — scope this rule to the reader group only).
static RULES: &[RecordRule] = &[RecordRule {
    model: "ev.doc",
    groups: &["reader"],
    ops: &[Operation::Read],
    domain: RuleDomain::Static(active_only),
}];

async fn seed_event(db: &Db, event_type: &str, record_id: i64, changes: serde_json::Value) {
    db.enqueue_event(&OutboxEvent {
        event_type: event_type.to_string(),
        model: "ev.doc".to_string(),
        record_id,
        author_uid: Some(0),
        company_id: None,
        change_summary: changes,
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cursor_is_gap_safe_and_visibility_is_per_caller() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let su = kigumi_test::su();
    let doc = resolve(&DOC, &[]);
    let doc: ResolvedModel = doc.unwrap();

    // ── gap safety ────────────────────────────────────────────────────────
    // Open writer tx A enqueues event #1 and STAYS OPEN; a pool write lands event #2 (committed).
    // The xmin guard must hide BOTH (2's tx started after A began... 2 commits but A's xid < xmin
    // requirement hides anything not strictly older than every running tx) — the point: the reader
    // never advances past a hole. When A resolves, both appear, in id order.
    let start = db.latest_event_id().await.unwrap();
    let mut tx_a = db.pool().begin().await.unwrap();
    db.enqueue_event_in_tx(&mut tx_a, &OutboxEvent {
        event_type: "model.updated".to_string(),
        model: "ev.doc".to_string(),
        record_id: 101,
        author_uid: Some(0),
        company_id: None,
        change_summary: json!({}),
    })
    .await
    .unwrap();
    seed_event(db, "model.updated", 102, json!({})).await; // committed, but a later xid
    let seen = db.events_after(start, 100).await.unwrap();
    assert!(
        seen.is_empty(),
        "no event is visible while an older writer tx is open (no skippable hole), got {seen:?}"
    );
    tx_a.commit().await.unwrap();
    let seen = db.events_after(start, 100).await.unwrap();
    assert_eq!(seen.len(), 2, "both events surface once the writer resolves");
    assert!(seen[0].id < seen[1].id, "id-ordered");
    assert_eq!(seen[0].record_id, 101, "insertion order preserved");
    // The cursor advances and pages correctly.
    let after_first = db.events_after(seen[0].id, 100).await.unwrap();
    assert_eq!(after_first.len(), 1);
    assert_eq!(after_first[0].record_id, 102);

    // ── visibility filtering ──────────────────────────────────────────────
    let reader = Ctx::new(7, vec!["reader".to_string()]);
    let admin = Ctx::new(8, vec!["admin".to_string()]);
    let nobody = Ctx::new(9, vec!["other".to_string()]);

    let visible_id = db
        .insert_secured(&doc, &su, &[], &[], json!({ "name": "open", "active": true }).as_object().unwrap())
        .await
        .unwrap();
    let hidden_id = db
        .insert_secured(&doc, &su, &[], &[], json!({ "name": "shut", "active": false }).as_object().unwrap())
        .await
        .unwrap();

    let cursor = db.latest_event_id().await.unwrap();
    seed_event(db, "model.updated", visible_id, json!({ "changed_fields": ["name", "secret"] })).await;
    seed_event(db, "model.updated", hidden_id, json!({ "changed_fields": ["name"] })).await;
    seed_event(db, "model.deleted", 999, json!({})).await;
    let batch = db.events_after(cursor, 100).await.unwrap();
    assert_eq!(batch.len(), 3);

    // The reader: sees the ACTIVE record's event only (record rule), with `secret` STRIPPED from
    // changed_fields (D6); sees the deleted event (Read ACL + shared company).
    let shaped = db.visible_events(&reader, ACLS, RULES, &batch).await.unwrap();
    assert_eq!(shaped.len(), 2, "hidden record's event filtered out: {shaped:?}");
    let upd = shaped.iter().find(|e| e["type"] == "model.updated").unwrap();
    assert_eq!(upd["record_id"].as_i64(), Some(visible_id));
    let fields: Vec<&str> = upd["changes"]["changed_fields"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(fields, vec!["name"], "restricted field name stripped for the reader");
    assert!(shaped.iter().any(|e| e["type"] == "model.deleted"), "deleted event passes the ACL gate");

    // The admin: sees both records' events and the full changed_fields.
    let shaped = db.visible_events(&admin, ACLS, RULES, &batch).await.unwrap();
    assert_eq!(shaped.len(), 3);
    let upd = shaped.iter().find(|e| e["record_id"].as_i64() == Some(visible_id)).unwrap();
    let fields: Vec<&str> = upd["changes"]["changed_fields"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(fields, vec!["name", "secret"], "admin keeps the full change summary");

    // No Read ACL at all: nothing, not even the deleted event.
    let shaped = db.visible_events(&nobody, ACLS, RULES, &batch).await.unwrap();
    assert!(shaped.is_empty(), "no Read ACL leaks nothing: {shaped:?}");
}
