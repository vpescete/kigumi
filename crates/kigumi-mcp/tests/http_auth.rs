//! The HTTP-transport authentication seam: `resolve_bearer` turns an `Authorization` header into
//! the calling `Ctx` via the API-key path (the same lookup/verify/narrow as the REST server), and
//! then a tool run under that ctx is gated by the engine. This tests the security-critical auth
//! logic of the network-facing MCP surface directly, without standing up the HTTP transport.
//! Requires DATABASE_URL.

use kigumi::prelude::*;
use kigumi_mcp::{CreateParams, KigumiMcp, SearchParams};
use serde_json::{json, Value as Json};

#[model(name = "httptest.doc", table = "httptest_doc")]
pub struct HttpTestDoc {
    #[field(label = "Title", required)]
    title: Text,
}

static ACLS: [Acl; 2] = [
    Acl { model: "httptest.doc", group: "httptest.reader", read: true, write: false, create: false, delete: false },
    Acl { model: "httptest.doc", group: "httptest.writer", read: true, write: true, create: true, delete: false },
];
kigumi::register_acls!(&ACLS);

fn unpack(result: &rmcp::model::CallToolResult) -> (bool, Json) {
    let v = serde_json::to_value(result).unwrap();
    let is_error = v["isError"].as_bool().unwrap_or(false);
    let text = v["content"][0]["text"].as_str().unwrap_or("null");
    (is_error, serde_json::from_str(text).unwrap_or(Json::Null))
}

#[tokio::test]
async fn http_mcp_authenticates_and_narrows_by_api_key() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;

    // A user who can read and write; seed one row and mint two keys (full + reader-narrowed).
    db.upsert_user("bot", "x", &["httptest.reader", "httptest.writer"]).await.unwrap();
    let uid = db.find_user("bot").await.unwrap().unwrap().id;
    let model = resolve_registered("httptest.doc").unwrap();
    kigumi_test::ins(db, &model, &kigumi_test::su(), json!({ "title": "seed" })).await;

    let full = kigumi_auth::new_api_key().unwrap();
    db.create_api_key(&full.prefix, &full.hash, uid, "full", &[], None).await.unwrap();
    let ro = kigumi_auth::new_api_key().unwrap();
    db.create_api_key(&ro.prefix, &ro.hash, uid, "ro", &["httptest.reader".to_string()], None).await.unwrap();

    let srv = KigumiMcp::for_http(db.clone()).await.unwrap();

    // No header, garbage, and a well-formed but unknown key: all one uniform denial.
    assert!(srv.resolve_bearer(None).await.is_err(), "no header denied");
    assert!(srv.resolve_bearer(Some("Bearer not-a-key")).await.is_err(), "malformed denied");
    assert!(srv.resolve_bearer(Some("Bearer kg_deadbeef_bogus")).await.is_err(), "unknown prefix denied");

    // The FULL key resolves to bot's Ctx with both groups → it can create.
    let full_ctx = srv.resolve_bearer(Some(&format!("Bearer {}", full.plain))).await
        .expect("full key authenticates");
    assert!(full_ctx.groups.contains(&"httptest.writer".to_string()));
    let (err, created) = unpack(
        &srv.create_record_inner(&full_ctx, CreateParams {
            model: "httptest.doc".into(),
            values: json!({ "title": "by full key" }).as_object().unwrap().clone(),
        }).await.unwrap(),
    );
    assert!(!err, "full key creates: {created}");

    // The READER-narrowed key resolves to a Ctx WITHOUT writer → create is denied by the engine,
    // read is allowed. The key never exceeds its user, and its own scope narrows further.
    let ro_ctx = srv.resolve_bearer(Some(&format!("Bearer {}", ro.plain))).await.expect("ro key authenticates");
    assert!(!ro_ctx.groups.contains(&"httptest.writer".to_string()), "narrowed away writer");
    let (err, _) = unpack(
        &srv.create_record_inner(&ro_ctx, CreateParams {
            model: "httptest.doc".into(),
            values: json!({ "title": "nope" }).as_object().unwrap().clone(),
        }).await.unwrap(),
    );
    assert!(err, "reader-scoped key cannot create");
    let (err, found) = unpack(
        &srv.search_records_inner(&ro_ctx, SearchParams {
            model: "httptest.doc".into(), domain: None, limit: None,
        }).await.unwrap(),
    );
    assert!(!err && found["matched"].as_i64().unwrap() >= 1, "reader-scoped key reads");

    // Revoke the reader key → it stops authenticating immediately.
    let ro_id = db.list_api_keys(uid).await.unwrap().iter().find(|k| k.name == "ro").unwrap().id;
    assert!(db.revoke_api_key_admin(ro_id).await.unwrap());
    assert!(srv.resolve_bearer(Some(&format!("Bearer {}", ro.plain))).await.is_err(), "revoked key is dead");

    t.db.drop_table(&model).await.ok();
}
