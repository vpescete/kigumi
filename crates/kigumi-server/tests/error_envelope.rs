//! The structured error envelope over real HTTP: every DbError-mapped failure returns
//! `{"error": {"code", "message", "fields"?}}` with the SAME statuses as the plain-text era.
//! Field-level failures (an @api.constrains violation, a not-null rejection) carry the offending
//! field(s) so a form can render them inline; access denials and conflicts carry code+message only.
//! Requires DATABASE_URL.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use kigumi_auth::Authenticator;
use kigumi_core::{resolve, Acl, ComputeInput, ConstraintRegistration, Ctx, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, ResolvedModel};
use kigumi_db::Db;
use kigumi_server::router_with_data;
use tower::ServiceExt;

const SECRET: &str = "error-envelope-secret";

fn bearer(groups: &str) -> String {
    let g: Vec<String> = groups.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
    let token = Authenticator::new(SECRET).issue_access(1, g, None, vec![], 3600).unwrap();
    format!("Bearer {token}")
}

const fn txt(name: &'static str, required: bool) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Text, required, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}
const fn int(name: &'static str) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Integer, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}

static DOC: ModelDescriptor = ModelDescriptor { name: "env.doc", table: "env_doc", fields: &[txt("name", true), int("qty")] };
fn f_doc() -> &'static ModelDescriptor { &DOC }
kigumi_core::inventory::submit! { ModelRegistration { name: "env.doc", module: "test", descriptor: f_doc } }

// An @api.constrains rule on qty: negative quantities are rejected. Its declared trigger field is
// what the envelope must surface.
fn qty_non_negative(i: &ComputeInput) -> Result<(), String> {
    if i.int("qty") < 0 {
        return Err("quantity cannot be negative".to_string());
    }
    Ok(())
}
kigumi_core::inventory::submit! { ConstraintRegistration { model: "env.doc", fields: &["qty"], func: qty_non_negative } }

static ACLS: &[Acl] = &[Acl { model: "env.doc", group: "u", read: true, write: true, create: true, delete: true }];

fn m(d: &'static ModelDescriptor) -> ResolvedModel { resolve(d, &[]).unwrap() }

async fn post(app: Router, uri: &str, groups: Option<&str>, body: &str) -> (StatusCode, String) {
    let mut b = Request::builder().method("POST").uri(uri).header("content-type", "application/json");
    if let Some(g) = groups {
        b = b.header("authorization", bearer(g));
    }
    let resp = app.oneshot(b.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn envelope(body: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(body).unwrap_or_else(|_| panic!("not a JSON envelope: {body}"))["error"].clone()
}

#[tokio::test]
async fn errors_carry_the_structured_envelope() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let seed = Db::connect(&url).await.unwrap();
    let doc = m(&DOC);
    seed.drop_table(&doc).await.unwrap();
    seed.create_table(&doc).await.unwrap();

    let app_db = Db::connect(&url).await.unwrap();
    let blobs = std::sync::Arc::new(kigumi_server::FsBlobStore::new(std::env::temp_dir().join("kigumi_test_blobs")));
    let app = router_with_data(vec![m(&DOC)], app_db, ACLS, &[], SECRET, blobs);

    // A constraint violation → 400, code "invalid", the rule's message, and its declared field.
    let (st, body) = post(app.clone(), "/api/env.doc", Some("u"), r#"{"name":"x","qty":-3}"#).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "constraint violation is a 400: {body}");
    let err = envelope(&body);
    assert_eq!(err["code"], "invalid", "unexpected error: {err}");
    assert_eq!(err["message"], "quantity cannot be negative");
    assert_eq!(err["fields"]["qty"][0], "quantity cannot be negative", "the rule's trigger field is surfaced");

    // A missing required field → 400 with the exact field named (the Rust write boundary catches it
    // before SQL; the 23502 downcast in From<sqlx::Error> is the raw-SQL backstop for the same shape).
    let (st, body) = post(app.clone(), "/api/env.doc", Some("u"), r#"{"qty":1}"#).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "missing required is a 400: {body}");
    let err = envelope(&body);
    assert_eq!(err["code"], "invalid", "unexpected error: {err}");
    assert_eq!(err["fields"]["name"][0], "required", "the missing field is named");

    // An ACL denial → 403 with code access-denied (no fields).
    let (st, body) = post(app.clone(), "/api/env.doc", Some("other"), r#"{"name":"x"}"#).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let err = envelope(&body);
    assert_eq!(err["code"], "access-denied");
    assert!(err.get("fields").is_none(), "non-field errors carry no fields object");

    // The happy path is untouched: a valid create still succeeds.
    let (st, _) = post(app.clone(), "/api/env.doc", Some("u"), r#"{"name":"ok","qty":2}"#).await;
    assert_eq!(st, StatusCode::CREATED);

    seed.drop_table(&doc).await.unwrap();
}
