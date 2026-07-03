//! MCP v2 custom-field merge: a field added at runtime via the API (`ir_model_field` + a real
//! column) is visible in the MCP contract and writable/readable through the MCP tools — the
//! same surface a REST client sees. Requires DATABASE_URL.

use kigumi::prelude::*;
use kigumi_mcp::{CreateParams, KigumiMcp, ModelParam, RecordParam};
use kigumi_schema::pg_column_type;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value as Json};

#[model(name = "cftest.doc", table = "cftest_doc")]
pub struct CfTestDoc {
    #[field(label = "Title", required)]
    title: Text,
}

static ACLS: [Acl; 1] = [Acl {
    model: "cftest.doc",
    group: "cftest.user",
    read: true,
    write: true,
    create: true,
    delete: false,
}];
kigumi::register_acls!(&ACLS);

fn unpack(result: &rmcp::model::CallToolResult) -> (bool, Json) {
    let v = serde_json::to_value(result).unwrap();
    let is_error = v["isError"].as_bool().unwrap_or(false);
    let text = v["content"][0]["text"].as_str().unwrap_or("null");
    (is_error, serde_json::from_str(text).unwrap_or(Json::Null))
}

#[tokio::test]
async fn mcp_sees_and_writes_runtime_custom_fields() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;

    // Add a runtime custom field the compiled-in model never declared (real column + registry row).
    let priority_col = pg_column_type(&FieldKind::Integer);
    db.add_custom_field("cftest.doc", "priority", "Priority", "integer", false, None, None, "cftest_doc", priority_col)
        .await
        .unwrap();

    // A server built BEFORE the merge (with_ctx, compile-time only) does not see it...
    let bare = KigumiMcp::with_ctx(db.clone(), Ctx::new(9, vec!["cftest.user".to_string()])).unwrap();
    let (_, contract) = unpack(&bare.get_model(Parameters(ModelParam { model: "cftest.doc".into() })).await.unwrap());
    assert!(!contract.to_string().contains("priority"), "with_ctx is compile-time only");

    // ...but for_login (the real entry point) merges it: the contract lists it, and it round-trips.
    let user = KigumiMcp::for_login(db.clone(), "cf_user").await.err(); // no such user yet
    assert!(user.is_some(), "unknown user rejected");
    // Build the merged server directly via for_login against a real user.
    db.upsert_user("cf_user", "x", &["cftest.user"]).await.unwrap();
    let srv = KigumiMcp::for_login(db.clone(), "cf_user").await.unwrap();

    let (_, contract) = unpack(&srv.get_model(Parameters(ModelParam { model: "cftest.doc".into() })).await.unwrap());
    assert!(contract.to_string().contains("priority"), "the runtime field is in the MCP contract: {contract}");

    let (err, created) = unpack(
        &srv.create_record(Parameters(CreateParams {
            model: "cftest.doc".into(),
            values: json!({ "title": "Onboard", "priority": 5 }).as_object().unwrap().clone(),
        }))
        .await
        .unwrap(),
    );
    assert!(!err, "create with the custom field accepted: {created}");
    let id = created["id"].as_i64().unwrap();

    let (_, rec) = unpack(&srv.get_record(Parameters(RecordParam { model: "cftest.doc".into(), id })).await.unwrap());
    assert_eq!(rec["priority"].as_i64(), Some(5), "the custom value round-trips through MCP: {rec}");
}
