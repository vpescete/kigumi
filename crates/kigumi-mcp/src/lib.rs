//! The MCP projection of the catalog: one more artifact derived from the declarative model —
//! after DDL, OpenAPI and the UI contract, the AI surface. Any binary that links its modules can
//! serve MCP over stdio (`kigumi mcp <login>`, or the scaffolded app's `app mcp <login>`), and
//! every tool call runs under the IMPERSONATED USER'S `Ctx`: ACLs, record rules and field groups
//! are enforced by the data layer on every path, exactly as for the REST API. The guardrail is
//! the security engine, not the prompt.
//!
//! TRUST MODEL: impersonation is UNAUTHENTICATED — `for_login` takes a login string, no
//! credential. Whoever can start this process already holds DATABASE_URL (full database access),
//! so the boundary is operator trust, like every other CLI command; it is NOT a network-facing
//! authenticated surface.
//!
//! Scope (v1): the static compiled-in security baseline and catalog (like `kigumi-runtime`'s
//! serve) and the stdio transport. Runtime DB overlays are NOT merged: ACL/rule rows added via
//! the API and custom fields from `ir_model_field` are invisible here until a v2 merge — a
//! custom field readable over REST is rejected as unknown by this surface. HTTP transports and
//! the runtime overlays can layer on later.

use std::collections::HashMap;
use std::sync::Arc;

use kigumi_core::{
    is_mailed, is_transient, registered_acls, registered_rules, resolve_all_registered, Acl, Ctx,
    Domain, RecordRule, ResolvedModel,
};
use kigumi_db::{Db, DbError};
use kigumi_schema::to_ui_contract;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{tool, tool_router, ErrorData};
use serde::Deserialize;
use serde_json::{json, Map, Value as Json};

/// How many records `search_records` returns when the caller does not say (and its hard cap).
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

pub struct KigumiMcp {
    db: Db,
    ctx: Ctx,
    acls: Arc<[Acl]>,
    rules: Arc<[RecordRule]>,
    models: HashMap<String, ResolvedModel>,
}

// -- tool parameter shapes ------------------------------------------------------------------

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ModelParam {
    /// Model name, e.g. "res.partner".
    pub model: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    /// Model name, e.g. "res.partner".
    pub model: String,
    /// Optional domain AST: {"field","op","value"} nodes composed with {"and":[..]},
    /// {"or":[..]}, {"not":..}. Ops: =, !=, <, <=, >, >=, like, ilike, in, not in.
    pub domain: Option<Json>,
    /// Max records to return (default 50, cap 200).
    pub limit: Option<u32>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct RecordParam {
    pub model: String,
    pub id: i64,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct CreateParams {
    pub model: String,
    /// Field values by field name. Relations take the target record id.
    pub values: Map<String, Json>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct UpdateParams {
    pub model: String,
    pub id: i64,
    pub values: Map<String, Json>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ActionParams {
    pub model: String,
    pub id: i64,
    /// Action name as listed in the model contract (get_model).
    pub action: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ServiceParams {
    pub model: String,
    pub id: i64,
    /// Service name registered on the model.
    pub service: String,
    /// The service's JSON input (an object), if it takes one.
    pub body: Option<Map<String, Json>>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct MessageParams {
    pub model: String,
    pub id: i64,
    /// Message body (plain text) posted on the record's chatter thread.
    pub body: String,
}

// -- result helpers -------------------------------------------------------------------------

fn ok_json(v: &Json) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(
        serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()),
    )])
}

/// A DbError is a CALLER-visible failure (the agent should read it and adapt), never a protocol
/// error: same envelope shape as the REST API, so `fields` reaches the agent on validation.
fn db_err(e: DbError) -> CallToolResult {
    let body = match e {
        DbError::AccessDenied { model, operation } => {
            json!({ "error": { "code": "access-denied", "message": format!("access denied: {operation} on {model}") } })
        }
        DbError::Invalid { message, fields } => {
            let mut map = Map::new();
            for (f, m) in fields {
                map.entry(f).or_insert_with(|| Json::Array(vec![])).as_array_mut().unwrap().push(Json::String(m));
            }
            json!({ "error": { "code": "invalid", "message": message, "fields": map } })
        }
        DbError::BadInput(m) => json!({ "error": { "code": "bad-input", "message": m } }),
        DbError::Conflict(m) => json!({ "error": { "code": "conflict", "message": m } }),
        // Internal detail (SQL, schema) never reaches the agent — same rule as the HTTP layer.
        other => {
            eprintln!("kigumi-mcp internal error: {other:?}");
            json!({ "error": { "code": "internal", "message": "internal error" } })
        }
    };
    CallToolResult::error(vec![ContentBlock::text(body.to_string())])
}

fn arg_err(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(
        json!({ "error": { "code": "bad-input", "message": msg.into() } }).to_string(),
    )])
}

type ToolResult = Result<CallToolResult, ErrorData>;

#[tool_router(server_handler)]
impl KigumiMcp {
    /// Every model served by this instance, with its chatter/transient nature.
    #[tool(
        name = "list_models",
        description = "List every business model served by this Kigumi instance. Use get_model for a model's fields and actions."
    )]
    pub async fn list_models(&self) -> ToolResult {
        let mut names: Vec<&String> = self.models.keys().collect();
        names.sort();
        let rows: Vec<Json> = names
            .into_iter()
            .map(|n| json!({ "model": n, "chatter": is_mailed(n), "transient": is_transient(n) }))
            .collect();
        Ok(ok_json(&Json::Array(rows)))
    }

    /// The model's machine-readable contract: fields (types, labels, required, selections,
    /// relation targets), list columns and actions.
    #[tool(
        name = "get_model",
        description = "Get a model's contract: fields with types/labels/required/selection values/relation targets, plus its actions. Read this before creating or updating records."
    )]
    pub async fn get_model(&self, Parameters(p): Parameters<ModelParam>) -> ToolResult {
        let Some(model) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        match to_ui_contract(model, &[]) {
            Ok(contract) => match serde_json::from_str::<Json>(&contract) {
                Ok(v) => Ok(ok_json(&v)),
                Err(_) => Ok(ok_json(&Json::String(contract))),
            },
            Err(e) => Ok(arg_err(format!("contract failed: {e:?}"))),
        }
    }

    /// Secured search: the caller's ACLs, record rules and company scope shape what is visible.
    #[tool(
        name = "search_records",
        description = "Search a model's records with an optional domain AST filter ({\"field\",\"op\",\"value\"} nodes composed with {\"and\":[..]}/{\"or\":[..]}/{\"not\":..}; ops: =, !=, <, <=, >, >=, like, ilike, in, not in). Returns at most `limit` records (default 50). Only records the impersonated user may read are returned."
    )]
    pub async fn search_records(&self, Parameters(p): Parameters<SearchParams>) -> ToolResult {
        let Some(model) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        let filter = match p.domain {
            Some(ast) => match Domain::from_json(&ast.to_string()).and_then(|d| d.validate(model).map(|_| d)) {
                Ok(d) => Some(d),
                Err(e) => return Ok(arg_err(format!("invalid domain: {e:?}"))),
            },
            None => None,
        };
        let limit = (p.limit.unwrap_or(DEFAULT_LIMIT as u32) as i64).min(MAX_LIMIT);
        // The cap is enforced by the DATABASE (LIMIT in SQL): an agent-issued broad search must
        // never materialize a huge table in memory (review must-fix). `matched` is a count.
        match self.db.list_secured(model, &self.ctx, &self.acls, &self.rules, filter.as_ref(), &[], limit, 0).await {
            Ok(page) => Ok(ok_json(&json!({ "returned": page.data.len(), "matched": page.total, "records": page.data }))),
            Err(e) => Ok(db_err(e)),
        }
    }

    #[tool(
        name = "get_record",
        description = "Read one record by id (fields filtered by the impersonated user's field-group visibility)."
    )]
    pub async fn get_record(&self, Parameters(p): Parameters<RecordParam>) -> ToolResult {
        let Some(model) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        match self.db.find_one_secured(model, &self.ctx, &self.acls, &self.rules, p.id).await {
            Ok(Some(rec)) => Ok(ok_json(&rec)),
            Ok(None) => Ok(arg_err("not found or not permitted")),
            Err(e) => Ok(db_err(e)),
        }
    }

    #[tool(
        name = "create_record",
        description = "Create a record. Values by field name (see get_model); relations take the target id. Validation failures return {\"error\":{\"fields\":...}} with per-field messages."
    )]
    pub async fn create_record(&self, Parameters(p): Parameters<CreateParams>) -> ToolResult {
        let Some(model) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        match self.db.insert_secured(model, &self.ctx, &self.acls, &self.rules, &p.values).await {
            Ok(id) => Ok(ok_json(&json!({ "id": id }))),
            Err(e) => Ok(db_err(e)),
        }
    }

    #[tool(
        name = "update_record",
        description = "Update fields of a record by id. Only writable, permitted fields are accepted."
    )]
    pub async fn update_record(&self, Parameters(p): Parameters<UpdateParams>) -> ToolResult {
        let Some(model) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        match self.db.update_secured(model, &self.ctx, &self.acls, &self.rules, p.id, &p.values).await {
            Ok(n) if n > 0 => Ok(ok_json(&json!({ "updated": true }))),
            Ok(_) => Ok(arg_err("not found or not permitted")),
            Err(e) => Ok(db_err(e)),
        }
    }

    #[tool(name = "delete_record", description = "Delete a record by id (requires the Delete ACL).")]
    pub async fn delete_record(&self, Parameters(p): Parameters<RecordParam>) -> ToolResult {
        let Some(model) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        match self.db.delete_secured(model, &self.ctx, &self.acls, &self.rules, p.id).await {
            Ok(n) if n > 0 => Ok(ok_json(&json!({ "deleted": true }))),
            Ok(_) => Ok(arg_err("not found or not permitted")),
            Err(e) => Ok(db_err(e)),
        }
    }

    #[tool(
        name = "run_action",
        description = "Run a state-transition action on a record (the model contract lists each model's actions, e.g. confirm, open, close). Invalid transitions return the action's own message."
    )]
    pub async fn run_action(&self, Parameters(p): Parameters<ActionParams>) -> ToolResult {
        let Some(model) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        match self.db.run_action(model, &self.ctx, &self.acls, &self.rules, p.id, &p.action).await {
            Ok(()) => Ok(ok_json(&json!({ "ok": true, "action": p.action }))),
            Err(e) => Ok(db_err(e)),
        }
    }

    #[tool(
        name = "run_service",
        description = "Run a registered cross-record service on a record (one transaction: commit on success, rollback on error). Body is the service's JSON input object."
    )]
    pub async fn run_service(&self, Parameters(p): Parameters<ServiceParams>) -> ToolResult {
        let Some(model) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        let body = p.body.unwrap_or_default();
        match self.db.run_service(model, &self.ctx, &self.acls, &self.rules, p.id, &p.service, body).await {
            Ok(out) => Ok(ok_json(&out)),
            Err(e) => Ok(db_err(e)),
        }
    }

    #[tool(
        name = "post_message",
        description = "Post a plain-text message on a record's chatter thread (models with \"chatter\": true in list_models)."
    )]
    pub async fn post_message(&self, Parameters(p): Parameters<MessageParams>) -> ToolResult {
        if !is_mailed(&p.model) {
            return Ok(arg_err(format!("model '{}' has no chatter thread", p.model)));
        }
        let Some(host) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        let Some(message) = self.models.get("mail.message") else {
            return Ok(arg_err("the mail module is not linked in this binary"));
        };
        // The chatter invariant, same as the REST handler: you may post only to the thread of a
        // record YOU can read — gate on the host under the caller's ctx, THEN insert elevated
        // (normal users have no mail.message ACL by design; author_id stays the real uid).
        match self.db.find_one_secured(host, &self.ctx, &self.acls, &self.rules, p.id).await {
            Ok(Some(_)) => {}
            Ok(None) => return Ok(arg_err("not found or not permitted")),
            Err(e) => return Ok(db_err(e)),
        }
        let mut values = Map::new();
        values.insert("res_model".into(), Json::String(p.model));
        values.insert("res_id".into(), Json::Number(p.id.into()));
        values.insert("author_id".into(), Json::Number(self.ctx.uid.into()));
        values.insert("body".into(), Json::String(p.body));
        values.insert("message_type".into(), Json::String("comment".into()));
        let elevated = self.ctx.sudo();
        match self.db.insert_secured(message, &elevated, &self.acls, &self.rules, &values).await {
            Ok(id) => Ok(ok_json(&json!({ "id": id }))),
            Err(e) => Ok(db_err(e)),
        }
    }
}

impl KigumiMcp {
    /// Builds the server impersonating `login`: every tool call runs under that user's `Ctx`.
    /// UNAUTHENTICATED by design — no credential is checked; running this at all requires
    /// DATABASE_URL, so the gate is operator trust (see the module doc).
    pub async fn for_login(db: Db, login: &str) -> Result<Self, DbError> {
        let user = db
            .find_user(login)
            .await?
            .ok_or_else(|| DbError::BadInput(format!("unknown user '{login}'")))?;
        let mut ctx = Ctx::new(user.id, user.groups);
        if let Some(active) = user.company_id {
            ctx = ctx.in_companies(active, user.company_ids);
        }
        Self::with_ctx(db, ctx)
    }

    /// Builds the server with an explicit `Ctx` (embedding, tests). The catalog and the security
    /// baseline are the compiled-in registrations, like kigumi-runtime's serve; a catalog that
    /// fails to resolve REFUSES to serve (review must-fix: silently serving zero models looks
    /// "up" while every tool answers unknown-model). Runtime custom fields are not merged (v1).
    pub fn with_ctx(db: Db, ctx: Ctx) -> Result<Self, DbError> {
        let models = resolve_all_registered()
            .map(|models| models.into_iter().map(|m| (m.name.to_string(), m)).collect())
            .map_err(DbError::Migration)?;
        Ok(KigumiMcp {
            db,
            ctx,
            acls: registered_acls().into(),
            rules: registered_rules().into(),
            models,
        })
    }

    /// Serves MCP over stdio until the client disconnects.
    pub async fn serve_stdio(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use rmcp::ServiceExt;
        let running = self.serve(rmcp::transport::stdio()).await?;
        running.waiting().await?;
        Ok(())
    }
}
