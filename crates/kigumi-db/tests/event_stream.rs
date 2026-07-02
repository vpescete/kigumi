//! The live-event read surface: gap-safe (txn, id) cursor reads — an open writer transaction hides
//! later rows until it resolves, and the xid/id INVERSION (a long tx with a low xid inserting late,
//! high ids across a younger writer) cannot skip either — and per-caller visibility filtering
//! (Read ACL, record rules incl. deleted-event suppression, company gate, D6 stripping of both
//! restricted field NAMES and state-transition VALUES). Requires DATABASE_URL.

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
    fields: &[txt("name", true), txt("secret", false), txt("state", false), boolean("active")],
};
fn f_doc() -> &'static ModelDescriptor { &DOC }
kigumi_core::inventory::submit! { ModelRegistration { name: "ev.doc", module: "test", descriptor: f_doc } }
// `secret` and `state` are admin-only (D6): a plain reader must learn neither that they changed
// nor their values.
kigumi_core::inventory::submit! { FieldGroupRegistration { model: "ev.doc", field: "secret", groups: &["admin"] } }
kigumi_core::inventory::submit! { FieldGroupRegistration { model: "ev.doc", field: "state", groups: &["admin"] } }

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
    // Open writer tx A enqueues an event and STAYS OPEN; a pool write lands another (committed).
    // The xmin guard must hide BOTH — the reader never advances past a hole. When A resolves, both
    // appear, in (txn, id) order.
    let start = db.latest_event_cursor().await.unwrap();
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
    // The guarded latest-cursor must not advance past the hole either (a fresh client connecting
    // now must still receive these once they commit).
    assert_eq!(db.latest_event_cursor().await.unwrap(), start, "connect cursor holds below the open writer");
    tx_a.commit().await.unwrap();
    let seen = db.events_after(start, 100).await.unwrap();
    assert_eq!(seen.len(), 2, "both events surface once the writer resolves");
    assert!((seen[0].txn, seen[0].id) < (seen[1].txn, seen[1].id), "(txn, id)-ordered");
    // The cursor advances and pages correctly.
    let after_first = db.events_after((seen[0].txn, seen[0].id), 100).await.unwrap();
    assert_eq!(after_first.len(), 1);

    // ── xid/id inversion ─────────────────────────────────────────────────
    // Long tx Y takes its xid FIRST (early write), then a younger tx X inserts a LOWER-id event
    // and stays open while Y inserts a HIGHER-id event and commits. An id-only cursor would advance
    // past X's id when Y's event is delivered; the (txn, id) cursor must not lose X's event.
    let start = db.latest_event_cursor().await.unwrap();
    let mut tx_y = db.pool().begin().await.unwrap();
    db.enqueue_event_in_tx(&mut tx_y, &OutboxEvent {
        event_type: "model.updated".to_string(), model: "ev.doc".to_string(), record_id: 201,
        author_uid: Some(0), company_id: None, change_summary: json!({}),
    }).await.unwrap(); // Y's xid assigned here (older), id lower
    let mut tx_x = db.pool().begin().await.unwrap();
    db.enqueue_event_in_tx(&mut tx_x, &OutboxEvent {
        event_type: "model.updated".to_string(), model: "ev.doc".to_string(), record_id: 202,
        author_uid: Some(0), company_id: None, change_summary: json!({}),
    }).await.unwrap(); // X: younger xid, middle id
    db.enqueue_event_in_tx(&mut tx_y, &OutboxEvent {
        event_type: "model.updated".to_string(), model: "ev.doc".to_string(), record_id: 203,
        author_uid: Some(0), company_id: None, change_summary: json!({}),
    }).await.unwrap(); // Y again: OLD xid, HIGHEST id — the inversion
    tx_y.commit().await.unwrap();
    // X still open: only Y's two events are deliverable (Y's xid < X's xid = xmin).
    let mut cursor = start;
    let seen = db.events_after(cursor, 100).await.unwrap();
    let ids: Vec<i64> = seen.iter().map(|e| e.record_id).collect();
    assert_eq!(ids, vec![201, 203], "the old-xid writer's events deliver while the younger tx runs");
    cursor = (seen.last().unwrap().txn, seen.last().unwrap().id);
    tx_x.commit().await.unwrap();
    // X's event has a HIGHER txn but LOWER id than the cursor — the pair cursor must still find it.
    let seen = db.events_after(cursor, 100).await.unwrap();
    let ids: Vec<i64> = seen.iter().map(|e| e.record_id).collect();
    assert_eq!(ids, vec![202], "the inverted (younger-xid, lower-id) event is NOT lost");

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

    let cursor = db.latest_event_cursor().await.unwrap();
    seed_event(db, "model.updated", visible_id, json!({ "changed_fields": ["name", "secret"] })).await;
    seed_event(db, "model.updated", hidden_id, json!({ "changed_fields": ["name"] })).await;
    seed_event(db, "model.deleted", 999, json!({})).await;
    seed_event(db, "model.state_changed", visible_id, json!({ "field": "state", "from": "draft", "to": "done" })).await;
    let batch = db.events_after(cursor, 100).await.unwrap();
    assert_eq!(batch.len(), 4);

    // The reader: sees the ACTIVE record's events only (record rule), with `secret` STRIPPED from
    // changed_fields (D6) and the state transition's VALUES blanked (state is D6-restricted). The
    // deleted event is SUPPRESSED: a Read record rule applies to this caller and cannot be
    // evaluated against a gone row — default-deny.
    let shaped = db.visible_events(&reader, ACLS, RULES, &batch).await.unwrap();
    assert_eq!(shaped.len(), 2, "rule-hidden record + deleted event filtered out: {shaped:?}");
    let upd = shaped.iter().find(|e| e["type"] == "model.updated").unwrap();
    assert_eq!(upd["record_id"].as_i64(), Some(visible_id));
    let fields: Vec<&str> = upd["changes"]["changed_fields"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(fields, vec!["name"], "restricted field name stripped for the reader");
    let st = shaped.iter().find(|e| e["type"] == "model.state_changed").unwrap();
    assert!(st["changes"].get("from").is_none(), "restricted state VALUES blanked: {st}");
    assert!(!shaped.iter().any(|e| e["type"] == "model.deleted"), "deleted suppressed under a record rule");

    // The admin (no rule targets them): both records' events, full changed_fields, full state
    // transition, AND the deleted event.
    let shaped = db.visible_events(&admin, ACLS, RULES, &batch).await.unwrap();
    assert_eq!(shaped.len(), 4);
    let upd = shaped.iter().find(|e| e["record_id"].as_i64() == Some(visible_id) && e["type"] == "model.updated").unwrap();
    let fields: Vec<&str> = upd["changes"]["changed_fields"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(fields, vec!["name", "secret"], "admin keeps the full change summary");
    let st = shaped.iter().find(|e| e["type"] == "model.state_changed").unwrap();
    assert_eq!(st["changes"]["to"], "done", "admin keeps the state values");
    assert!(shaped.iter().any(|e| e["type"] == "model.deleted"), "no rule for the admin: deleted delivered");

    // No Read ACL at all: nothing, not even the deleted event.
    let shaped = db.visible_events(&nobody, ACLS, RULES, &batch).await.unwrap();
    assert!(shaped.is_empty(), "no Read ACL leaks nothing: {shaped:?}");
}
