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
//! Scope: the compiled-in security baseline (like `kigumi-runtime`'s serve) plus the runtime
//! CUSTOM FIELDS from `ir_model_field` (merged at construction, so a field added via the API and
//! readable over REST is read/written here too), over the stdio transport. Still NOT merged:
//! runtime ACL/rule DB overlays (the compiled-in baseline is the authority here); the custom-field
//! snapshot is taken at connect, so a field added mid-session appears on reconnect. HTTP
//! transports and the ACL overlays can layer on later.

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
use rmcp::service::RequestContext;
use rmcp::RoleServer;
use rmcp::{tool, tool_router, ErrorData};
use serde::Deserialize;
use serde_json::{json, Map, Value as Json};

/// How many records `search_records` returns when the caller does not say (and its hard cap).
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;
/// Matches the server's API-key usage-stamp throttle.
const API_KEY_TOUCH_THROTTLE_SECS: i64 = 300;

/// How a tool call's `Ctx` is resolved. `Fixed` is the stdio/embedded identity (set once at
/// construction — operator trust). `PerRequest` authenticates each call from the request's
/// `Authorization: Bearer kg_...` API key (the HTTP transport — a network-facing surface).
enum Auth {
    Fixed(Ctx),
    PerRequest,
}

pub struct KigumiMcp {
    db: Db,
    auth: Auth,
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

/// A static Argon2 hash used to equalize timing when a presented key's prefix is absent (mirrors
/// the server's dummy_hash): the not-found path spends the same Argon2 as a wrong-secret path, so
/// latency does not leak which keys exist.
fn dummy_hash() -> &'static str {
    use std::sync::OnceLock;
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| kigumi_auth::hash_password("kigumi-timing-equalizer").expect("dummy hash"))
}

impl KigumiMcp {
    /// Resolves the calling `Ctx` for a tool. The Fixed identity (stdio/embedded) returns directly;
    /// the HTTP transport authenticates the request's `Authorization: Bearer kg_...` API key — the
    /// same key path as the REST server, narrowed by the key's scopes. On failure returns a ready
    /// error result (the tool returns it as `Ok`, an in-band tool error, not a protocol error).
    async fn caller(&self, rc: &RequestContext<RoleServer>) -> Result<Ctx, CallToolResult> {
        match &self.auth {
            Auth::Fixed(ctx) => Ok(ctx.clone()),
            Auth::PerRequest => {
                let header: Option<&str> = rc
                    .extensions
                    .get::<http::request::Parts>()
                    .and_then(|p| p.headers.get("authorization"))
                    .and_then(|v| v.to_str().ok());
                self.resolve_bearer(header).await
            }
        }
    }

    /// Resolves an `Authorization` header value to the caller's `Ctx` via the API-key path — the
    /// same lookup/verify/narrow the REST server does. Public within the crate so the HTTP auth is
    /// unit-testable without standing up a transport. One uniform denial for every failure.
    pub async fn resolve_bearer(&self, header: Option<&str>) -> Result<Ctx, CallToolResult> {
        let denied = || {
            CallToolResult::error(vec![ContentBlock::text(
                json!({ "error": { "code": "unauthorized", "message": "unauthorized" } }).to_string(),
            )])
        };
        let Some((prefix, secret)) = header
            .and_then(|h| h.strip_prefix("Bearer "))
            .and_then(|h| kigumi_auth::parse_api_key(h))
        else {
            return Err(denied());
        };
        let key = self.db.find_api_key(&prefix).await.map_err(|_| denied())?;
        // Timing equalizer: an absent/revoked/expired prefix still spends one Argon2. Own the hash
        // so no borrow of `key` survives its move below.
        let hash: String = key.as_ref().map(|k| k.hash.clone()).unwrap_or_else(|| dummy_hash().to_string());
        let ok = kigumi_auth::verify_password(&secret, &hash);
        let Some(key) = key else { return Err(denied()) };
        if !ok {
            return Err(denied());
        }
        let resolved = self.db.build_key_ctx(key.user_id, &key.scopes).await.map_err(|_| denied())?;
        let _ = self.db.touch_api_key(&prefix, API_KEY_TOUCH_THROTTLE_SECS).await;
        Ok(resolved)
    }
}

#[tool_router(server_handler)]
impl KigumiMcp {
    /// Every model served by this instance, with its chatter/transient nature.
    #[tool(
        name = "list_models",
        description = "List every business model served by this Kigumi instance. Use get_model for a model's fields and actions."
    )]
    pub async fn list_models(&self, rc: RequestContext<RoleServer>) -> ToolResult {
        if let Err(e) = self.caller(&rc).await { return Ok(e); }
        self.list_models_inner().await
    }

    /// The model's machine-readable contract: fields (types, labels, required, selections,
    /// relation targets), list columns and actions.
    #[tool(
        name = "get_model",
        description = "Get a model's contract: fields with types/labels/required/selection values/relation targets, plus its actions. Read this before creating or updating records."
    )]
    pub async fn get_model(&self, Parameters(p): Parameters<ModelParam>, rc: RequestContext<RoleServer>) -> ToolResult {
        if let Err(e) = self.caller(&rc).await { return Ok(e); }
        self.get_model_inner(p).await
    }

    /// Secured search: the caller's ACLs, record rules and company scope shape what is visible.
    #[tool(
        name = "search_records",
        description = "Search a model's records with an optional domain AST filter ({\"field\",\"op\",\"value\"} nodes composed with {\"and\":[..]}/{\"or\":[..]}/{\"not\":..}; ops: =, !=, <, <=, >, >=, like, ilike, in, not in). Returns at most `limit` records (default 50). Only records the impersonated user may read are returned."
    )]
    pub async fn search_records(&self, Parameters(p): Parameters<SearchParams>, rc: RequestContext<RoleServer>) -> ToolResult {
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        self.search_records_inner(&ctx, p).await
    }

    #[tool(
        name = "get_record",
        description = "Read one record by id (fields filtered by the impersonated user's field-group visibility)."
    )]
    pub async fn get_record(&self, Parameters(p): Parameters<RecordParam>, rc: RequestContext<RoleServer>) -> ToolResult {
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        self.get_record_inner(&ctx, p).await
    }

    #[tool(
        name = "create_record",
        description = "Create a record. Values by field name (see get_model); relations take the target id. Validation failures return {\"error\":{\"fields\":...}} with per-field messages."
    )]
    pub async fn create_record(&self, Parameters(p): Parameters<CreateParams>, rc: RequestContext<RoleServer>) -> ToolResult {
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        self.create_record_inner(&ctx, p).await
    }

    #[tool(
        name = "update_record",
        description = "Update fields of a record by id. Only writable, permitted fields are accepted."
    )]
    pub async fn update_record(&self, Parameters(p): Parameters<UpdateParams>, rc: RequestContext<RoleServer>) -> ToolResult {
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        self.update_record_inner(&ctx, p).await
    }

    #[tool(name = "delete_record", description = "Delete a record by id (requires the Delete ACL).")]
    pub async fn delete_record(&self, Parameters(p): Parameters<RecordParam>, rc: RequestContext<RoleServer>) -> ToolResult {
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        self.delete_record_inner(&ctx, p).await
    }

    #[tool(
        name = "run_action",
        description = "Run a state-transition action on a record (the model contract lists each model's actions, e.g. confirm, open, close). Invalid transitions return the action's own message."
    )]
    pub async fn run_action(&self, Parameters(p): Parameters<ActionParams>, rc: RequestContext<RoleServer>) -> ToolResult {
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        self.run_action_inner(&ctx, p).await
    }

    #[tool(
        name = "run_service",
        description = "Run a registered cross-record service on a record (one transaction: commit on success, rollback on error). Body is the service's JSON input object."
    )]
    pub async fn run_service(&self, Parameters(p): Parameters<ServiceParams>, rc: RequestContext<RoleServer>) -> ToolResult {
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        self.run_service_inner(&ctx, p).await
    }

    #[tool(
        name = "post_message",
        description = "Post a plain-text message on a record's chatter thread (models with \"chatter\": true in list_models)."
    )]
    pub async fn post_message(&self, Parameters(p): Parameters<MessageParams>, rc: RequestContext<RoleServer>) -> ToolResult {
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        let ctx = match self.caller(&rc).await { Ok(c) => c, Err(e) => return Ok(e) };
        self.post_message_inner(&ctx, p).await
    }
}

/// The tool bodies, testable directly with an explicit `Ctx` (the `#[tool]` wrappers resolve the
/// caller then delegate here).
impl KigumiMcp {
    pub async fn list_models_inner(&self) -> ToolResult {
        let mut names: Vec<&String> = self.models.keys().collect();
        names.sort();
        let rows: Vec<Json> = names
            .into_iter()
            .map(|n| json!({ "model": n, "chatter": is_mailed(n), "transient": is_transient(n) }))
            .collect();
        Ok(ok_json(&Json::Array(rows)))
    }

    pub async fn get_model_inner(&self, p: ModelParam) -> ToolResult {
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

    pub async fn search_records_inner(&self, ctx: &Ctx, p: SearchParams) -> ToolResult {
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
        match self.db.list_secured(model, ctx, &self.acls, &self.rules, filter.as_ref(), &[], limit, 0).await {
            Ok(page) => Ok(ok_json(&json!({ "returned": page.data.len(), "matched": page.total, "records": page.data }))),
            Err(e) => Ok(db_err(e)),
        }
    }

    pub async fn get_record_inner(&self, ctx: &Ctx, p: RecordParam) -> ToolResult {
        let Some(model) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        match self.db.find_one_secured(model, ctx, &self.acls, &self.rules, p.id).await {
            Ok(Some(rec)) => Ok(ok_json(&rec)),
            Ok(None) => Ok(arg_err("not found or not permitted")),
            Err(e) => Ok(db_err(e)),
        }
    }

    pub async fn create_record_inner(&self, ctx: &Ctx, p: CreateParams) -> ToolResult {
        let Some(model) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        match self.db.insert_secured(model, ctx, &self.acls, &self.rules, &p.values).await {
            Ok(id) => Ok(ok_json(&json!({ "id": id }))),
            Err(e) => Ok(db_err(e)),
        }
    }

    pub async fn update_record_inner(&self, ctx: &Ctx, p: UpdateParams) -> ToolResult {
        let Some(model) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        match self.db.update_secured(model, ctx, &self.acls, &self.rules, p.id, &p.values).await {
            Ok(n) if n > 0 => Ok(ok_json(&json!({ "updated": true }))),
            Ok(_) => Ok(arg_err("not found or not permitted")),
            Err(e) => Ok(db_err(e)),
        }
    }

    pub async fn delete_record_inner(&self, ctx: &Ctx, p: RecordParam) -> ToolResult {
        let Some(model) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        match self.db.delete_secured(model, ctx, &self.acls, &self.rules, p.id).await {
            Ok(n) if n > 0 => Ok(ok_json(&json!({ "deleted": true }))),
            Ok(_) => Ok(arg_err("not found or not permitted")),
            Err(e) => Ok(db_err(e)),
        }
    }

    pub async fn run_action_inner(&self, ctx: &Ctx, p: ActionParams) -> ToolResult {
        let Some(model) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        match self.db.run_action(model, ctx, &self.acls, &self.rules, p.id, &p.action).await {
            Ok(()) => Ok(ok_json(&json!({ "ok": true, "action": p.action }))),
            Err(e) => Ok(db_err(e)),
        }
    }

    pub async fn run_service_inner(&self, ctx: &Ctx, p: ServiceParams) -> ToolResult {
        let Some(model) = self.models.get(&p.model) else {
            return Ok(arg_err(format!("unknown model '{}'", p.model)));
        };
        let body = p.body.unwrap_or_default();
        match self.db.run_service(model, ctx, &self.acls, &self.rules, p.id, &p.service, body).await {
            Ok(out) => Ok(ok_json(&out)),
            Err(e) => Ok(db_err(e)),
        }
    }

    pub async fn post_message_inner(&self, ctx: &Ctx, p: MessageParams) -> ToolResult {
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
        match self.db.find_one_secured(host, ctx, &self.acls, &self.rules, p.id).await {
            Ok(Some(_)) => {}
            Ok(None) => return Ok(arg_err("not found or not permitted")),
            Err(e) => return Ok(db_err(e)),
        }
        let mut values = Map::new();
        values.insert("res_model".into(), Json::String(p.model));
        values.insert("res_id".into(), Json::Number(p.id.into()));
        values.insert("author_id".into(), Json::Number(ctx.uid.into()));
        values.insert("body".into(), Json::String(p.body));
        values.insert("message_type".into(), Json::String("comment".into()));
        let elevated = ctx.sudo();
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
        // Merge the runtime custom fields (like the server's live map) so a field added via the
        // API is read/written here too — snapshot at connect, so a mid-session add needs a
        // reconnect. Ignored if the registry table is absent (a bare, never-migrated DB).
        let custom = db.custom_fields_by_model().await.unwrap_or_default();
        Self::with_overlays(db, Auth::Fixed(ctx), &custom)
    }

    /// Builds the server with an explicit `Ctx` and no runtime overlays — the compiled-in catalog
    /// only (embedding, tests).
    pub fn with_ctx(db: Db, ctx: Ctx) -> Result<Self, DbError> {
        Self::with_overlays(db, Auth::Fixed(ctx), &std::collections::HashMap::new())
    }

    /// Builds an HTTP server: every tool call authenticates the request's API key (no fixed
    /// identity). Merges runtime custom fields once at startup. This is the network-facing surface
    /// — a caller presents `Authorization: Bearer kg_...` and acts as that key's user.
    pub async fn for_http(db: Db) -> Result<Self, DbError> {
        let custom = db.custom_fields_by_model().await.unwrap_or_default();
        Self::with_overlays(db, Auth::PerRequest, &custom)
    }

    /// Builds the server, extending each compiled-in model with its runtime custom fields. A
    /// catalog that fails to resolve REFUSES to serve (review must-fix: silently serving zero
    /// models looks "up" while every tool answers unknown-model).
    fn with_overlays(
        db: Db,
        auth: Auth,
        custom: &HashMap<String, Vec<kigumi_core::FieldDef>>,
    ) -> Result<Self, DbError> {
        let resolved = resolve_all_registered().map_err(DbError::Migration)?;
        let models = resolved
            .into_iter()
            .map(|mut m| {
                if let Some(extra) = custom.get(m.name) {
                    m.fields.extend(extra.iter().cloned());
                }
                (m.name.to_string(), m)
            })
            .collect();
        Ok(KigumiMcp {
            db,
            auth,
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

/// Serves MCP over streamable HTTP at `bind`, authenticating each request's API key. Endpoint path
/// is `/mcp`. The runtime custom fields are snapshotted ONCE at startup and shared by every
/// session (the identity is per-request, so a single catalog config is correct); a mid-run field
/// add needs a restart, as documented for the static-catalog model.
pub async fn serve_http(db: Db, bind: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };
    // Snapshot custom fields once; a broken catalog fails startup here (not per request).
    let custom = db.custom_fields_by_model().await.unwrap_or_default();
    let _ = KigumiMcp::with_overlays(db.clone(), Auth::PerRequest, &custom)?;
    let custom = Arc::new(custom);
    let service = StreamableHttpService::new(
        move || {
            KigumiMcp::with_overlays(db.clone(), Auth::PerRequest, &custom)
                .map_err(|e| std::io::Error::other(format!("{e:?}")))
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let app = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("kigumi MCP (HTTP) serving on http://{bind}/mcp");
    axum::serve(listener, app).await?;
    Ok(())
}
