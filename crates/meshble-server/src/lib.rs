//! Headless HTTP layer (axum). Serves the integration surface from a model set:
//! the OpenAPI spec, the model list, per-model UI contracts, and — when a database backend is
//! provided — secured data endpoints that enforce the ACL + record-rule engine.
//!
//! The server is agnostic of any module: a host wires its catalog in with
//! `meshble_core::resolve_all_registered()` and its security policy, then calls [`router`] or
//! [`router_with_data`]. The core stays headless; this crate is optional.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::Value as Json2;
use meshble_auth::Authenticator;
use meshble_core::{Acl, Ctx, RecordRule, ResolvedModel};
use meshble_db::{Db, DbError};
use meshble_schema::{openapi, to_ui_contract};

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
        .route("/api/:name", get(list_handler).post(create_handler))
        .route("/api/:name/:id", axum::routing::patch(update_handler).delete(delete_handler))
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

async fn view_handler(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    match state.models.iter().find(|m| m.name == name) {
        Some(m) => match to_ui_contract(m, &[]) {
            Ok(json) => json_response(json),
            Err(e) => internal_error("view", e),
        },
        None => (StatusCode::NOT_FOUND, format!("unknown model: {name}")).into_response(),
    }
}

async fn list_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
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
    match backend.db.find_secured(model, &ctx, backend.acls, backend.rules, None).await {
        Ok(rows) => json_response(serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string())),
        Err(DbError::AccessDenied { .. }) => {
            (StatusCode::FORBIDDEN, "access denied").into_response()
        }
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
