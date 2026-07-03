//! M15.5 slice 1: the report endpoint over real HTTP. A report is secured exactly by read access to
//! its record (find_one_secured), so this pins: no token is 401, an unknown report name is 404, an
//! unknown/forbidden record is 404, and an authorized read renders the registered HTML. Requires
//! DATABASE_URL.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use kigumi_auth::Authenticator;
use kigumi_core::{
    resolve, Acl, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, ReportRegistration,
    ResolvedModel,
};
use kigumi_server::{router_with_data, router_with_data_rasterized, Rasterizer};
use serde_json::{json, Value as Json};
use std::sync::Arc;
use tower::ServiceExt;

const SECRET: &str = "report-api-secret";

fn bearer(groups: &str) -> String {
    let g: Vec<String> = groups.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
    let token = Authenticator::new(SECRET).issue_access(1, g, None, vec![], 3600).unwrap();
    format!("Bearer {token}")
}

const fn txt(name: &'static str, required: bool) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Text, required, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}

static DOC: ModelDescriptor = ModelDescriptor { name: "test.doc", table: "test_doc", fields: &[txt("name", true)] };
fn f_doc() -> &'static ModelDescriptor { &DOC }
kigumi_core::inventory::submit! { ModelRegistration { name: "test.doc", module: "test", descriptor: f_doc } }

/// A trivial report rendering the record's name into an HTML heading.
fn render_doc(rec: &Json) -> String {
    format!("<!doctype html><title>Slip</title><h1>{}</h1>", rec.get("name").and_then(Json::as_str).unwrap_or(""))
}
kigumi_core::inventory::submit! { ReportRegistration { model: "test.doc", name: "slip", title: "Slip", func: render_doc } }

static ACLS: &[Acl] = &[Acl { model: "test.doc", group: "u", read: true, write: true, create: true, delete: true }];

fn m(d: &'static ModelDescriptor) -> ResolvedModel { resolve(d, &[]).unwrap() }

async fn get(app: Router, uri: &str, groups: Option<&str>) -> (StatusCode, String) {
    let mut b = Request::builder().method("GET").uri(uri);
    if let Some(g) = groups {
        b = b.header("authorization", bearer(g));
    }
    let resp = app.oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn report_endpoint_is_secured_by_record_read() {
    // test.doc is REGISTERED — the kit's reset already created its table.
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let seed = &t.db;
    let su = kigumi_test::su();
    let doc = m(&DOC);
    let id = seed.insert_secured(&doc, &su, &[], &[], json!({ "name": "Hello" }).as_object().unwrap()).await.unwrap();

    let blobs = std::sync::Arc::new(kigumi_server::FsBlobStore::new(std::env::temp_dir().join("kigumi_test_blobs")));
    let app = router_with_data(vec![m(&DOC)], t.db.clone(), ACLS, &[], SECRET, blobs);

    let uri = format!("/api/test.doc/{id}/report/slip");

    // No token → 401.
    let (st, _) = get(app.clone(), &uri, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // Authorized read → 200 and the rendered HTML.
    let (st, body) = get(app.clone(), &uri, Some("u")).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body.contains("<h1>Hello</h1>"), "renders the record: {body}");

    // Unknown report name → 404.
    let (st, _) = get(app.clone(), &format!("/api/test.doc/{id}/report/nope"), Some("u")).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "unknown report");

    // Unknown record → 404 (find_one_secured returns nothing).
    let (st, _) = get(app.clone(), "/api/test.doc/999999/report/slip", Some("u")).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "unknown record");

    // PDF requested but no rasterizer configured → 501.
    let (st, _) = get(app.clone(), &format!("{uri}?format=pdf"), Some("u")).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED, "no rasterizer → 501");

    // Same seeded record, but a router WITH a rasterizer: ?format=pdf serves PDF bytes of the HTML.
    let blobs2 = std::sync::Arc::new(kigumi_server::FsBlobStore::new(std::env::temp_dir().join("kigumi_test_blobs")));
    let raster: Option<Arc<dyn Rasterizer>> = Some(Arc::new(FakeRasterizer));
    let app_pdf = router_with_data_rasterized(vec![m(&DOC)], t.db.clone(), ACLS, &[], SECRET, blobs2, raster);
    let resp = app_pdf
        .oneshot(Request::builder().method("GET").uri(format!("{uri}?format=pdf")).header("authorization", bearer("u")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("content-type").unwrap(), "application/pdf");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(bytes.starts_with(b"%PDF-fake"), "served the rasterizer's bytes");
    assert!(String::from_utf8_lossy(&bytes).contains("<h1>Hello</h1>"), "rasterized the rendered HTML");
}

/// A fake rasterizer: prefixes the HTML with a recognizable marker, no real PDF engine.
struct FakeRasterizer;
impl Rasterizer for FakeRasterizer {
    fn render_pdf(&self, html: &str) -> Result<Vec<u8>, String> {
        let mut v = b"%PDF-fake\n".to_vec();
        v.extend_from_slice(html.as_bytes());
        Ok(v)
    }
}
