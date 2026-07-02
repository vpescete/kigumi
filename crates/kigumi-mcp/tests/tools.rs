//! The MCP tool layer against a real database: the catalog is discoverable, CRUD/search/action
//! flow through the secured paths under the impersonated Ctx, and the security engine — not the
//! prompt — denies what the user may not do (no Delete ACL, group-less default-deny). Requires
//! DATABASE_URL.

use kigumi::prelude::*;
use kigumi_mcp::{
    ActionParams, CreateParams, KigumiMcp, ModelParam, RecordParam, SearchParams, UpdateParams,
};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value as Json};

#[model(name = "mcptest.item", table = "mcptest_item")]
pub struct McpTestItem {
    #[field(label = "Title", required)]
    title: Text,

    #[field(label = "State", default = "draft", selection = "draft:Draft,done:Done")]
    state: Selection,
}

fn finish_item(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "draft" => Ok(ActionOutcome::new().set("state", Value::Str("done".to_string()))),
        s => Err(format!("can only finish a draft item (state is '{s}')")),
    }
}
kigumi::register_action!("mcptest.item", "finish", finish_item, &["mcptest.user"]);

static ACLS: [Acl; 1] = [
    // No delete: the delete tool must come back denied for this group.
    Acl { model: "mcptest.item", group: "mcptest.user", read: true, write: true, create: true, delete: false },
];
kigumi::register_acls!(&ACLS);

/// (is_error, first text content parsed as JSON) — via the Serialize form, so the test does not
/// couple to rmcp's content enum internals.
fn unpack(result: &rmcp::model::CallToolResult) -> (bool, Json) {
    let v = serde_json::to_value(result).unwrap();
    let is_error = v["isError"].as_bool().unwrap_or(false);
    let text = v["content"][0]["text"].as_str().unwrap_or("null");
    (is_error, serde_json::from_str(text).unwrap_or(Json::Null))
}

#[tokio::test]
async fn mcp_tools_enforce_the_callers_ctx() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let user = KigumiMcp::with_ctx(t.db.clone(), Ctx::new(7, vec!["mcptest.user".to_string()]));
    let nobody = KigumiMcp::with_ctx(t.db.clone(), Ctx::new(8, vec![]));

    // Discovery: the model is in the catalog and its contract lists the action.
    let (err, models) = unpack(&user.list_models().await.unwrap());
    assert!(!err);
    assert!(models.as_array().unwrap().iter().any(|m| m["model"] == "mcptest.item"));
    let (err, contract) = unpack(
        &user.get_model(Parameters(ModelParam { model: "mcptest.item".into() })).await.unwrap(),
    );
    assert!(!err);
    assert!(contract.to_string().contains("finish"), "contract lists the action");

    // Create → search (domain AST) → get → update → action, all as the impersonated user.
    let (err, created) = unpack(
        &user
            .create_record(Parameters(CreateParams {
                model: "mcptest.item".into(),
                values: json!({ "title": "Fix the door" }).as_object().unwrap().clone(),
            }))
            .await
            .unwrap(),
    );
    assert!(!err, "create failed: {created}");
    let id = created["id"].as_i64().unwrap();

    let (err, found) = unpack(
        &user
            .search_records(Parameters(SearchParams {
                model: "mcptest.item".into(),
                domain: Some(json!({ "field": "state", "op": "=", "value": "draft" })),
                limit: None,
            }))
            .await
            .unwrap(),
    );
    assert!(!err);
    assert_eq!(found["returned"].as_i64(), Some(1));

    let (err, rec) = unpack(
        &user.get_record(Parameters(RecordParam { model: "mcptest.item".into(), id })).await.unwrap(),
    );
    assert!(!err);
    assert_eq!(rec["title"], "Fix the door");

    let (err, _) = unpack(
        &user
            .update_record(Parameters(UpdateParams {
                model: "mcptest.item".into(),
                id,
                values: json!({ "title": "Fix the gate" }).as_object().unwrap().clone(),
            }))
            .await
            .unwrap(),
    );
    assert!(!err);

    let (err, _) = unpack(
        &user
            .run_action(Parameters(ActionParams {
                model: "mcptest.item".into(),
                id,
                action: "finish".into(),
            }))
            .await
            .unwrap(),
    );
    assert!(!err);
    let (_, rec) = unpack(
        &user.get_record(Parameters(RecordParam { model: "mcptest.item".into(), id })).await.unwrap(),
    );
    assert_eq!(rec["state"], "done");

    // The engine is the guardrail: no Delete ACL for the group → denied; a group-less caller is
    // default-denied even on read; a missing-required create returns the structured field error.
    let (err, body) = unpack(
        &user.delete_record(Parameters(RecordParam { model: "mcptest.item".into(), id })).await.unwrap(),
    );
    assert!(err, "delete must be denied: {body}");

    let (err, body) = unpack(
        &nobody
            .search_records(Parameters(SearchParams {
                model: "mcptest.item".into(),
                domain: None,
                limit: None,
            }))
            .await
            .unwrap(),
    );
    assert!(err, "default-deny for the group-less caller: {body}");

    let (err, body) = unpack(
        &user
            .create_record(Parameters(CreateParams {
                model: "mcptest.item".into(),
                values: serde_json::Map::new(),
            }))
            .await
            .unwrap(),
    );
    assert!(err);
    assert!(body["error"]["fields"].to_string().contains("title"), "field error surfaced: {body}");
}
