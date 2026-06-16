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

/// Full router: metadata routes plus `GET /api/{model}` returning rows visible to the request's
/// identity, enforcing ACL + record rules through [`Db::find_secured`].
pub fn router_with_data(
    models: Vec<ResolvedModel>,
    db: Db,
    acls: &'static [Acl],
    rules: &'static [RecordRule],
) -> Router {
    base_router()
        .route("/api/:name", get(list_handler))
        .with_state(AppState {
            models: Arc::new(models),
            data: Some(DataBackend { db: Arc::new(db), acls, rules }),
        })
}

/// DEV-ONLY identity: trusts client-supplied `X-User-Id` / `X-User-Groups` headers. This is NOT
/// authentication — any client can claim any group. A real deployment MUST replace this with an
/// authenticated session/token → `Ctx` mapping before exposing data endpoints.
fn dev_identity_from_headers(headers: &HeaderMap) -> Ctx {
    let uid = headers
        .get("x-user-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let groups = headers
        .get("x-user-groups")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').map(|g| g.trim().to_string()).filter(|g| !g.is_empty()).collect())
        .unwrap_or_default();
    Ctx::new(uid, groups)
}

fn json_response(body: String) -> Response {
    ([("content-type", "application/json")], body).into_response()
}

async fn openapi_handler(State(state): State<AppState>) -> Response {
    let refs: Vec<&ResolvedModel> = state.models.iter().collect();
    json_response(openapi(&refs))
}

async fn models_handler(State(state): State<AppState>) -> Json<Vec<String>> {
    Json(state.models.iter().map(|m| m.name.to_string()).collect())
}

async fn view_handler(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    match state.models.iter().find(|m| m.name == name) {
        Some(m) => match to_ui_contract(m, &[]) {
            Ok(json) => json_response(json),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:?}")).into_response(),
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
    let ctx = dev_identity_from_headers(&headers);
    match backend.db.find_secured(model, &ctx, backend.acls, backend.rules, None).await {
        Ok(rows) => json_response(serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string())),
        Err(DbError::AccessDenied { .. }) => {
            (StatusCode::FORBIDDEN, "access denied").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:?}")).into_response(),
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
