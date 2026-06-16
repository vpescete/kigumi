//! End-to-end test of the secured data endpoint: HTTP request → dev identity → Db::find_secured
//! (ACL + record rules) → JSON. Requires `DATABASE_URL`; skipped otherwise.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use meshble_core::{
    resolve, Acl, Domain, FieldDef, FieldKind, ModelDescriptor, Operation, RecordRule,
    ResolvedModel,
};
use meshble_db::Db;
use meshble_server::router_with_data;
use tower::ServiceExt;

static MODEL: ModelDescriptor = ModelDescriptor {
    name: "widget",
    table: "widget_api_test",
    fields: &[
        FieldDef {
            name: "name", label: "Name", kind: FieldKind::Text,
            required: true, stored: true, compute: None, depends: &[],
        },
        FieldDef {
            name: "active", label: "Active", kind: FieldKind::Bool,
            required: false, stored: true, compute: None, depends: &[],
        },
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
        req = req.header("x-user-id", "1").header("x-user-groups", g);
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
    let app = router_with_data(vec![model()], app_db, ACLS, RULES);

    // Group "u": the record rule restricts to active rows → alpha, beta only.
    let (status, body) = get(app.clone(), "/api/widget", Some("u")).await;
    assert_eq!(status, StatusCode::OK);
    let rows: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);
    assert!(body.contains("alpha") && body.contains("beta") && !body.contains("gamma"));

    // No granting group → ACL denies → 403.
    let (status, _) = get(app.clone(), "/api/widget", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    seed.drop_table(&m).await.unwrap();
}
