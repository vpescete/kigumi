//! Headless HTTP layer. Builds an axum [`Router`] from a set of resolved models and serves the
//! integration surface: the OpenAPI spec, the model list, and per-model UI contracts.
//!
//! The server is agnostic of any specific module: a host wires its catalog in with
//! `meshble_core::resolve_all_registered()` and passes the result to [`router`]. The core stays
//! headless; this crate is optional.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use meshble_core::ResolvedModel;
use meshble_schema::{openapi, to_ui_contract};

type Models = Arc<Vec<ResolvedModel>>;

/// Builds the router serving `models`.
pub fn router(models: Vec<ResolvedModel>) -> Router {
    let state: Models = Arc::new(models);
    Router::new()
        .route("/openapi.json", get(openapi_handler))
        .route("/api/models", get(models_handler))
        .route("/api/:name/view", get(view_handler))
        .with_state(state)
}

fn json_response(body: String) -> Response {
    ([("content-type", "application/json")], body).into_response()
}

async fn openapi_handler(State(models): State<Models>) -> Response {
    let refs: Vec<&ResolvedModel> = models.iter().collect();
    json_response(openapi(&refs))
}

async fn models_handler(State(models): State<Models>) -> Json<Vec<String>> {
    Json(models.iter().map(|m| m.name.to_string()).collect())
}

async fn view_handler(State(models): State<Models>, Path(name): Path<String>) -> Response {
    match models.iter().find(|m| m.name == name) {
        Some(m) => match to_ui_contract(m, &[]) {
            Ok(json) => json_response(json),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:?}")).into_response(),
        },
        None => (StatusCode::NOT_FOUND, format!("unknown model: {name}")).into_response(),
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
