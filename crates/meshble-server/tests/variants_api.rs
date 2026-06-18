//! Slice 4: the generate_variants endpoint over real HTTP. The gate is WRITE on product.template
//! (manager-only via ACL); generation then runs elevated. This pins the authorization end-to-end:
//! a junior is forbidden, a manager succeeds, a wrong path / unknown template are rejected, and no
//! token is unauthorized. Requires DATABASE_URL.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use meshble_auth::Authenticator;
use meshble_core::{resolve, Acl, Ctx, FieldDef, FieldKind, InheritsRegistration, ModelDescriptor, ModelRegistration, ResolvedModel};
use meshble_db::Db;
use meshble_server::router_with_data;
use serde_json::json;
use tower::ServiceExt;

const SECRET: &str = "variants-api-secret";

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
const fn m2m(name: &'static str, target: &'static str, relation: &'static str, column: &'static str, target_column: &'static str) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Many2many { target, relation, column, target_column }, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None }
}

static ATTRIBUTE: ModelDescriptor = ModelDescriptor { name: "product.attribute", table: "product_attribute", fields: &[txt("name", true), FieldDef { name: "create_variant", label: "create_variant", kind: FieldKind::Selection(&[("always", "Instantly"), ("no_variant", "Never")]), required: false, stored: true, compute: None, depends: &[], default: Some("always"), unique: false, check: None }] };
static ATTR_VALUE: ModelDescriptor = ModelDescriptor { name: "product.attribute.value", table: "product_attribute_value", fields: &[txt("name", true), m2o("attribute_id", "product.attribute", true)] };
static TEMPLATE: ModelDescriptor = ModelDescriptor { name: "product.template", table: "product_template", fields: &[txt("name", true)] };
static VARIANT: ModelDescriptor = ModelDescriptor {
    name: "product.product", table: "product_product",
    fields: &[
        m2o("product_tmpl_id", "product.template", true),
        FieldDef { name: "active", label: "active", kind: FieldKind::Bool, required: false, stored: true, compute: None, depends: &[], default: Some("true"), unique: false, check: None },
        FieldDef { name: "price_extra", label: "Extra", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[], default: Some("0"), unique: false, check: None },
        m2m("product_template_attribute_value_ids", "product.template.attribute.value", "variant_ptav_rel", "product_id", "ptav_id"),
    ],
};
static LINE: ModelDescriptor = ModelDescriptor {
    name: "product.template.attribute.line", table: "product_template_attribute_line",
    fields: &[m2o("product_tmpl_id", "product.template", true), m2o("attribute_id", "product.attribute", true), m2m("value_ids", "product.attribute.value", "ptal_value_rel", "line_id", "value_id")],
};
static PTAV: ModelDescriptor = ModelDescriptor {
    name: "product.template.attribute.value", table: "product_template_attribute_value",
    fields: &[m2o("product_tmpl_id", "product.template", true), m2o("attribute_line_id", "product.template.attribute.line", true), m2o("product_attribute_value_id", "product.attribute.value", true), FieldDef { name: "price_extra", label: "Extra", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[], default: Some("0"), unique: false, check: None }],
};
fn f_attr() -> &'static ModelDescriptor { &ATTRIBUTE }
fn f_val() -> &'static ModelDescriptor { &ATTR_VALUE }
fn f_tmpl() -> &'static ModelDescriptor { &TEMPLATE }
fn f_var() -> &'static ModelDescriptor { &VARIANT }
fn f_line() -> &'static ModelDescriptor { &LINE }
fn f_ptav() -> &'static ModelDescriptor { &PTAV }
meshble_core::inventory::submit! { ModelRegistration { name: "product.attribute", module: "test", descriptor: f_attr } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.attribute.value", module: "test", descriptor: f_val } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.template", module: "test", descriptor: f_tmpl } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.product", module: "test", descriptor: f_var } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.template.attribute.line", module: "test", descriptor: f_line } }
meshble_core::inventory::submit! { ModelRegistration { name: "product.template.attribute.value", module: "test", descriptor: f_ptav } }
meshble_core::inventory::submit! { InheritsRegistration { model: "product.product", parent: "product.template", via: "product_tmpl_id" } }

// The manager maintains templates; a junior may only read. Generation gates on product.template WRITE.
static ACLS: &[Acl] = &[
    Acl { model: "product.template", group: "sales.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "product.template", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "product.product", group: "sales.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "product.product", group: "sales.user", read: true, write: false, create: false, delete: false },
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
async fn generate_variants_endpoint_authorization() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let seed = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();
    let (attr, val, tmpl, variant, line, ptav) = (m(&ATTRIBUTE), m(&ATTR_VALUE), m(&TEMPLATE), m(&VARIANT), m(&LINE), m(&PTAV));

    for j in ["variant_ptav_rel", "ptal_value_rel"] { sqlx::query(&format!("DROP TABLE IF EXISTS {j}")).execute(seed.pool()).await.unwrap(); }
    for d in [&ptav, &line, &variant, &tmpl, &val, &attr] { seed.drop_table(d).await.unwrap(); }
    for d in [&attr, &val, &tmpl, &variant, &line, &ptav] { seed.create_table(d).await.unwrap(); }
    seed.create_m2m_relations(&variant).await.unwrap();
    seed.create_m2m_relations(&line).await.unwrap();

    // Seed one template with Color(Red,Blue) x Size(S) = 2 combinations.
    let color = seed.insert_secured(&attr, &su, &[], &[], json!({ "name": "Color" }).as_object().unwrap()).await.unwrap();
    let size = seed.insert_secured(&attr, &su, &[], &[], json!({ "name": "Size" }).as_object().unwrap()).await.unwrap();
    let red = seed.insert_secured(&val, &su, &[], &[], json!({ "name": "Red", "attribute_id": color }).as_object().unwrap()).await.unwrap();
    let blue = seed.insert_secured(&val, &su, &[], &[], json!({ "name": "Blue", "attribute_id": color }).as_object().unwrap()).await.unwrap();
    let s = seed.insert_secured(&val, &su, &[], &[], json!({ "name": "S", "attribute_id": size }).as_object().unwrap()).await.unwrap();
    let t = seed.insert_secured(&tmpl, &su, &[], &[], json!({ "name": "Shirt" }).as_object().unwrap()).await.unwrap();
    seed.insert_secured(&line, &su, &[], &[], json!({ "product_tmpl_id": t, "attribute_id": color, "value_ids": [red, blue] }).as_object().unwrap()).await.unwrap();
    seed.insert_secured(&line, &su, &[], &[], json!({ "product_tmpl_id": t, "attribute_id": size, "value_ids": [s] }).as_object().unwrap()).await.unwrap();

    let app_db = Db::connect(&url).await.unwrap();
    let models = vec![m(&ATTRIBUTE), m(&ATTR_VALUE), m(&TEMPLATE), m(&VARIANT), m(&LINE), m(&PTAV)];
    let blobs = std::sync::Arc::new(meshble_server::FsBlobStore::new(std::env::temp_dir().join("meshble_test_blobs")));
    let app = router_with_data(models, app_db, ACLS, &[], SECRET, blobs);

    let gen = format!("/api/product.template/{t}/generate_variants");

    // No token → 401.
    let (st, _) = post(app.clone(), &gen, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // A junior (template WRITE not granted) → 403, and nothing is generated.
    let (st, _) = post(app.clone(), &gen, Some("sales.user")).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "junior cannot generate");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM product_product WHERE product_tmpl_id=$1").bind(t).fetch_one(seed.pool()).await.unwrap();
    assert_eq!(n, 0, "no variants created by the denied request");

    // A manager → 200 and the 2 combinations are generated.
    let (st, body) = post(app.clone(), &gen, Some("sales.manager")).await;
    assert_eq!(st, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["created"].as_array().unwrap().len(), 2, "two variants created");

    // Wrong host model (name is pinned to product.template) → 400.
    let (st, _) = post(app.clone(), &format!("/api/product.product/{t}/generate_variants"), Some("sales.manager")).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "endpoint pinned to product.template");

    // Unknown template id → 400 (not found / not permitted — deliberately not a 404 existence oracle).
    let (st, _) = post(app.clone(), "/api/product.template/999999/generate_variants", Some("sales.manager")).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    for j in ["variant_ptav_rel", "ptal_value_rel"] { sqlx::query(&format!("DROP TABLE IF EXISTS {j}")).execute(seed.pool()).await.unwrap(); }
    for d in [&ptav, &line, &variant, &tmpl, &val, &attr] { seed.drop_table(d).await.unwrap(); }
}
