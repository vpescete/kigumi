//! The catalog (`/openapi.json`, `/api/models`, `/api/:name/view`) follows the SAME ACLs as the data
//! it describes: authenticated by default, with an anonymous caller seeing exactly the models a
//! `public` Read ACL exposes — the portal primitive, not a second switch. Requires `DATABASE_URL`;
//! skipped otherwise.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use kigumi_auth::Authenticator;
use kigumi_core::{resolve, Acl, FieldDef, FieldKind, ModelDescriptor, RecordRule, ResolvedModel};
use kigumi_server::router_with_data;
use tower::ServiceExt;

const SECRET: &str = "test-secret-change-me";

static FIELDS: &[FieldDef] = &[FieldDef {
    name: "name", label: "Name", kind: FieldKind::Text,
    required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None,
}];

/// Readable only by group "u" — the ordinary, staff-only case.
static PRIVATE: ModelDescriptor =
    ModelDescriptor { name: "widget", table: "widget_catalog_test", fields: FIELDS };
/// Readable by the `public` group — the portal case that must stay visible to a guest.
static PORTAL: ModelDescriptor =
    ModelDescriptor { name: "flyer", table: "flyer_catalog_test", fields: FIELDS };

static ACLS: &[Acl] = &[
    Acl { model: "widget", group: "u", read: true, write: false, create: false, delete: false },
    Acl { model: "flyer", group: "public", read: true, write: false, create: false, delete: false },
];
static RULES: &[RecordRule] = &[];

fn models() -> Vec<ResolvedModel> {
    vec![resolve(&PRIVATE, &[]).unwrap(), resolve(&PORTAL, &[]).unwrap()]
}

fn bearer(groups: &str) -> String {
    let g: Vec<String> = groups.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
    format!("Bearer {}", Authenticator::new(SECRET).issue_access(1, g, None, vec![], 3600).unwrap())
}

/// `auth`: None = no Authorization header at all (the guest path).
async fn get(app: Router, uri: &str, auth: Option<String>) -> (StatusCode, String) {
    let mut req = Request::builder().uri(uri);
    if let Some(a) = auth {
        req = req.header("authorization", a);
    }
    let resp = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn app(db: kigumi_db::Db) -> Router {
    let blobs = std::sync::Arc::new(kigumi_server::FsBlobStore::new(
        std::env::temp_dir().join("kigumi_test_blobs"),
    ));
    router_with_data(models(), db, ACLS, RULES, SECRET, blobs)
}

#[tokio::test]
async fn anonymous_sees_only_what_the_public_group_may_read() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let app = app(t.db.clone());

    let (status, body) = get(app.clone(), "/api/models", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("flyer"), "the public model stays visible: {body}");
    assert!(!body.contains("widget"), "the private model must NOT be listed anonymously: {body}");

    // The spec is the integration contract; it must not describe models the caller cannot read.
    let (status, body) = get(app.clone(), "/openapi.json", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("/api/flyer"), "public path documented: {body}");
    assert!(!body.contains("widget"), "private model absent from the anonymous spec: {body}");

    // A model the caller cannot read answers exactly like one that does not exist — no name leak.
    let (status, _) = get(app.clone(), "/api/widget/view", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get(app.clone(), "/api/flyer/view", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_granted_group_sees_the_private_model() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let app = app(t.db.clone());

    let (status, body) = get(app.clone(), "/api/models", Some(bearer("u"))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("widget"), "group u reads widget: {body}");

    let (status, _) = get(app.clone(), "/api/widget/view", Some(bearer("u"))).await;
    assert_eq!(status, StatusCode::OK);

    // Authenticated but ungranted is still nothing — the ACL decides, not the fact of logging in.
    let (status, body) = get(app.clone(), "/api/models", Some(bearer("other"))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("widget"), "no grant, no listing: {body}");
}

/// Credentials that do not verify must say so, not degrade to guest: a client holding an expired
/// token would otherwise see a mysteriously empty catalog instead of "log in again".
#[tokio::test]
async fn a_bad_token_is_401_not_a_silent_downgrade_to_guest() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let app = app(t.db.clone());
    let bad = Some("Bearer not-a-real-token".to_string());
    for uri in ["/api/models", "/openapi.json", "/api/flyer/view"] {
        let (status, _) = get(app.clone(), uri, bad.clone()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} must 401 on a bad token");
    }
}
