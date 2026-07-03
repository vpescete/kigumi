//! API-key lifecycle over HTTP: a logged-in user mints a key, the key authenticates data routes
//! as that user, its scopes NARROW (never widen) access, an over-broad scope is refused at mint,
//! and revocation is immediate. Requires DATABASE_URL.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use kigumi_auth::hash_password;
use kigumi_core::{resolve, Acl, FieldDef, FieldKind, ModelDescriptor, RecordRule, ResolvedModel};
use kigumi_server::router_with_data;
use tower::ServiceExt;

const SECRET: &str = "apikey-test-secret";

static MODEL: ModelDescriptor = ModelDescriptor {
    name: "widget",
    table: "apikey_widget_test",
    fields: &[FieldDef {
        name: "name", label: "Name", kind: FieldKind::Text,
        required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
};
// Only the "reader" group may read; "writer" may create. A key scoped to [reader] must lose create.
static ACLS: &[Acl] = &[
    Acl { model: "widget", group: "reader", read: true, write: false, create: false, delete: false },
    Acl { model: "widget", group: "writer", read: true, write: true, create: true, delete: false },
];
static RULES: &[RecordRule] = &[];

fn model() -> ResolvedModel {
    resolve(&MODEL, &[]).unwrap()
}

async fn send(app: Router, method: &str, uri: &str, bearer: Option<&str>, json: Option<&str>) -> (StatusCode, serde_json::Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = bearer {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    let body = match json {
        Some(j) => { b = b.header("content-type", "application/json"); Body::from(j.to_string()) }
        None => Body::empty(),
    };
    let resp = app.oneshot(b.body(body).unwrap()).await.unwrap();
    let st = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

#[tokio::test]
async fn api_key_lifecycle() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let setup = &t.db;
    let m = model();
    setup.drop_table(&m).await.unwrap();
    setup.create_table(&m).await.unwrap();
    sqlx::query("INSERT INTO apikey_widget_test (name) VALUES ('seed')").execute(setup.pool()).await.unwrap();
    // A writer: holds both groups.
    setup.upsert_user("dev", &hash_password("pw").unwrap(), &["reader", "writer"]).await.unwrap();

    let blobs = std::sync::Arc::new(kigumi_server::FsBlobStore::new(std::env::temp_dir().join("kigumi_apikey_blobs")));
    let app = router_with_data(vec![model()], t.db.clone(), ACLS, RULES, SECRET, blobs);

    // Log in to get an access token for managing keys.
    let (s, tok) = send(app.clone(), "POST", "/auth/login", None, Some(r#"{"login":"dev","password":"pw"}"#)).await;
    assert_eq!(s, StatusCode::OK);
    let access = tok["access_token"].as_str().unwrap().to_string();

    // Managing keys requires auth.
    assert_eq!(send(app.clone(), "GET", "/auth/keys", None, None).await.0, StatusCode::UNAUTHORIZED);

    // An over-broad scope (a group the user does NOT hold) is refused at mint.
    let (s, _) = send(app.clone(), "POST", "/auth/keys", Some(&access),
        Some(r#"{"name":"bad","scopes":"admin"}"#)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "a key cannot name a group its user lacks");

    // Mint a FULL key (all the user's groups): it can read AND create.
    let (s, full) = send(app.clone(), "POST", "/auth/keys", Some(&access), Some(r#"{"name":"ci"}"#)).await;
    assert_eq!(s, StatusCode::CREATED);
    let full_key = full["key"].as_str().unwrap().to_string();
    assert!(full_key.starts_with("kg_"), "key carries the scheme prefix");

    assert_eq!(send(app.clone(), "GET", "/api/widget", Some(&full_key), None).await.0, StatusCode::OK);
    let (s, _) = send(app.clone(), "POST", "/api/widget", Some(&full_key), Some(r#"{"name":"made-by-key"}"#)).await;
    assert_eq!(s, StatusCode::CREATED, "the full key inherits create");

    // Mint a NARROWED key scoped to [reader]: it reads but can no longer create.
    let (s, ro) = send(app.clone(), "POST", "/auth/keys", Some(&access), Some(r#"{"name":"readonly","scopes":"reader"}"#)).await;
    assert_eq!(s, StatusCode::CREATED);
    let ro_key = ro["key"].as_str().unwrap().to_string();
    let ro_id = ro["id"].as_i64().unwrap();

    assert_eq!(send(app.clone(), "GET", "/api/widget", Some(&ro_key), None).await.0, StatusCode::OK, "read allowed");
    let (s, _) = send(app.clone(), "POST", "/api/widget", Some(&ro_key), Some(r#"{"name":"nope"}"#)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "the scope NARROWED away create");

    // The key appears in the owner's list (without the secret).
    let (s, list) = send(app.clone(), "GET", "/auth/keys", Some(&access), None).await;
    assert_eq!(s, StatusCode::OK);
    let rows = list["data"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "two live keys");
    assert!(rows.iter().all(|r| r.get("key").is_none() && r.get("hash").is_none()), "no secret leaks");

    // Revoke the read-only key → it stops authenticating immediately.
    assert_eq!(send(app.clone(), "DELETE", &format!("/auth/keys/{ro_id}"), Some(&access), None).await.0, StatusCode::OK);
    assert_eq!(send(app.clone(), "GET", "/api/widget", Some(&ro_key), None).await.0, StatusCode::UNAUTHORIZED, "revoked key is dead");

    // A garbage key with the scheme is a clean 401 (no 500).
    assert_eq!(send(app.clone(), "GET", "/api/widget", Some("kg_deadbeef_bogus"), None).await.0, StatusCode::UNAUTHORIZED);

    setup.drop_table(&m).await.unwrap();
}
