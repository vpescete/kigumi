//! The generic service route `POST /api/:name/:id/service/:service` over real HTTP: the framework
//! dispatcher gates a module-owned service exactly like an action (ACL Write + record visibility) before
//! the body runs, with no model-name literal in the router. A synthetic model + synthetic service pin the
//! wiring end-to-end, decoupled from any ERP module: no token → 401, a caller without Write → 403, a caller
//! with Write → 200 with the body's JSON, an unknown service name → 400, an invisible record → 400.
//! Requires DATABASE_URL. (The ERP services that ride this route, e.g. generate_variants, are behavior-
//! tested in their owning module's tests.)

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use kigumi_auth::Authenticator;
use kigumi_core::{resolve, Acl, Ctx, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, ResolvedModel};
use kigumi_db::{BoxServiceFut, Db, DbError, ServiceCtx, ServiceInput, ServiceOutput, ServiceRegistration};
use kigumi_server::router_with_data;
use serde_json::json;
use tower::ServiceExt;

const SECRET: &str = "service-api-secret";

fn bearer(groups: &str) -> String {
    let g: Vec<String> = groups.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
    let token = Authenticator::new(SECRET).issue_access(1, g, None, vec![], 3600).unwrap();
    format!("Bearer {token}")
}

const fn txt(name: &'static str, required: bool) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Text, required, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}

static DOC: ModelDescriptor = ModelDescriptor { name: "svc.doc", table: "svc_doc", fields: &[txt("name", true)] };
fn f_doc() -> &'static ModelDescriptor { &DOC }
kigumi_core::inventory::submit! { ModelRegistration { name: "svc.doc", module: "test", descriptor: f_doc } }

// A trivial mutating service: past the gate, echo the record id. The dispatcher's gate is what this test
// exercises — the body itself is a no-op that proves it was reached only when authorized.
fn touch<'c, 'a, 't>(_cx: &'c mut ServiceCtx<'a, 't>, inp: ServiceInput) -> BoxServiceFut<'c, Result<ServiceOutput, DbError>> {
    Box::pin(async move { Ok(ServiceOutput::json(json!({ "ok": inp.record_id }))) })
}
kigumi_core::inventory::submit! {
    ServiceRegistration { model: "svc.doc", name: "touch", func: touch, write_gate: true, groups: &[] }
}

// A manager may write the doc; a junior may only read. The service gates on Write.
static ACLS: &[Acl] = &[
    Acl { model: "svc.doc", group: "mgr", read: true, write: true, create: true, delete: true },
    Acl { model: "svc.doc", group: "usr", read: true, write: false, create: false, delete: false },
];

fn m(d: &'static ModelDescriptor) -> ResolvedModel { resolve(d, &[]).unwrap() }

async fn post(app: Router, uri: &str, groups: Option<&str>) -> (StatusCode, String) {
    let mut b = Request::builder().method("POST").uri(uri);
    if let Some(g) = groups {
        b = b.header("authorization", bearer(g));
    }
    let resp = app.oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn generic_service_route_authorization() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let seed = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();
    let doc = m(&DOC);

    seed.drop_table(&doc).await.unwrap();
    seed.create_table(&doc).await.unwrap();
    let t = seed.insert_secured(&doc, &su, &[], &[], json!({ "name": "Doc" }).as_object().unwrap()).await.unwrap();

    let app_db = Db::connect(&url).await.unwrap();
    let blobs = std::sync::Arc::new(kigumi_server::FsBlobStore::new(std::env::temp_dir().join("kigumi_test_blobs")));
    let app = router_with_data(vec![m(&DOC)], app_db, ACLS, &[], SECRET, blobs);

    let touch = format!("/api/svc.doc/{t}/service/touch");

    // No token → 401.
    let (st, _) = post(app.clone(), &touch, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // A junior (Write not granted) → 403.
    let (st, _) = post(app.clone(), &touch, Some("usr")).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "no Write, service denied");

    // A manager → 200 and the body echoes the record id.
    let (st, body) = post(app.clone(), &touch, Some("mgr")).await;
    assert_eq!(st, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["ok"].as_i64(), Some(t), "the gated body ran and returned its JSON");

    // Unknown service name → 400.
    let (st, _) = post(app.clone(), &format!("/api/svc.doc/{t}/service/nope"), Some("mgr")).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "unknown service");

    // Unknown record id → 400 (not found / not permitted — deliberately not a 404 existence oracle).
    let (st, _) = post(app.clone(), "/api/svc.doc/999999/service/touch", Some("mgr")).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    seed.drop_table(&doc).await.unwrap();
}
