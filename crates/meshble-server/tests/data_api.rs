//! End-to-end test of the secured data endpoint: HTTP request → JWT auth → Db::find_secured
//! (ACL + record rules) → JSON. Requires `DATABASE_URL`; skipped otherwise.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use meshble_auth::Authenticator;
use meshble_core::{
    resolve, Acl, Domain, FieldDef, FieldKind, ModelDescriptor, Operation, RecordRule,
    ResolvedModel,
};
use meshble_db::Db;
use meshble_server::router_with_data;
use tower::ServiceExt;

const SECRET: &str = "test-secret-change-me";

/// Mints a `Bearer` header for a JWT carrying the given comma-separated groups.
fn bearer(groups: &str) -> String {
    let g: Vec<String> = groups.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
    let token = Authenticator::new(SECRET).issue_access(1, g, 3600).unwrap();
    format!("Bearer {token}")
}

static MODEL: ModelDescriptor = ModelDescriptor {
    name: "widget",
    table: "widget_api_test",
    fields: &[
        FieldDef {
            name: "name", label: "Name", kind: FieldKind::Text,
            required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef {
            name: "active", label: "Active", kind: FieldKind::Bool,
            required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};

fn active_only() -> Domain {
    Domain::field("active").eq(true)
}

static ACLS: &[Acl] = &[Acl {
    model: "widget", group: "u", read: true, write: false, create: false, delete: false,
}];
static RULES: &[RecordRule] = &[RecordRule {
    model: "widget", groups: &["u"], ops: &[Operation::Read], domain: active_only,
}];

fn model() -> ResolvedModel {
    resolve(&MODEL, &[]).unwrap()
}

async fn get(app: Router, uri: &str, groups: Option<&str>) -> (StatusCode, String) {
    let mut req = Request::builder().uri(uri);
    if let Some(g) = groups {
        req = req.header("authorization", bearer(g));
    }
    let resp = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn secured_data_endpoint_enforces_rules() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let seed = Db::connect(&url).await.unwrap();
    let m = model();
    seed.drop_table(&m).await.unwrap();
    seed.create_table(&m).await.unwrap();
    for (n, a) in [("alpha", true), ("beta", true), ("gamma", false)] {
        sqlx::query("INSERT INTO widget_api_test (name, active) VALUES ($1, $2)")
            .bind(n)
            .bind(a)
            .execute(seed.pool())
            .await
            .unwrap();
    }

    let app_db = Db::connect(&url).await.unwrap();
    let app = router_with_data(vec![model()], app_db, ACLS, RULES, SECRET);

    // Group "u": the record rule restricts to active rows → alpha, beta only.
    let (status, body) = get(app.clone(), "/api/widget", Some("u")).await;
    assert_eq!(status, StatusCode::OK);
    // The list endpoint returns a paginated envelope { data, total, limit, offset }.
    let env: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(env["data"].as_array().unwrap().len(), 2);
    assert_eq!(env["total"].as_i64().unwrap(), 2, "total under the record rule");
    assert!(body.contains("alpha") && body.contains("beta") && !body.contains("gamma"));

    // Authenticated but no granting group → ACL denies → 403.
    let (status, _) = get(app.clone(), "/api/widget", Some("other")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // No token → unauthenticated → 401.
    let (status, _) = get(app.clone(), "/api/widget", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    seed.drop_table(&m).await.unwrap();
}

static WRITE_MODEL: ModelDescriptor = ModelDescriptor {
    name: "widget",
    table: "widget_write_test",
    fields: &[
        FieldDef {
            name: "name", label: "Name", kind: FieldKind::Text,
            required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef {
            name: "active", label: "Active", kind: FieldKind::Bool,
            required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};

static WRITE_ACLS: &[Acl] = &[Acl {
    model: "widget", group: "u", read: true, write: true, create: true, delete: true,
}];
static WRITE_RULES: &[RecordRule] = &[
    RecordRule { model: "widget", groups: &["u"], ops: &[Operation::Write], domain: active_only },
    RecordRule { model: "widget", groups: &["u"], ops: &[Operation::Delete], domain: active_only },
];

async fn req(app: Router, method: &str, uri: &str, groups: Option<&str>, body: Option<&str>) -> (StatusCode, String) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(g) = groups {
        b = b.header("authorization", bearer(g));
    }
    let body = match body {
        Some(s) => {
            b = b.header("content-type", "application/json");
            Body::from(s.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.oneshot(b.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn id_of(body: &str) -> i64 {
    serde_json::from_str::<serde_json::Value>(body).unwrap()["id"].as_i64().unwrap()
}

#[tokio::test]
async fn write_path_enforces_acl_and_rules() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let seed = Db::connect(&url).await.unwrap();
    let m = resolve(&WRITE_MODEL, &[]).unwrap();
    seed.drop_table(&m).await.unwrap();
    seed.create_table(&m).await.unwrap();

    let app_db = Db::connect(&url).await.unwrap();
    let app = router_with_data(vec![resolve(&WRITE_MODEL, &[]).unwrap()], app_db, WRITE_ACLS, WRITE_RULES, SECRET);

    // Create (group "u" has Create) → 201.
    let (s, body) = req(app.clone(), "POST", "/api/widget", Some("u"), Some(r#"{"name":"keep","active":true}"#)).await;
    assert_eq!(s, StatusCode::CREATED);
    let id_active = id_of(&body);
    let (s, body) = req(app.clone(), "POST", "/api/widget", Some("u"), Some(r#"{"name":"gone","active":false}"#)).await;
    assert_eq!(s, StatusCode::CREATED);
    let id_inactive = id_of(&body);

    // Authenticated but no granting group → 403.
    let (s, _) = req(app.clone(), "POST", "/api/widget", Some("other"), Some(r#"{"name":"x","active":true}"#)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // No token → 401.
    let (s, _) = req(app.clone(), "POST", "/api/widget", None, Some(r#"{"name":"x","active":true}"#)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Unknown field → 400 (input validation at the boundary).
    let (s, _) = req(app.clone(), "POST", "/api/widget", Some("u"), Some(r#"{"nope":1}"#)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Update the active row (write rule active=true matches) → 200 updated 1.
    let (s, body) = req(app.clone(), "PATCH", &format!("/api/widget/{id_active}"), Some("u"), Some(r#"{"name":"kept"}"#)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("\"updated\": 1"));

    // Update the inactive row → 404 (write rule excludes it; no rows match).
    let (s, _) = req(app.clone(), "PATCH", &format!("/api/widget/{id_inactive}"), Some("u"), Some(r#"{"name":"y"}"#)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Delete the inactive row → 404 (delete rule excludes it).
    let (s, _) = req(app.clone(), "DELETE", &format!("/api/widget/{id_inactive}"), Some("u"), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Delete the active row → 200 deleted 1.
    let (s, body) = req(app.clone(), "DELETE", &format!("/api/widget/{id_active}"), Some("u"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("\"deleted\": 1"));

    seed.drop_table(&m).await.unwrap();
}
