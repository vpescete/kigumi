//! M15.4 slice 2: the wizard `open` endpoint over real HTTP. Opening a `register_wizard!`-bound
//! transient model runs its server-side `default_get` (seeding from the open context), creates the
//! scratchpad under the caller's create ACL, and returns it (with the DB-defaulted `create_date`). A
//! non-wizard model is 400, no token is 401, and a context that can't satisfy a required seed is 400.
//! Requires DATABASE_URL.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use kigumi_auth::Authenticator;
use kigumi_core::{
    resolve, Acl, Ctx, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, ResolvedModel,
    TransientRegistration, Value, WizardContext, WizardRegistration,
};
use kigumi_db::Db;
use kigumi_server::router_with_data;
use serde_json::json;
use tower::ServiceExt;

const SECRET: &str = "wizard-api-secret";

fn bearer(groups: &str) -> String {
    let g: Vec<String> = groups.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
    let token = Authenticator::new(SECRET).issue_access(1, g, None, vec![], 3600).unwrap();
    format!("Bearer {token}")
}

const fn txt(name: &'static str, required: bool) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Text, required, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}
const fn m2o(name: &'static str, target: &'static str, required: bool) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Many2one { target }, required, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}
const fn dec(name: &'static str) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[], default: Some("0"), unique: false, check: None }
}
const fn dtm(name: &'static str) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Datetime, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}

static THING: ModelDescriptor = ModelDescriptor { name: "test.thing", table: "test_thing", fields: &[txt("name", true)] };
// The wizard targets a thing via a REQUIRED m2o, so an open with no active record cannot seed it → 400.
static WIZ: ModelDescriptor = ModelDescriptor {
    name: "test.wiz", table: "test_wiz",
    fields: &[m2o("thing_id", "test.thing", true), dec("discount"), dtm("create_date")],
};
fn f_thing() -> &'static ModelDescriptor { &THING }
fn f_wiz() -> &'static ModelDescriptor { &WIZ }
kigumi_core::inventory::submit! { ModelRegistration { name: "test.thing", module: "test", descriptor: f_thing } }
kigumi_core::inventory::submit! { ModelRegistration { name: "test.wiz", module: "test", descriptor: f_wiz } }
kigumi_core::inventory::submit! { TransientRegistration { model: "test.wiz" } }

/// default_get: seed thing_id from the open context's active record (empty if there is none).
fn seed_wiz(ctx: &WizardContext) -> Vec<(&'static str, Value)> {
    match ctx.active_id {
        Some(id) => vec![("thing_id", Value::Int(id))],
        None => vec![],
    }
}
kigumi_core::inventory::submit! { WizardRegistration { model: "test.wiz", default_get: seed_wiz } }

static ACLS: &[Acl] = &[
    Acl { model: "test.thing", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "test.wiz", group: "u", read: true, write: true, create: true, delete: false },
];

fn m(d: &'static ModelDescriptor) -> ResolvedModel { resolve(d, &[]).unwrap() }

async fn open(app: Router, uri: &str, groups: Option<&str>, body: serde_json::Value) -> (StatusCode, String) {
    let mut b = Request::builder().method("POST").uri(uri).header("content-type", "application/json");
    if let Some(g) = groups {
        b = b.header("authorization", bearer(g));
    }
    let resp = app.oneshot(b.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn open_wizard_seeds_from_context_and_is_secured() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let seed = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();
    let (thing, wiz) = (m(&THING), m(&WIZ));
    for d in [&wiz, &thing] { seed.drop_table(d).await.unwrap(); }
    for d in [&thing, &wiz] { seed.create_table(d).await.unwrap(); }
    seed.ensure_transient_defaults().await.unwrap();

    let thing_id = seed.insert_secured(&thing, &su, &[], &[], json!({ "name": "Target" }).as_object().unwrap()).await.unwrap();

    let app_db = Db::connect(&url).await.unwrap();
    let blobs = std::sync::Arc::new(kigumi_server::FsBlobStore::new(std::env::temp_dir().join("kigumi_test_blobs")));
    let app = router_with_data(vec![m(&THING), m(&WIZ)], app_db, ACLS, &[], SECRET, blobs);

    // No token → 401.
    let (st, _) = open(app.clone(), "/api/test.wiz/open", None, json!({ "active_id": thing_id })).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // Authenticated open with an active record → 201, thing_id seeded, create_date stamped by the DB.
    let (st, body) = open(app.clone(), "/api/test.wiz/open", Some("u"), json!({ "active_model": "test.thing", "active_id": thing_id })).await;
    assert_eq!(st, StatusCode::CREATED, "open should create the scratchpad");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["thing_id"].as_i64(), Some(thing_id), "thing_id seeded from active_id");
    assert_eq!(v["discount"].as_str(), Some("0"), "discount defaulted");
    assert!(!v["create_date"].is_null(), "create_date stamped by the DB default");

    // A non-wizard model → 400 (test.thing is not register_wizard!-bound).
    let (st, _) = open(app.clone(), "/api/test.thing/open", Some("u"), json!({})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "non-wizard model rejected");

    // No active record → empty seed → the required thing_id can't be satisfied → 400.
    let (st, _) = open(app.clone(), "/api/test.wiz/open", Some("u"), json!({})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "missing required seed is a bad request");

    for d in [&wiz, &thing] { seed.drop_table(d).await.unwrap(); }
}
