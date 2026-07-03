//! The generic module-route dispatch `GET|POST /api/x/:route` over real HTTP, with synthetic routes
//! covering the seam's whole contract: an UNAUTHENTICATED webhook receiver that verifies a signature
//! over the RAW body and elevates only after it (valid → 200, tampered → 403, and the guest context
//! cannot touch secured data before verification); a GET challenge handshake echoing PLAIN TEXT
//! (unquoted — the exact-match a provider performs); an authenticated, group-gated route (401 without
//! a bearer, 403 wrong group, 200 right group); and 405 + Allow on a method mismatch. Requires
//! DATABASE_URL.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use kigumi_auth::Authenticator;
use kigumi_core::{resolve, Acl, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, ResolvedModel};
use kigumi_db::{BoxServiceFut, Db, DbError, RouteInput, RouteMethod, RouteOutput, RouteRegistration};
use kigumi_server::router_with_data;
use serde_json::json;
use sha2::Sha256;
use tower::ServiceExt;

const SECRET: &str = "module-route-secret";
const HOOK_SECRET: &str = "hook-shared-secret";

fn bearer(groups: &str) -> String {
    let g: Vec<String> = groups.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
    let token = Authenticator::new(SECRET).issue_access(1, g, None, vec![], 3600).unwrap();
    format!("Bearer {token}")
}

/// A REAL HMAC-SHA256 (what providers actually send) — the receiver verifies it in constant time
/// via RouteInput::verify_hmac_sha256. Never a plain hash of secret+body (length-extension
/// forgeable) and never a `==` comparison (timing oracle).
fn sign(body: &[u8]) -> String {
    use hmac::Mac;
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(HOOK_SECRET.as_bytes()).unwrap();
    mac.update(body);
    let out = mac.finalize().into_bytes();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

const fn txt(name: &'static str, required: bool) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Text, required, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}
static DOC: ModelDescriptor = ModelDescriptor { name: "hook.doc", table: "hook_doc", fields: &[txt("name", true)] };
fn f_doc() -> &'static ModelDescriptor { &DOC }
kigumi_core::inventory::submit! { ModelRegistration { name: "hook.doc", module: "test", descriptor: f_doc } }

// hook.doc IS writable — by the "ops" group. The guest-denial assertion below is therefore
// meaningful: the dispatcher hands the body a group-less non-su ctx, which the ACL engine denies
// even though a grant exists.
static ACLS: &[Acl] = &[Acl { model: "hook.doc", group: "ops", read: true, write: true, create: true, delete: true }];

/// POST /api/x/test-hook — the webhook receiver: verify the signature over the RAW bytes, then (and
/// only then) elevate and record the delivery.
fn hook_post<'a>(db: &'a Db, input: RouteInput) -> BoxServiceFut<'a, Result<RouteOutput, DbError>> {
    Box::pin(async move {
        let claimed = input.header("x-test-signature").unwrap_or("");
        if !input.verify_hmac_sha256(HOOK_SECRET.as_bytes(), claimed) {
            return Err(DbError::AccessDenied { model: "route:test-hook".to_string(), operation: "signature" });
        }
        // Sanity: the guest context is default-deny — a secured write BEFORE elevation must fail.
        let doc = kigumi_core::resolve_registered("hook.doc").map_err(DbError::BadInput)?;
        let guest_attempt = db
            .insert_secured(&doc, &input.ctx, &[], &[], json!({ "name": "guest" }).as_object().unwrap())
            .await;
        assert!(guest_attempt.is_err(), "guest ctx must be denied by the ACL default-deny");
        // Verified sender → explicit elevation (the same greppable idiom as ServiceCtx::elevated).
        let id = db
            .insert_secured(&doc, &input.ctx.sudo(), &[], &[], json!({ "name": input.query_str("event") }).as_object().unwrap())
            .await?;
        Ok(RouteOutput::Json(json!({ "stored": id })))
    })
}
kigumi_core::inventory::submit! {
    RouteRegistration { name: "test-hook", method: RouteMethod::Post, auth: false, groups: &[], func: hook_post }
}

/// GET /api/x/test-hook — the provider's verification handshake: echo the challenge as PLAIN text.
/// Registered through the facade macro, so its expansion is compiled and exercised here.
async fn hook_challenge(_db: &Db, input: RouteInput) -> Result<RouteOutput, DbError> {
    Ok(RouteOutput::Text(input.query_str("challenge").to_string()))
}
kigumi::register_route!("test-hook", Get, false, &[], hook_challenge);

/// GET /api/x/ops-ping — an authenticated, group-gated bespoke endpoint.
fn ops_ping<'a>(_db: &'a Db, input: RouteInput) -> BoxServiceFut<'a, Result<RouteOutput, DbError>> {
    Box::pin(async move { Ok(RouteOutput::Json(json!({ "pong": input.ctx.uid }))) })
}
kigumi_core::inventory::submit! {
    RouteRegistration { name: "ops-ping", method: RouteMethod::Get, auth: true, groups: &["ops"], func: ops_ping }
}

fn m(d: &'static ModelDescriptor) -> ResolvedModel { resolve(d, &[]).unwrap() }

async fn send(app: Router, method: &str, uri: &str, auth: Option<&str>, sig: Option<&str>, body: &str) -> (StatusCode, String, Option<String>) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(g) = auth {
        b = b.header("authorization", bearer(g));
    }
    if let Some(s) = sig {
        b = b.header("x-test-signature", s);
    }
    let resp = app.oneshot(b.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = resp.status();
    let allow = resp.headers().get("allow").and_then(|v| v.to_str().ok()).map(String::from);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap(), allow)
}

#[tokio::test]
async fn module_routes_dispatch_gate_and_verify() {
    // hook.doc is REGISTERED — the kit's reset already created its table.
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let seed = &t.db;
    let doc = m(&DOC);

    let blobs = std::sync::Arc::new(kigumi_server::FsBlobStore::new(std::env::temp_dir().join("kigumi_test_blobs")));
    let app = router_with_data(vec![m(&DOC)], t.db.clone(), ACLS, &[], SECRET, blobs);

    // Unauthenticated receiver, VALID signature over the exact raw bytes → 200 and the write landed.
    let payload = r#"{"event":"payment","amount":42}"#;
    let (st, body, _) = send(app.clone(), "POST", "/api/x/test-hook?event=payment", None, Some(&sign(payload.as_bytes())), payload).await;
    assert_eq!(st, StatusCode::OK, "valid signature accepted: {body}");
    let stored = serde_json::from_str::<serde_json::Value>(&body).unwrap()["stored"].as_i64().unwrap();
    let su = kigumi_test::su();
    let row = seed.find_one_secured(&doc, &su, &[], &[], stored).await.unwrap().unwrap();
    assert_eq!(row["name"], "payment", "the verified delivery was recorded elevated");

    // Tampered body (signature no longer matches the raw bytes) → 403, nothing written.
    let (st, _, _) = send(app.clone(), "POST", "/api/x/test-hook", None, Some(&sign(payload.as_bytes())), r#"{"event":"tampered"}"#).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "bad signature rejected");

    // The GET challenge handshake echoes PLAIN text — exactly the query value, unquoted.
    let (st, body, _) = send(app.clone(), "GET", "/api/x/test-hook?challenge=abc123", None, None, "").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body, "abc123", "challenge echoed as unquoted text");

    // Authenticated + group-gated route: no bearer → 401; wrong group → 403; right group → 200.
    let (st, _, _) = send(app.clone(), "GET", "/api/x/ops-ping", None, None, "").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _, _) = send(app.clone(), "GET", "/api/x/ops-ping", Some("sales"), None, "").await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, body, _) = send(app.clone(), "GET", "/api/x/ops-ping", Some("ops"), None, "").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["pong"], 1);

    // Method mismatch on an existing name → 405 with the Allow header; unknown name → 404.
    let (st, _, allow) = send(app.clone(), "POST", "/api/x/ops-ping", Some("ops"), None, "").await;
    assert_eq!(st, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(allow.as_deref(), Some("GET"));
    let (st, _, _) = send(app.clone(), "GET", "/api/x/nope", None, None, "").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}
