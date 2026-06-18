//! Headless HTTP layer (axum). Serves the integration surface from a model set:
//! the OpenAPI spec, the model list, per-model UI contracts, and — when a database backend is
//! provided — secured data endpoints that enforce the ACL + record-rule engine.
//!
//! The server is agnostic of any module: a host wires its catalog in with
//! `meshble_core::resolve_all_registered()` and its security policy, then calls [`router`] or
//! [`router_with_data`]. The core stays headless; this crate is optional.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde_json::Value as Json2;
use meshble_auth::{hash_password, new_jti, verify_password, Authenticator};
use meshble_core::{
    check_access, field_accessible, is_mailed, wizard_for, Acl, Condition, Ctx, Domain, FieldKind,
    Operation, Operator, RecordRule, ResolvedModel, Value, WizardContext,
};
use meshble_db::{Db, DbError};
use meshble_schema::{openapi, to_ui_contract};
use meshble_storage::{sha256_hex, BlobStore};

// Re-exported so hosts (the CLI) and tests can construct a store without a direct meshble-storage dep.
pub use meshble_storage::FsBlobStore;

/// Access tokens are short-lived; refresh tokens long-lived (and revocable/rotated server-side).
const ACCESS_TTL: u64 = 900; // 15 minutes
const REFRESH_TTL: u64 = 2_592_000; // 30 days
/// Maximum request body, bounding an upload (and any JSON write) in memory. Explicit so it is neither
/// axum's restrictive 2 MB default nor unbounded. Config-driven sizing is a later enhancement.
const MAX_BODY_BYTES: usize = 25 * 1024 * 1024;

#[derive(Clone)]
struct AppState {
    models: Arc<Vec<ResolvedModel>>,
    data: Option<DataBackend>,
}

#[derive(Clone)]
struct DataBackend {
    db: Arc<Db>,
    acls: &'static [Acl],
    rules: &'static [RecordRule],
    auth: Arc<Authenticator>,
    blobs: Arc<dyn BlobStore>,
}

fn base_router() -> Router<AppState> {
    Router::new()
        .route("/openapi.json", get(openapi_handler))
        .route("/api/models", get(models_handler))
        .route("/api/:name/view", get(view_handler))
}

/// Metadata-only router: OpenAPI spec, model list, UI contracts. No database.
pub fn router(models: Vec<ResolvedModel>) -> Router {
    base_router().with_state(AppState { models: Arc::new(models), data: None })
}

/// Full router: metadata routes plus secured CRUD data endpoints. `auth_secret` is the HS256
/// secret used to verify the `Authorization: Bearer <token>` of each data request into a `Ctx`.
#[allow(clippy::too_many_arguments)]
pub fn router_with_data(
    models: Vec<ResolvedModel>,
    db: Db,
    acls: &'static [Acl],
    rules: &'static [RecordRule],
    auth_secret: impl Into<String>,
    blobs: Arc<dyn BlobStore>,
) -> Router {
    base_router()
        .route("/auth/login", post(login_handler))
        .route("/auth/refresh", post(refresh_handler))
        .route("/auth/logout", post(logout_handler))
        .route("/auth/me", get(me_handler))
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .route("/api/:name", get(list_handler).post(create_handler))
        .route(
            "/api/:name/:id",
            get(get_one_handler).patch(update_handler).delete(delete_handler),
        )
        .route("/api/:name/:id/action/:action", post(action_handler))
        // Variant generation: materialize a product.template's attribute combinations into variants.
        .route("/api/:name/:id/generate_variants", post(generate_variants_handler))
        // Re-price a sale order's lines from its pricelist.
        .route("/api/:name/:id/apply_pricelist", post(apply_pricelist_handler))
        // Open a wizard (transient model): seed it via default_get and return the scratchpad record.
        .route("/api/:name/open", post(open_wizard_handler))
        // Attachments (ir.attachment): files on a record. List/download need host read; upload/delete
        // need host write. Bytes live in the content-addressed blob store; the row is metadata.
        .route("/api/:name/:id/attachments", get(list_attachments_handler).post(upload_attachment_handler))
        .route("/api/attachment/:aid/content", get(download_attachment_handler))
        .route("/api/attachment/:aid", delete(delete_attachment_handler))
        // Chatter (mail subsystem): a record's message thread. Gated by read access to the host.
        .route("/api/:name/:id/messages", get(messages_handler))
        .route("/api/:name/:id/message", post(post_message_handler))
        // Activities (to-dos) on a record: list open ones (state derived), schedule, mark done.
        .route("/api/:name/:id/activities", get(activities_handler))
        .route("/api/:name/:id/activity", post(schedule_activity_handler))
        .route("/api/:name/:id/activities/:aid/done", post(activity_done_handler))
        // Followers: who is subscribed to a record's thread. Follow/unfollow are idempotent.
        .route("/api/:name/:id/followers", get(followers_handler))
        .route("/api/:name/:id/follow", post(follow_handler))
        .route("/api/:name/:id/unfollow", post(unfollow_handler))
        .with_state(AppState {
            models: Arc::new(models),
            data: Some(DataBackend {
                db: Arc::new(db),
                acls,
                rules,
                auth: Arc::new(Authenticator::new(auth_secret)),
                blobs,
            }),
        })
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
}

/// Verifies the request's bearer token into a trusted `Ctx`, or a 401 response. This is real
/// authentication: a client cannot claim a group without a token signed by the server secret.
fn authenticate(backend: &DataBackend, headers: &HeaderMap) -> Result<Ctx, Response> {
    let header = headers.get("authorization").and_then(|v| v.to_str().ok());
    backend
        .auth
        .verify_bearer(header)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "unauthorized").into_response())
}

fn json_response(body: String) -> Response {
    ([("content-type", "application/json")], body).into_response()
}

fn json_status(status: StatusCode, body: String) -> Response {
    (status, [("content-type", "application/json")], body).into_response()
}

/// Resolves the model for a path name, or a 404 response.
fn resolve_model<'a>(state: &'a AppState, name: &str) -> Result<&'a ResolvedModel, Response> {
    state
        .models
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("unknown model: {name}")).into_response())
}

/// Maps a write DbError to an HTTP response (opaque 500, never leaking schema/SQL on the 500 path).
fn write_error(context: &str, e: DbError) -> Response {
    match e {
        DbError::AccessDenied { .. } => (StatusCode::FORBIDDEN, "access denied").into_response(),
        DbError::BadInput(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        DbError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        other => internal_error(context, other),
    }
}

async fn openapi_handler(State(state): State<AppState>) -> Response {
    let refs: Vec<&ResolvedModel> = state.models.iter().collect();
    json_response(openapi(&refs))
}

async fn models_handler(State(state): State<AppState>) -> Json<Vec<String>> {
    Json(state.models.iter().map(|m| m.name.to_string()).collect())
}

/// Logs the detail server-side and returns an opaque 500 — internal schema/SQL detail must never
/// reach the client (it would leak table/column names and Postgres error text).
fn internal_error(context: &str, detail: impl std::fmt::Debug) -> Response {
    eprintln!("meshble-server {context} error: {detail:?}");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
}

/// Liveness: the process is up. No DB touch — safe for a fast container health probe.
async fn health_handler() -> Response {
    json_response("{\"status\":\"ok\"}".to_string())
}

/// Readiness: the process can serve traffic (database reachable). 503 until it can.
async fn ready_handler(State(state): State<AppState>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    match backend.db.ping().await {
        Ok(_) => json_response("{\"status\":\"ready\"}".to_string()),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "{\"status\":\"not_ready\"}").into_response(),
    }
}

/// The authenticated caller's own identity — the trusted `Ctx` derived from the bearer token.
async fn me_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let body = serde_json::json!({
        "uid": ctx.uid,
        "groups": ctx.groups,
        "company_id": ctx.company_id,
        "allowed_company_ids": ctx.allowed_company_ids,
    });
    json_response(body.to_string())
}

async fn view_handler(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    match state.models.iter().find(|m| m.name == name) {
        Some(m) => match to_ui_contract(m, &[]) {
            Ok(json) => json_response(json),
            Err(e) => internal_error("view", e),
        },
        None => (StatusCode::NOT_FOUND, format!("unknown model: {name}")).into_response(),
    }
}

const DEFAULT_LIMIT: i64 = 80;
const MAX_LIMIT: i64 = 500;

async fn list_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let model = match state.models.iter().find(|m| m.name == name) {
        Some(m) => m,
        None => return (StatusCode::NOT_FOUND, format!("unknown model: {name}")).into_response(),
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let (filter, order, limit, offset) = match parse_list_query(model, &params) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match backend
        .db
        .list_secured(model, &ctx, backend.acls, backend.rules, filter.as_ref(), &order, limit, offset)
        .await
    {
        Ok(page) => {
            let body = serde_json::json!({
                "data": page.data, "total": page.total, "limit": limit, "offset": offset,
            });
            json_response(body.to_string())
        }
        Err(DbError::AccessDenied { .. }) => (StatusCode::FORBIDDEN, "access denied").into_response(),
        Err(DbError::BadInput(msg)) => bad_request(msg),
        Err(DbError::Domain(e)) => bad_request(format!("invalid filter: {e:?}")),
        Err(e) => internal_error("data", e),
    }
}

fn bad_request(msg: String) -> Response {
    (StatusCode::BAD_REQUEST, msg).into_response()
}

/// Parses list query params into (filter, order, limit, offset). Two filter forms (decision D5):
/// suffix operators `field__op=value` (the default, AND-ed) and a `?domain=<json AST>` escape for
/// arbitrary AND/OR/NOT — AND-ed together when both are present.
fn parse_list_query(
    model: &ResolvedModel,
    params: &HashMap<String, String>,
) -> Result<(Option<Domain>, Vec<(String, bool)>, i64, i64), Response> {
    let mut conds: Vec<Domain> = Vec::new();
    if let Some(js) = params.get("domain") {
        match Domain::from_json(js) {
            Ok(d) => conds.push(d),
            Err(e) => return Err(bad_request(format!("invalid domain JSON: {e:?}"))),
        }
    }
    for (key, raw) in params {
        if matches!(key.as_str(), "domain" | "order" | "limit" | "offset") {
            continue;
        }
        let (field, op) = split_suffix(key);
        conds.push(build_leaf(model, field, op, raw).map_err(bad_request)?);
    }
    let filter = conds.into_iter().reduce(|a, b| a.and(b));

    let mut order = Vec::new();
    if let Some(o) = params.get("order") {
        for part in o.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            match part.strip_prefix('-') {
                Some(f) => order.push((f.to_string(), true)),
                None => order.push((part.to_string(), false)),
            }
        }
    }

    let limit = match params.get("limit") {
        Some(s) => s.parse::<i64>().map_err(|_| bad_request("limit must be an integer".into()))?.clamp(1, MAX_LIMIT),
        None => DEFAULT_LIMIT,
    };
    let offset = match params.get("offset") {
        Some(s) => s.parse::<i64>().map_err(|_| bad_request("offset must be an integer".into()))?.max(0),
        None => 0,
    };
    Ok((filter, order, limit, offset))
}

/// Splits `field__op` into (field, op); a bare `field` defaults to the `eq` operator.
fn split_suffix(key: &str) -> (&str, &str) {
    match key.rfind("__") {
        Some(i) => (&key[..i], &key[i + 2..]),
        None => (key, "eq"),
    }
}

/// Builds one leaf condition, coercing the raw string to the field's typed value.
fn build_leaf(model: &ResolvedModel, field: &str, op: &str, raw: &str) -> Result<Domain, String> {
    let kind = if field == "id" {
        FieldKind::Integer
    } else {
        model
            .fields
            .iter()
            .find(|f| f.name == field)
            .map(|f| f.kind)
            .ok_or_else(|| format!("unknown filter field '{field}'"))?
    };
    let operator = match op {
        "eq" => Operator::Eq,
        "ne" => Operator::Ne,
        "gt" => Operator::Gt,
        "gte" => Operator::Ge,
        "lt" => Operator::Lt,
        "lte" => Operator::Le,
        "like" => Operator::Like,
        "ilike" => Operator::ILike,
        "in" => Operator::In,
        other => return Err(format!("unknown operator suffix '__{other}'")),
    };
    let value = if operator == Operator::In {
        Value::List(raw.split(',').map(|s| coerce(&kind, s.trim())).collect::<Result<Vec<_>, _>>()?)
    } else {
        coerce(&kind, raw)?
    };
    Ok(Domain::Cond(Condition { field: field.to_string(), op: operator, value }))
}

/// Coerces a query string to a typed [`Value`] for the field's kind.
fn coerce(kind: &FieldKind, raw: &str) -> Result<Value, String> {
    Ok(match kind {
        FieldKind::Integer | FieldKind::Many2one { .. } | FieldKind::Image => {
            Value::Int(raw.parse().map_err(|_| format!("'{raw}' is not an integer"))?)
        }
        FieldKind::Decimal { .. } => {
            Value::Decimal(raw.parse::<rust_decimal::Decimal>().map_err(|_| format!("'{raw}' is not a number"))?)
        }
        FieldKind::Float => Value::Float(raw.parse::<f64>().map_err(|_| format!("'{raw}' is not a number"))?),
        FieldKind::Bool => Value::Bool(matches!(raw, "true" | "1" | "t" | "yes")),
        // Date/Datetime filters travel as ISO strings; Postgres validates the cast at query time.
        FieldKind::Text | FieldKind::Html | FieldKind::Selection(_) | FieldKind::Date | FieldKind::Datetime => {
            Value::Str(raw.to_string())
        }
        FieldKind::One2many { .. } | FieldKind::Many2many { .. } => {
            return Err("cannot filter on a One2many/Many2many field directly".to_string())
        }
    })
}

async fn get_one_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    match backend.db.find_one_secured(model, &ctx, backend.acls, backend.rules, id).await {
        Ok(Some(obj)) => {
            json_response(serde_json::to_string(&obj).unwrap_or_else(|_| "{}".to_string()))
        }
        Ok(None) => (StatusCode::NOT_FOUND, "not found or not permitted").into_response(),
        Err(DbError::AccessDenied { .. }) => (StatusCode::FORBIDDEN, "access denied").into_response(),
        Err(e) => internal_error("data", e),
    }
}

fn body_object(body: &Json2) -> Result<&serde_json::Map<String, Json2>, Response> {
    body.as_object()
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "body must be a JSON object").into_response())
}

async fn create_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Json2>,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let obj = match body_object(&body) {
        Ok(o) => o,
        Err(r) => return r,
    };
    match backend.db.insert_secured(model, &ctx, backend.acls, backend.rules, obj).await {
        Ok(id) => json_status(StatusCode::CREATED, format!("{{\"id\": {id}}}")),
        Err(e) => write_error("create", e),
    }
}

async fn update_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
    Json(body): Json<Json2>,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let obj = match body_object(&body) {
        Ok(o) => o,
        Err(r) => return r,
    };
    match backend.db.update_secured(model, &ctx, backend.acls, backend.rules, id, obj).await {
        Ok(0) => (StatusCode::NOT_FOUND, "not found or not permitted").into_response(),
        Ok(n) => json_status(StatusCode::OK, format!("{{\"updated\": {n}}}")),
        Err(e) => write_error("update", e),
    }
}

/// Runs a registered state-transition action on a record (e.g. confirm a draft order).
async fn action_handler(
    State(state): State<AppState>,
    Path((name, id, action)): Path<(String, i64, String)>,
    headers: HeaderMap,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    match backend.db.run_action(model, &ctx, backend.acls, backend.rules, id, &action).await {
        Ok(()) => json_response(format!("{{\"ok\":true,\"action\":{}}}", serde_json::to_string(&action).unwrap_or_default())),
        Err(e) => write_error("action", e),
    }
}

/// Generates `product.product` variants for a `product.template` (the cartesian product of its
/// attribute lines). v1: product-template-specific, so the path name is pinned. Authorization +
/// reconciliation live in the db layer; this handler only authenticates and shapes the response.
async fn generate_variants_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    if name != "product.template" {
        return (StatusCode::BAD_REQUEST, "generate_variants is only valid on product.template")
            .into_response();
    }
    // 404 if product.template is not served (its module isn't installed).
    if let Err(r) = resolve_model(&state, &name) {
        return r;
    }
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    match backend.db.generate_variants(&ctx, backend.acls, backend.rules, id).await {
        Ok(o) => json_response(
            serde_json::json!({ "created": o.created, "archived": o.archived, "kept": o.kept })
                .to_string(),
        ),
        Err(e) => write_error("generate_variants", e),
    }
}

/// Re-prices a sale order's lines from its pricelist. v1: pinned to sale.order. Authorization +
/// currency check live in the db layer (apply_pricelist gates on order write, runs as the caller).
async fn apply_pricelist_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    if name != "sale.order" {
        return (StatusCode::BAD_REQUEST, "apply_pricelist is only valid on sale.order").into_response();
    }
    if let Err(r) = resolve_model(&state, &name) {
        return r;
    }
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    match backend.db.apply_pricelist(&ctx, backend.acls, backend.rules, id).await {
        Ok(n) => json_response(serde_json::json!({ "priced": n }).to_string()),
        Err(e) => write_error("apply_pricelist", e),
    }
}

/// Converts a metamodel [`Value`] into JSON for the secured insert path (decimals stay strings to
/// preserve precision, matching the rest of the API).
fn value_to_json(v: &Value) -> Json2 {
    match v {
        Value::Str(s) => Json2::String(s.clone()),
        Value::Int(n) => Json2::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f).map(Json2::Number).unwrap_or(Json2::Null),
        Value::Decimal(d) => Json2::String(d.to_string()),
        Value::Bool(b) => Json2::Bool(*b),
        Value::Null => Json2::Null,
        Value::List(xs) => Json2::Array(xs.iter().map(value_to_json).collect()),
    }
}

/// Opens a wizard (transient model): computes its server-side defaults from the open context, creates
/// the scratchpad row under the caller, and returns it for the frontend to contract-render. The model
/// must be `register_wizard!`-bound (else 400). Authorization is the normal create ACL on the model.
async fn open_wizard_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Json2>,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let Some(wizard) = wizard_for(&name) else {
        return (StatusCode::BAD_REQUEST, "not a wizard model").into_response();
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    // The open context (Odoo's active_model / active_id / active_ids) — all optional.
    let wctx = WizardContext {
        active_model: body.get("active_model").and_then(|v| v.as_str()).map(str::to_string),
        active_id: body.get("active_id").and_then(Json2::as_i64),
        active_ids: body
            .get("active_ids")
            .and_then(Json2::as_array)
            .map(|a| a.iter().filter_map(Json2::as_i64).collect())
            .unwrap_or_default(),
    };
    let seed: serde_json::Map<String, Json2> = (wizard.default_get)(&wctx)
        .into_iter()
        .map(|(k, v)| (k.to_string(), value_to_json(&v)))
        .collect();
    let id = match backend.db.insert_secured(model, &ctx, backend.acls, backend.rules, &seed).await {
        Ok(id) => id,
        Err(e) => return write_error("open", e),
    };
    match backend.db.find_one_secured(model, &ctx, backend.acls, backend.rules, id).await {
        Ok(Some(rec)) => {
            json_status(StatusCode::CREATED, serde_json::to_string(&rec).unwrap_or_else(|_| "{}".to_string()))
        }
        Ok(None) => (StatusCode::NOT_FOUND, "not found or not permitted").into_response(),
        Err(e) => write_error("open", e),
    }
}

async fn delete_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    match backend.db.delete_secured(model, &ctx, backend.acls, backend.rules, id).await {
        Ok(0) => (StatusCode::NOT_FOUND, "not found or not permitted").into_response(),
        Ok(n) => json_status(StatusCode::OK, format!("{{\"deleted\": {n}}}")),
        Err(e) => write_error("delete", e),
    }
}

fn str_field<'a>(body: &'a Json2, key: &str) -> Option<&'a str> {
    body.get(key).and_then(|v| v.as_str())
}

/// The target user id from a request body's optional `user_id` (defaulting to `default`, the caller).
/// A present-but-non-integer value is a 400 — never silently fall back to the caller.
fn body_user_id(body: &Json2, default: i64) -> Result<i64, Response> {
    match body.get("user_id") {
        None | Some(Json2::Null) => Ok(default),
        Some(v) => v
            .as_i64()
            .ok_or_else(|| (StatusCode::BAD_REQUEST, "user_id must be an integer").into_response()),
    }
}

/// Authorizes acting on another user's behalf: only the `admin` group may target a `uid` other than
/// the caller's own. Prevents a normal user from force-(un)subscribing arbitrary users (IDOR).
fn ensure_self_or_admin(ctx: &Ctx, uid: i64) -> Result<(), Response> {
    if uid == ctx.uid || ctx.is_member("admin") {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "cannot manage another user's subscription").into_response())
    }
}

/// Resolves a served model by exact name, or a 500 (the owning module isn't installed/served). Used
/// for the mail subsystem's internal models (mail.message, mail.activity) the chatter endpoints act on.
fn served_model<'a>(state: &'a AppState, name: &str) -> Result<&'a ResolvedModel, Response> {
    state
        .models
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| internal_error("mail", format!("{name} not served (mail module not installed)")))
}

/// The shared opening of every chatter/activity/follower endpoint: take the data backend,
/// authenticate the caller, gate on READ access to the host record `(name, id)`, and resolve the
/// served mail model to act on. Returns `(backend, ctx, model)` or the `Response` to return. One
/// place for the access decision, so no handler can forget the host-read gate.
async fn chatter_setup<'a>(
    state: &'a AppState,
    headers: &HeaderMap,
    name: &str,
    id: i64,
    model_name: &str,
) -> Result<(&'a DataBackend, Ctx, &'a ResolvedModel), Response> {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = authenticate(backend, headers)?;
    chatter_gate(state, backend, &ctx, name, id).await?;
    let model = served_model(state, model_name)?;
    Ok((backend, ctx, model))
}

/// Gates a chatter request: the host model must opt into mail, AND the caller must be able to READ
/// the host record — you cannot see or post to the thread of a record you cannot read. The single
/// access chokepoint for both thread endpoints (reuses the secured read path, no bespoke check).
async fn chatter_gate(
    state: &AppState,
    backend: &DataBackend,
    ctx: &Ctx,
    name: &str,
    id: i64,
) -> Result<(), Response> {
    let host = resolve_model(state, name)?;
    if !is_mailed(name) {
        return Err((StatusCode::BAD_REQUEST, format!("model '{name}' has no mail thread")).into_response());
    }
    match backend.db.find_one_secured(host, ctx, backend.acls, backend.rules, id).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err((StatusCode::NOT_FOUND, "not found or not permitted").into_response()),
        Err(DbError::AccessDenied { .. }) => Err((StatusCode::FORBIDDEN, "access denied").into_response()),
        Err(e) => Err(internal_error("chatter-gate", e)),
    }
}

/// The polymorphic filter for one record's thread: `res_model = name AND res_id = id`.
fn thread_filter(name: &str, id: i64) -> Domain {
    Domain::Cond(Condition { field: "res_model".into(), op: Operator::Eq, value: Value::Str(name.to_string()) })
        .and(Domain::Cond(Condition { field: "res_id".into(), op: Operator::Eq, value: Value::Int(id) }))
}

/// Strips a header-unsafe string down to printable ASCII (no control chars, no `"`), or a fallback —
/// so a stored filename / mimetype cannot inject into a response header on download.
fn header_safe(s: &str, fallback: &str) -> String {
    let cleaned: String =
        s.chars().filter(|c| c.is_ascii() && !c.is_ascii_control() && *c != '"').collect();
    if cleaned.trim().is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

/// Gates an attachment request: the host model must be served and the host record visible to the
/// caller (read). For a mutation (`write`), the caller must also hold Write on the host model — you
/// modify a record's attachment set only if you can modify the record. The single access chokepoint.
async fn attachment_gate(
    state: &AppState,
    backend: &DataBackend,
    ctx: &Ctx,
    name: &str,
    id: i64,
    write: bool,
) -> Result<(), Response> {
    let host = resolve_model(state, name)?;
    if write && !check_access(Operation::Write, name, ctx, backend.acls) {
        return Err((StatusCode::FORBIDDEN, "access denied").into_response());
    }
    match backend.db.find_one_secured(host, ctx, backend.acls, backend.rules, id).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err((StatusCode::NOT_FOUND, "not found or not permitted").into_response()),
        Err(DbError::AccessDenied { .. }) => Err((StatusCode::FORBIDDEN, "access denied").into_response()),
        Err(e) => Err(internal_error("attachment-gate", e)),
    }
}

/// Shared opening of the host-anchored attachment endpoints: authenticate, gate on the host record,
/// resolve the served `ir.attachment` model. Returns `(backend, ctx, attachment_model)`.
async fn attachment_setup<'a>(
    state: &'a AppState,
    headers: &HeaderMap,
    name: &str,
    id: i64,
    write: bool,
) -> Result<(&'a DataBackend, Ctx, &'a ResolvedModel), Response> {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = authenticate(backend, headers)?;
    attachment_gate(state, backend, &ctx, name, id, write).await?;
    let model = served_model(state, "ir.attachment")?;
    Ok((backend, ctx, model))
}

/// `GET /api/:name/:id/attachments` — list a record's attachment metadata (no bytes). Host read gate.
async fn list_attachments_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let (backend, ctx, att) = match attachment_setup(&state, &headers, &name, id, false).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    // The gate IS the access decision; read the (admin-only) attachment model elevated.
    let su = ctx.sudo();
    match backend.db.find_secured(att, &su, backend.acls, backend.rules, Some(&thread_filter(&name, id))).await {
        Ok(rows) => json_response(serde_json::json!({ "data": rows }).to_string()),
        Err(e) => write_error("attachments", e),
    }
}

/// `POST /api/:name/:id/attachments` — upload a file onto a record. Host WRITE gate. The raw body is
/// the bytes; `X-Filename` and `Content-Type` headers carry the name and mimetype.
async fn upload_attachment_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (backend, ctx, att) = match attachment_setup(&state, &headers, &name, id, true).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty upload").into_response();
    }
    let filename = headers.get("x-filename").and_then(|v| v.to_str().ok()).unwrap_or("file").to_string();
    let mimetype = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_string();
    let sha = sha256_hex(&body);
    // Store the bytes (content-addressed, verified, deduplicated) BEFORE recording the metadata.
    if let Err(e) = backend.blobs.put(&sha, &body).await {
        return internal_error("attachment-put", e);
    }
    let su = ctx.sudo();
    let payload = serde_json::json!({
        "name": filename,
        "res_model": name,
        "res_id": id,
        "mimetype": mimetype,
        "file_size": body.len() as i64,
        "checksum": sha,
    });
    match backend.db.insert_secured(att, &su, backend.acls, backend.rules, payload.as_object().unwrap()).await {
        Ok(aid) => json_status(
            StatusCode::CREATED,
            serde_json::json!({ "id": aid, "name": filename, "mimetype": mimetype, "file_size": body.len(), "checksum": sha }).to_string(),
        ),
        Err(e) => write_error("attachment", e),
    }
}

/// `GET /api/attachment/:aid/content` — stream an attachment's bytes. Gated by READ on its HOST record.
async fn download_attachment_handler(
    State(state): State<AppState>,
    Path(aid): Path<i64>,
    headers: HeaderMap,
) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let att = match served_model(&state, "ir.attachment") {
        Ok(m) => m,
        Err(r) => return r,
    };
    // Read the attachment row elevated, then gate on READ of the host record it is attached to.
    let su = ctx.sudo();
    let row = match backend.db.find_one_secured(att, &su, backend.acls, backend.rules, aid).await {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::NOT_FOUND, "attachment not found").into_response(),
        Err(e) => return write_error("attachment", e),
    };
    let res_model = row.get("res_model").and_then(|v| v.as_str()).unwrap_or("");
    let res_id = row.get("res_id").and_then(|v| v.as_i64()).unwrap_or(0);
    if let Err(r) = attachment_gate(&state, backend, &ctx, res_model, res_id, false).await {
        return r;
    }
    let checksum = row.get("checksum").and_then(|v| v.as_str()).unwrap_or("");
    let mimetype = header_safe(row.get("mimetype").and_then(|v| v.as_str()).unwrap_or(""), "application/octet-stream");
    let filename = header_safe(row.get("name").and_then(|v| v.as_str()).unwrap_or(""), "file");
    // Serve INLINE only for a safe allowlist (images / pdf). Anything else — notably a user-uploaded
    // text/html blob — is forced to download (`attachment`) with `nosniff`, so it can never execute as
    // script in the app's origin. The uploader controls the mimetype, so inline-by-default is unsafe.
    let disposition = match mimetype.as_str() {
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "application/pdf" => "inline",
        _ => "attachment",
    };
    match backend.blobs.get(checksum).await {
        Ok(bytes) => (
            [
                ("content-type", mimetype),
                ("content-disposition", format!("{disposition}; filename=\"{filename}\"")),
                ("x-content-type-options", "nosniff".to_string()),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => internal_error("attachment-get", e),
    }
}

/// `DELETE /api/attachment/:aid` — remove an attachment row. Gated by WRITE on its HOST record. The
/// blob is left for GC (it may be shared by another attachment via content-address dedup).
async fn delete_attachment_handler(
    State(state): State<AppState>,
    Path(aid): Path<i64>,
    headers: HeaderMap,
) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let att = match served_model(&state, "ir.attachment") {
        Ok(m) => m,
        Err(r) => return r,
    };
    let su = ctx.sudo();
    let row = match backend.db.find_one_secured(att, &su, backend.acls, backend.rules, aid).await {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::NOT_FOUND, "attachment not found").into_response(),
        Err(e) => return write_error("attachment", e),
    };
    let res_model = row.get("res_model").and_then(|v| v.as_str()).unwrap_or("");
    let res_id = row.get("res_id").and_then(|v| v.as_i64()).unwrap_or(0);
    if let Err(r) = attachment_gate(&state, backend, &ctx, res_model, res_id, true).await {
        return r;
    }
    match backend.db.delete_secured(att, &su, backend.acls, backend.rules, aid).await {
        Ok(0) => (StatusCode::NOT_FOUND, "attachment not found").into_response(),
        Ok(_) => json_response(serde_json::json!({ "deleted": 1 }).to_string()),
        Err(e) => write_error("attachment", e),
    }
}

/// `GET /api/:name/:id/messages` — the record's message thread, oldest first (by id).
async fn messages_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let (backend, ctx, mail) = match chatter_setup(&state, &headers, &name, id, "mail.message").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    // The gate (host readable) IS the access decision; read the thread elevated so the framework
    // isn't forced to grant users a blanket read ACL on mail.message (which would leak all threads).
    let su = ctx.sudo();
    let filter = thread_filter(&name, id);
    match backend.db.find_secured(mail, &su, backend.acls, backend.rules, Some(&filter)).await {
        Ok(rows) => {
            // Embed each message's field-change audit (mail.tracking) so a notification message
            // carries its old→new diffs — one thread payload, comments and audit uniform.
            let ids: Vec<i64> = rows.iter().filter_map(|m| m.get("id").and_then(|v| v.as_i64())).collect();
            let tracking = match backend.db.tracking_for(&ids).await {
                Ok(t) => t,
                Err(e) => {
                    // A DB error here must not be hidden as "no audit"; log it (the messages still return).
                    eprintln!("meshble-server messages tracking enrichment failed: {e:?}");
                    Vec::new()
                }
            };
            // D6: redact tracking of fields the caller may not read (field-level security). The audit
            // trail must not become a second, unguarded read channel for group-restricted field values.
            let mut by_msg: HashMap<i64, Vec<Json2>> = HashMap::new();
            for t in tracking {
                let readable = t
                    .get("field")
                    .and_then(|v| v.as_str())
                    .map(|f| field_accessible(&name, f, &ctx))
                    .unwrap_or(false);
                if !readable {
                    continue;
                }
                if let Some(mid) = t.get("message_id").and_then(|v| v.as_i64()) {
                    by_msg.entry(mid).or_default().push(t);
                }
            }
            let enriched: Vec<Json2> = rows
                .into_iter()
                .map(|mut m| {
                    if let Some(mid) = m.get("id").and_then(|v| v.as_i64()) {
                        if let Some(obj) = m.as_object_mut() {
                            obj.insert("tracking".into(), Json2::Array(by_msg.remove(&mid).unwrap_or_default()));
                        }
                    }
                    m
                })
                .collect();
            json_response(serde_json::json!({ "data": enriched }).to_string())
        }
        Err(DbError::AccessDenied { .. }) => (StatusCode::FORBIDDEN, "access denied").into_response(),
        Err(e) => internal_error("messages", e),
    }
}

/// `POST /api/:name/:id/message` — post a comment (or log note) to the record's thread. The author
/// is the authenticated caller; the timestamp is the DB clock. `notification` type is reserved for
/// system tracking entries (a later slice), so only `comment`/`note` are accepted here.
async fn post_message_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
    Json(body): Json<Json2>,
) -> Response {
    let (backend, ctx, mail) = match chatter_setup(&state, &headers, &name, id, "mail.message").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let text = match str_field(&body, "body") {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => return (StatusCode::BAD_REQUEST, "message body is required").into_response(),
    };
    let mtype = match str_field(&body, "message_type") {
        None | Some("comment") => "comment",
        Some("note") => "note",
        Some(other) => return (StatusCode::BAD_REQUEST, format!("invalid message_type '{other}'")).into_response(),
    };
    let now = match backend.db.now().await {
        Ok(t) => t,
        Err(e) => return internal_error("clock", e),
    };
    let mut values = serde_json::Map::new();
    values.insert("res_model".into(), Json2::String(name.clone()));
    values.insert("res_id".into(), Json2::Number(id.into()));
    values.insert("author_id".into(), Json2::Number(ctx.uid.into()));
    values.insert("body".into(), Json2::String(text));
    values.insert("message_type".into(), Json2::String(mtype.to_string()));
    values.insert("date".into(), Json2::String(now));
    // The gate authorized this post; author stays the real caller (ctx.uid above), but the insert
    // runs elevated so users need no create ACL on mail.message (see the read path / mail ACLs).
    let su = ctx.sudo();
    match backend.db.insert_secured(mail, &su, backend.acls, backend.rules, &values).await {
        Ok(mid) => json_status(StatusCode::CREATED, format!("{{\"id\": {mid}}}")),
        Err(e) => write_error("message", e),
    }
}

/// An activity's state DERIVED from its deadline vs the DB's current date (ISO strings compare
/// lexically). The single place this rule lives (Odoo writes it three times).
fn activity_state(deadline: Option<&str>, today: &str) -> &'static str {
    match deadline {
        Some(d) if d < today => "overdue",
        Some(d) if d == today => "today",
        _ => "planned", // future deadline or none
    }
}

/// `GET /api/:name/:id/activities` — open to-dos on the record, each with a derived state.
async fn activities_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let (backend, ctx, act) = match chatter_setup(&state, &headers, &name, id, "mail.activity").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let today = match backend.db.today().await {
        Ok(t) => t,
        Err(e) => return internal_error("clock", e),
    };
    let su = ctx.sudo();
    let filter = thread_filter(&name, id).and(Domain::Cond(Condition {
        field: "active".into(),
        op: Operator::Eq,
        value: Value::Bool(true),
    }));
    match backend.db.find_secured(act, &su, backend.acls, backend.rules, Some(&filter)).await {
        Ok(rows) => {
            let enriched: Vec<Json2> = rows
                .into_iter()
                .map(|mut a| {
                    let deadline = a.get("date_deadline").and_then(|v| v.as_str()).map(str::to_string);
                    if let Some(obj) = a.as_object_mut() {
                        obj.insert("state".into(), Json2::String(activity_state(deadline.as_deref(), &today).to_string()));
                    }
                    a
                })
                .collect();
            json_response(serde_json::json!({ "data": enriched }).to_string())
        }
        Err(DbError::AccessDenied { .. }) => (StatusCode::FORBIDDEN, "access denied").into_response(),
        Err(e) => internal_error("activities", e),
    }
}

/// `POST /api/:name/:id/activity` — schedule a to-do `{summary, date_deadline?, user_id?}`. The
/// assignee defaults to the caller. Gated on read access to the host.
async fn schedule_activity_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
    Json(body): Json<Json2>,
) -> Response {
    let (backend, ctx, act) = match chatter_setup(&state, &headers, &name, id, "mail.activity").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let summary = match str_field(&body, "summary") {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => return (StatusCode::BAD_REQUEST, "activity summary is required").into_response(),
    };
    // Assignee defaults to the caller; a present-but-non-integer user_id is a client error.
    let assignee = match body_user_id(&body, ctx.uid) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let mut values = serde_json::Map::new();
    values.insert("res_model".into(), Json2::String(name.clone()));
    values.insert("res_id".into(), Json2::Number(id.into()));
    values.insert("summary".into(), Json2::String(summary));
    // An empty/whitespace deadline means "no deadline" (a planned to-do), not a 400.
    if let Some(d) = str_field(&body, "date_deadline").filter(|d| !d.trim().is_empty()) {
        values.insert("date_deadline".into(), Json2::String(d.to_string()));
    }
    values.insert("user_id".into(), Json2::Number(assignee.into()));
    values.insert("active".into(), Json2::Bool(true));
    let su = ctx.sudo();
    match backend.db.insert_secured(act, &su, backend.acls, backend.rules, &values).await {
        Ok(aid) => json_status(StatusCode::CREATED, format!("{{\"id\": {aid}}}")),
        Err(e) => write_error("activity", e),
    }
}

/// `POST /api/:name/:id/activities/:aid/done` — mark a to-do done (`active` → false). The activity
/// must belong to the gated host record (you can't close another record's to-do via a host you can read).
async fn activity_done_handler(
    State(state): State<AppState>,
    Path((name, id, aid)): Path<(String, i64, i64)>,
    headers: HeaderMap,
) -> Response {
    let (backend, ctx, act) = match chatter_setup(&state, &headers, &name, id, "mail.activity").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let su = ctx.sudo();
    // The activity must exist AND belong to this host record.
    let belongs = match backend.db.find_one_secured(act, &su, backend.acls, backend.rules, aid).await {
        Ok(Some(a)) => {
            a.get("res_model").and_then(|v| v.as_str()) == Some(name.as_str())
                && a.get("res_id").and_then(|v| v.as_i64()) == Some(id)
        }
        Ok(None) => false,
        Err(e) => return internal_error("activity-done", e),
    };
    if !belongs {
        return (StatusCode::NOT_FOUND, "activity not found on this record").into_response();
    }
    let mut values = serde_json::Map::new();
    values.insert("active".into(), Json2::Bool(false));
    match backend.db.update_secured(act, &su, backend.acls, backend.rules, aid, &values).await {
        // 0 rows = the activity vanished between the belongs-check and the update (e.g. the host was
        // concurrently deleted, cascading the cleanup). Report it truthfully, don't claim success.
        Ok(0) => (StatusCode::NOT_FOUND, "activity not found on this record").into_response(),
        Ok(_) => json_response("{\"ok\":true}".to_string()),
        Err(e) => write_error("activity-done", e),
    }
}

/// The polymorphic filter for one record's followers, optionally narrowed to a single `user_id`.
fn follower_filter(name: &str, id: i64, user_id: Option<i64>) -> Domain {
    let mut d = thread_filter(name, id);
    if let Some(uid) = user_id {
        d = d.and(Domain::Cond(Condition { field: "user_id".into(), op: Operator::Eq, value: Value::Int(uid) }));
    }
    d
}

/// `GET /api/:name/:id/followers` — the users subscribed to the record's thread.
async fn followers_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let (backend, ctx, foll) = match chatter_setup(&state, &headers, &name, id, "mail.follower").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let su = ctx.sudo();
    let filter = follower_filter(&name, id, None);
    match backend.db.find_secured(foll, &su, backend.acls, backend.rules, Some(&filter)).await {
        Ok(rows) => json_response(serde_json::json!({ "data": rows }).to_string()),
        Err(DbError::AccessDenied { .. }) => (StatusCode::FORBIDDEN, "access denied").into_response(),
        Err(e) => internal_error("followers", e),
    }
}

/// `POST /api/:name/:id/follow` — subscribe a user (default the caller) to the record. Idempotent:
/// re-following an already-followed record is a success, not a conflict (the composite unique index
/// guarantees one subscription per user per record).
async fn follow_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
    Json(body): Json<Json2>,
) -> Response {
    let (backend, ctx, foll) = match chatter_setup(&state, &headers, &name, id, "mail.follower").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let uid = match body_user_id(&body, ctx.uid) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if let Err(r) = ensure_self_or_admin(&ctx, uid) {
        return r;
    }
    let mut values = serde_json::Map::new();
    values.insert("res_model".into(), Json2::String(name.clone()));
    values.insert("res_id".into(), Json2::Number(id.into()));
    values.insert("user_id".into(), Json2::Number(uid.into()));
    let su = ctx.sudo();
    match backend.db.insert_secured(foll, &su, backend.acls, backend.rules, &values).await {
        Ok(_) => json_status(StatusCode::CREATED, "{\"ok\":true}".to_string()),
        // Already following — the unique index rejected the duplicate. Idempotent success.
        Err(DbError::Conflict(_)) => json_response("{\"ok\":true,\"already\":true}".to_string()),
        Err(e) => write_error("follow", e),
    }
}

/// `POST /api/:name/:id/unfollow` — unsubscribe a user (default the caller). Idempotent: unfollowing
/// when not a follower is a success (nothing to remove).
async fn unfollow_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
    Json(body): Json<Json2>,
) -> Response {
    let (backend, ctx, foll) = match chatter_setup(&state, &headers, &name, id, "mail.follower").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let uid = match body_user_id(&body, ctx.uid) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if let Err(r) = ensure_self_or_admin(&ctx, uid) {
        return r;
    }
    let su = ctx.sudo();
    let filter = follower_filter(&name, id, Some(uid));
    let ids = match backend.db.find_ids_secured(foll, &su, backend.acls, backend.rules, Some(&filter)).await {
        Ok(v) => v,
        Err(e) => return internal_error("unfollow", e),
    };
    for fid in ids {
        if let Err(e) = backend.db.delete_secured(foll, &su, backend.acls, backend.rules, fid).await {
            return write_error("unfollow", e);
        }
    }
    json_response("{\"ok\":true}".to_string())
}

/// Issues an access + (stored) refresh token pair for `uid` with `groups` and company `scope`
/// (active, allowed). The access token bakes in the scope so each request verifies into a
/// company-scoped Ctx with no extra DB round-trip.
async fn issue_token_pair(
    backend: &DataBackend,
    uid: i64,
    groups: Vec<String>,
    scope: (Option<i64>, Vec<i64>),
) -> Response {
    let (company, companies) = scope;
    let access = match backend.auth.issue_access(uid, groups, company, companies, ACCESS_TTL) {
        Ok(t) => t,
        Err(_) => return internal_error("token", "issue access"),
    };
    let jti = new_jti();
    if let Err(e) = backend.db.store_refresh(&jti, uid, REFRESH_TTL as i64).await {
        return internal_error("refresh-store", e);
    }
    let refresh = match backend.auth.issue_refresh(uid, &jti, REFRESH_TTL) {
        Ok(t) => t,
        Err(_) => return internal_error("token", "issue refresh"),
    };
    let body = serde_json::json!({
        "access_token": access,
        "refresh_token": refresh,
        "token_type": "Bearer",
        "expires_in": ACCESS_TTL,
    });
    json_status(StatusCode::OK, body.to_string())
}

/// A constant valid argon2 hash, verified against on the unknown-user path so login spends the
/// same argon2 time whether or not the account exists (defeats username enumeration via timing).
fn dummy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_password("meshble-timing-equalizer").expect("dummy hash"))
}

async fn login_handler(State(state): State<AppState>, Json(body): Json<Json2>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on auth routes");
    let (login, password) = match (str_field(&body, "login"), str_field(&body, "password")) {
        (Some(l), Some(p)) => (l, p),
        _ => return (StatusCode::BAD_REQUEST, "login and password required").into_response(),
    };
    let user = match backend.db.find_user(login).await {
        Ok(u) => u,
        Err(e) => return internal_error("login", e),
    };
    // Always run argon2 (against a dummy hash if the user is unknown) so timing — and the
    // 401 body — are identical for unknown-user and wrong-password (no user enumeration).
    let hash = user.as_ref().map(|u| u.password_hash.as_str()).unwrap_or_else(|| dummy_hash());
    let ok = verify_password(password, hash);
    match user {
        Some(u) if ok => {
            let scope = (u.company_id, u.company_ids);
            issue_token_pair(backend, u.id, u.groups, scope).await
        }
        _ => (StatusCode::UNAUTHORIZED, "invalid credentials").into_response(),
    }
}

async fn refresh_handler(State(state): State<AppState>, Json(body): Json<Json2>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on auth routes");
    let token = match str_field(&body, "refresh_token") {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "refresh_token required").into_response(),
    };
    let claims = match backend.auth.verify_refresh(token) {
        Ok(c) => c,
        Err(_) => return (StatusCode::UNAUTHORIZED, "invalid refresh token").into_response(),
    };
    // Atomically claim (revoke) the presented token. A concurrent replay claims zero rows and is
    // rejected → no double-spend. The token also must belong to the uid it claims.
    match backend.db.claim_refresh(&claims.jti).await {
        Ok(Some(uid)) if uid == claims.uid => {
            let groups = match backend.db.user_groups(uid).await {
                Ok(g) => g,
                Err(e) => return internal_error("groups", e),
            };
            // Re-read company scope too, so reassignments take effect on refresh (like groups).
            let scope = match backend.db.user_scope(uid).await {
                Ok(s) => s,
                Err(e) => return internal_error("scope", e),
            };
            issue_token_pair(backend, uid, groups, scope).await
        }
        Ok(_) => (StatusCode::UNAUTHORIZED, "invalid refresh token").into_response(),
        Err(e) => internal_error("refresh", e),
    }
}

async fn logout_handler(State(state): State<AppState>, Json(body): Json<Json2>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on auth routes");
    // Always 204 — never reveal whether the token was valid.
    if let Some(token) = str_field(&body, "refresh_token") {
        if let Ok(claims) = backend.auth.verify_refresh(token) {
            let _ = backend.db.revoke_refresh(&claims.jti).await;
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use meshble_core::{resolve, FieldDef, FieldKind, ModelDescriptor};
    use tower::ServiceExt;

    static M: ModelDescriptor = ModelDescriptor {
        name: "sale.order",
        table: "sale_order",
        fields: &[FieldDef {
            name: "state", label: "State",
            kind: FieldKind::Selection(&[("draft", "Draft"), ("done", "Done")]),
            required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
    };

    fn models() -> Vec<ResolvedModel> {
        vec![resolve(&M, &[]).unwrap()]
    }

    async fn fetch(uri: &str) -> (StatusCode, String) {
        let resp = router(models())
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn serves_openapi_spec() {
        let (status, body) = fetch("/openapi.json").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"openapi\""));
        assert!(body.contains("sale.order"));
    }

    #[tokio::test]
    async fn serves_model_list_and_view() {
        let (status, body) = fetch("/api/models").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("sale.order"));

        let (status, body) = fetch("/api/sale.order/view").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"model\": \"sale.order\""));

        let (status, _) = fetch("/api/nope/view").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}

#[cfg(test)]
mod query_tests {
    use super::*;
    use meshble_core::{resolve, FieldDef, ModelDescriptor};

    static M: ModelDescriptor = ModelDescriptor {
        name: "q.model",
        table: "q_model",
        fields: &[
            FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
            FieldDef { name: "amount", label: "Amount", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
            FieldDef { name: "state", label: "State", kind: FieldKind::Selection(&[("draft", "Draft"), ("done", "Done")]), required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        ],
    };
    fn model() -> ResolvedModel {
        resolve(&M, &[]).unwrap()
    }

    #[test]
    fn suffix_split() {
        assert_eq!(split_suffix("amount__gte"), ("amount", "gte"));
        assert_eq!(split_suffix("name"), ("name", "eq")); // bare field defaults to eq
    }

    #[test]
    fn activity_state_derivation() {
        // Derived purely from deadline vs today (ISO strings compare lexically).
        assert_eq!(activity_state(Some("2026-06-16"), "2026-06-17"), "overdue");
        assert_eq!(activity_state(Some("2026-06-17"), "2026-06-17"), "today");
        assert_eq!(activity_state(Some("2026-06-18"), "2026-06-17"), "planned");
        assert_eq!(activity_state(None, "2026-06-17"), "planned"); // no deadline
    }

    #[test]
    fn coerce_by_kind() {
        assert_eq!(coerce(&FieldKind::Integer, "5"), Ok(Value::Int(5)));
        assert_eq!(
            coerce(&FieldKind::Decimal { currency_field: None }, "1.5"),
            Ok(Value::Decimal("1.5".parse::<rust_decimal::Decimal>().unwrap()))
        );
        assert_eq!(coerce(&FieldKind::Bool, "true"), Ok(Value::Bool(true)));
        assert_eq!(coerce(&FieldKind::Text, "x"), Ok(Value::Str("x".to_string())));
        assert!(coerce(&FieldKind::Integer, "nope").is_err());
    }

    #[test]
    fn build_leaf_typed() {
        let m = model();
        let d = build_leaf(&m, "amount", "gte", "100").unwrap();
        assert_eq!(d, Domain::Cond(Condition { field: "amount".into(), op: Operator::Ge, value: Value::Decimal("100".parse().unwrap()) }));
        assert!(build_leaf(&m, "nope", "eq", "1").is_err(), "unknown field rejected");
        assert!(build_leaf(&m, "name", "weird", "1").is_err(), "unknown operator rejected");
    }

    #[test]
    fn parse_query_filter_order_limit() {
        let m = model();
        let mut p = HashMap::new();
        p.insert("state".to_string(), "draft".to_string());
        p.insert("amount__gte".to_string(), "10".to_string());
        p.insert("order".to_string(), "-id".to_string());
        p.insert("limit".to_string(), "5".to_string());
        let (filter, order, limit, offset) = parse_list_query(&m, &p).unwrap();
        assert!(filter.is_some());
        assert_eq!(order, vec![("id".to_string(), true)]);
        assert_eq!(limit, 5);
        assert_eq!(offset, 0);
        // The compiled filter is valid SQL against the model.
        assert!(filter.unwrap().compile(&m).is_ok());
    }

    #[test]
    fn limit_is_clamped() {
        let m = model();
        let mut p = HashMap::new();
        p.insert("limit".to_string(), "100000".to_string());
        let (_, _, limit, _) = parse_list_query(&m, &p).unwrap();
        assert_eq!(limit, MAX_LIMIT);
    }
}
