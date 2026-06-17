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
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::Value as Json2;
use meshble_auth::{hash_password, new_jti, verify_password, Authenticator};
use meshble_core::{
    Acl, Condition, Ctx, Domain, FieldKind, Operator, RecordRule, ResolvedModel, Value,
};
use meshble_db::{Db, DbError};
use meshble_schema::{openapi, to_ui_contract};

/// Access tokens are short-lived; refresh tokens long-lived (and revocable/rotated server-side).
const ACCESS_TTL: u64 = 900; // 15 minutes
const REFRESH_TTL: u64 = 2_592_000; // 30 days

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
pub fn router_with_data(
    models: Vec<ResolvedModel>,
    db: Db,
    acls: &'static [Acl],
    rules: &'static [RecordRule],
    auth_secret: impl Into<String>,
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
        .with_state(AppState {
            models: Arc::new(models),
            data: Some(DataBackend {
                db: Arc::new(db),
                acls,
                rules,
                auth: Arc::new(Authenticator::new(auth_secret)),
            }),
        })
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
    let body = serde_json::json!({ "uid": ctx.uid, "groups": ctx.groups });
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
        FieldKind::Integer | FieldKind::Many2one { .. } => {
            Value::Int(raw.parse().map_err(|_| format!("'{raw}' is not an integer"))?)
        }
        FieldKind::Decimal { .. } => {
            let f: f64 = raw.parse().map_err(|_| format!("'{raw}' is not a number"))?;
            if !f.is_finite() {
                return Err(format!("'{raw}' is not a finite number"));
            }
            Value::Float(f)
        }
        FieldKind::Bool => Value::Bool(matches!(raw, "true" | "1" | "t" | "yes")),
        FieldKind::Text | FieldKind::Selection(_) => Value::Str(raw.to_string()),
        FieldKind::One2many { .. } => return Err("cannot filter on a One2many field".to_string()),
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

/// Issues an access + (stored) refresh token pair for `uid` with `groups`.
async fn issue_token_pair(backend: &DataBackend, uid: i64, groups: Vec<String>) -> Response {
    let access = match backend.auth.issue_access(uid, groups, ACCESS_TTL) {
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
        Some(u) if ok => issue_token_pair(backend, u.id, u.groups).await,
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
            issue_token_pair(backend, uid, groups).await
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
            required: true, stored: true, compute: None, depends: &[],
        }],
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
            FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[] },
            FieldDef { name: "amount", label: "Amount", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[] },
            FieldDef { name: "state", label: "State", kind: FieldKind::Selection(&[("draft", "Draft"), ("done", "Done")]), required: false, stored: true, compute: None, depends: &[] },
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
    fn coerce_by_kind() {
        assert_eq!(coerce(&FieldKind::Integer, "5"), Ok(Value::Int(5)));
        assert_eq!(coerce(&FieldKind::Decimal { currency_field: None }, "1.5"), Ok(Value::Float(1.5)));
        assert_eq!(coerce(&FieldKind::Bool, "true"), Ok(Value::Bool(true)));
        assert_eq!(coerce(&FieldKind::Text, "x"), Ok(Value::Str("x".to_string())));
        assert!(coerce(&FieldKind::Integer, "nope").is_err());
    }

    #[test]
    fn build_leaf_typed() {
        let m = model();
        let d = build_leaf(&m, "amount", "gte", "100").unwrap();
        assert_eq!(d, Domain::Cond(Condition { field: "amount".into(), op: Operator::Ge, value: Value::Float(100.0) }));
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
