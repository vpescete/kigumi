//! End-to-end auth lifecycle: login → access token on data routes → refresh (with rotation) →
//! logout/revocation. Requires `DATABASE_URL`; skipped otherwise.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use kigumi_auth::hash_password;
use kigumi_core::{
    resolve, Acl, FieldDef, FieldKind, ModelDescriptor, RecordRule, ResolvedModel,
};
use kigumi_server::router_with_data;
use tower::ServiceExt;

const SECRET: &str = "auth-test-secret";

static MODEL: ModelDescriptor = ModelDescriptor {
    name: "thing",
    table: "auth_thing_test",
    fields: &[FieldDef {
        name: "name", label: "Name", kind: FieldKind::Text,
        required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
};
static ACLS: &[Acl] = &[Acl {
    model: "thing", group: "u", read: true, write: false, create: false, delete: false,
}];
static RULES: &[RecordRule] = &[];

fn model() -> ResolvedModel {
    resolve(&MODEL, &[]).unwrap()
}

async fn post(app: Router, uri: &str, json: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(json.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let st = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

async fn get_bearer(app: Router, uri: &str, bearer: &str) -> StatusCode {
    app.oneshot(
        Request::builder()
            .uri(uri)
            .header("authorization", format!("Bearer {bearer}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

#[tokio::test]
async fn auth_lifecycle() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let setup = &t.db;
    let m = model();
    // The model is ad-hoc (not registered) — the kit does not know its table.
    setup.drop_table(&m).await.unwrap();
    setup.create_table(&m).await.unwrap();
    sqlx::query("INSERT INTO auth_thing_test (name) VALUES ('row')").execute(setup.pool()).await.unwrap();
    setup.upsert_user("tester", &hash_password("pw").unwrap(), &["u"]).await.unwrap();

    let blobs = std::sync::Arc::new(kigumi_server::FsBlobStore::new(std::env::temp_dir().join("kigumi_test_blobs")));
    let app = router_with_data(vec![model()], t.db.clone(), ACLS, RULES, SECRET, blobs);

    // Wrong password and unknown user both → 401 (no user enumeration).
    assert_eq!(post(app.clone(), "/auth/login", r#"{"login":"tester","password":"nope"}"#).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(post(app.clone(), "/auth/login", r#"{"login":"ghost","password":"pw"}"#).await.0, StatusCode::UNAUTHORIZED);

    // Login → token pair.
    let (s, tok) = post(app.clone(), "/auth/login", r#"{"login":"tester","password":"pw"}"#).await;
    assert_eq!(s, StatusCode::OK);
    let access = tok["access_token"].as_str().unwrap().to_string();
    let refresh = tok["refresh_token"].as_str().unwrap().to_string();

    // Access token works on a data route; the refresh token does NOT (kind separation).
    assert_eq!(get_bearer(app.clone(), "/api/thing", &access).await, StatusCode::OK);
    assert_eq!(get_bearer(app.clone(), "/api/thing", &refresh).await, StatusCode::UNAUTHORIZED);

    // Refresh → a new pair; the new access works.
    let (s, tok2) = post(app.clone(), "/auth/refresh", &format!(r#"{{"refresh_token":"{refresh}"}}"#)).await;
    assert_eq!(s, StatusCode::OK);
    let access2 = tok2["access_token"].as_str().unwrap().to_string();
    let refresh2 = tok2["refresh_token"].as_str().unwrap().to_string();
    assert_eq!(get_bearer(app.clone(), "/api/thing", &access2).await, StatusCode::OK);

    // Rotation: the OLD refresh token is now revoked.
    assert_eq!(post(app.clone(), "/auth/refresh", &format!(r#"{{"refresh_token":"{refresh}"}}"#)).await.0, StatusCode::UNAUTHORIZED);

    // Logout revokes the current refresh token.
    assert_eq!(post(app.clone(), "/auth/logout", &format!(r#"{{"refresh_token":"{refresh2}"}}"#)).await.0, StatusCode::NO_CONTENT);
    assert_eq!(post(app.clone(), "/auth/refresh", &format!(r#"{{"refresh_token":"{refresh2}"}}"#)).await.0, StatusCode::UNAUTHORIZED);

    setup.drop_table(&m).await.unwrap();
}
