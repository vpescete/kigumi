//! M6 end-to-end: a user assigned to a company logs in and the access token carries that scope, so
//! the secured data routes return only that company's rows — proving multi-company is enforced PER
//! USER (no longer the empty=unrestricted stub). Under M7 default-deny an UNASSIGNED user sees only
//! shared rows (here: none), and the scope survives a token refresh. Requires `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use kigumi_auth::hash_password;
use kigumi_core::{resolve, Acl, FieldDef, FieldKind, ModelDescriptor, RecordRule, ResolvedModel};
use kigumi_server::router_with_data;
use tower::ServiceExt;

const SECRET: &str = "company-scope-secret";

static DOC: ModelDescriptor = ModelDescriptor {
    name: "csa.doc",
    table: "csa_doc",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "company_id", label: "Company", kind: FieldKind::Many2one { target: "csa.company" }, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
static ACLS: &[Acl] = &[Acl { model: "csa.doc", group: "u", read: true, write: false, create: false, delete: false }];
static RULES: &[RecordRule] = &[];

fn model() -> ResolvedModel {
    resolve(&DOC, &[]).unwrap()
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

/// GETs /api/csa.doc with a bearer and returns the number of rows in the envelope's `data`.
async fn visible_rows(app: Router, bearer: &str) -> usize {
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/csa.doc")
                .header("authorization", format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    body["data"].as_array().map(|a| a.len()).unwrap_or(0)
}

#[tokio::test]
async fn login_scopes_data_to_the_users_company() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let setup = &t.db;
    let m = model();

    // A company table for the FK + one doc in each company (ad-hoc — the kit does not know them).
    setup.drop_table(&m).await.unwrap();
    sqlx::query("DROP TABLE IF EXISTS csa_company").execute(setup.pool()).await.unwrap();
    sqlx::query("CREATE TABLE csa_company (id bigserial PRIMARY KEY, name text)").execute(setup.pool()).await.unwrap();
    let c1: i64 = sqlx::query_scalar("INSERT INTO csa_company (name) VALUES ('C1') RETURNING id").fetch_one(setup.pool()).await.unwrap();
    let c2: i64 = sqlx::query_scalar("INSERT INTO csa_company (name) VALUES ('C2') RETURNING id").fetch_one(setup.pool()).await.unwrap();
    setup.create_table(&m).await.unwrap();
    sqlx::query("INSERT INTO csa_doc (name, company_id) VALUES ('in-c1', $1), ('in-c2', $2)").bind(c1).bind(c2).execute(setup.pool()).await.unwrap();

    setup.upsert_user("scoped", &hash_password("pw").unwrap(), &["u"]).await.unwrap();
    setup.set_user_companies("scoped", Some(c1), &[c1]).await.unwrap();
    setup.upsert_user("roamer", &hash_password("pw").unwrap(), &["u"]).await.unwrap(); // no assignment → unrestricted

    // Assigning a company to a non-existent user is an error, not a silent no-op.
    assert!(setup.set_user_companies("ghost", Some(c1), &[]).await.is_err());

    let blobs = std::sync::Arc::new(kigumi_server::FsBlobStore::new(std::env::temp_dir().join("kigumi_test_blobs")));
    let app = router_with_data(vec![model()], t.db.clone(), ACLS, RULES, SECRET, blobs);

    // The scoped user's login token sees only its company's row.
    let (s, tok) = post(app.clone(), "/auth/login", r#"{"login":"scoped","password":"pw"}"#).await;
    assert_eq!(s, StatusCode::OK);
    let access = tok["access_token"].as_str().unwrap().to_string();
    assert_eq!(visible_rows(app.clone(), &access).await, 1, "scoped login sees only its company");

    // Scope survives a refresh (re-read from kigumi_user, like groups).
    let refresh = tok["refresh_token"].as_str().unwrap().to_string();
    let (_, tok2) = post(app.clone(), "/auth/refresh", &format!(r#"{{"refresh_token":"{refresh}"}}"#)).await;
    let access2 = tok2["access_token"].as_str().unwrap().to_string();
    assert_eq!(visible_rows(app.clone(), &access2).await, 1, "scope survives refresh");

    // M7 default-deny: an unassigned user is NOT god-mode — it sees only shared (NULL-company) rows,
    // so with every row owned by a company it sees nothing.
    let (_, tokr) = post(app.clone(), "/auth/login", r#"{"login":"roamer","password":"pw"}"#).await;
    let accessr = tokr["access_token"].as_str().unwrap().to_string();
    assert_eq!(visible_rows(app.clone(), &accessr).await, 0, "unassigned user sees no company-owned rows");

    setup.drop_table(&m).await.unwrap();
    sqlx::query("DROP TABLE IF EXISTS csa_company").execute(setup.pool()).await.unwrap();
}
