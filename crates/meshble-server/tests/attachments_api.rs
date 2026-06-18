//! Attachments over HTTP: upload/list/download/delete a file on a host record, gated by host access
//! (read to list/download, write to upload/delete), with bytes in the content-addressed blob store and
//! the row cleaned up when the host record is deleted. Synthetic host + ir.attachment models; a real
//! FsBlobStore over a temp dir. Requires DATABASE_URL.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use meshble_auth::Authenticator;
use meshble_core::{resolve, Acl, Ctx, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, ResolvedModel};
use meshble_db::Db;
use meshble_server::{router_with_data, FsBlobStore};
use serde_json::json;
use tower::ServiceExt;

const SECRET: &str = "attachments-secret";

fn bearer(groups: &str) -> String {
    let g: Vec<String> = groups.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
    format!("Bearer {}", Authenticator::new(SECRET).issue_access(1, g, None, vec![], 3600).unwrap())
}

const fn txt(name: &'static str) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}
const fn int(name: &'static str) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Integer, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}

static HOST: ModelDescriptor = ModelDescriptor { name: "att.host", table: "att_host", fields: &[txt("name")] };
static ATTACHMENT: ModelDescriptor = ModelDescriptor {
    name: "ir.attachment",
    table: "meshble_attachment",
    fields: &[txt("name"), txt("res_model"), int("res_id"), txt("mimetype"), int("file_size"), txt("checksum")],
};
fn host_d() -> &'static ModelDescriptor { &HOST }
fn att_d() -> &'static ModelDescriptor { &ATTACHMENT }
meshble_core::inventory::submit! { ModelRegistration { name: "att.host", module: "test", descriptor: host_d } }
meshble_core::inventory::submit! { ModelRegistration { name: "ir.attachment", module: "test", descriptor: att_d } }

// `w` can modify the host (so manage its attachments); `r` may only read it.
static ACLS: &[Acl] = &[
    Acl { model: "att.host", group: "w", read: true, write: true, create: true, delete: true },
    Acl { model: "att.host", group: "r", read: true, write: false, create: false, delete: false },
    Acl { model: "ir.attachment", group: "admin", read: true, write: true, create: true, delete: true },
];

fn m(d: &'static ModelDescriptor) -> ResolvedModel { resolve(d, &[]).unwrap() }

async fn send(app: Router, method: &str, uri: &str, groups: Option<&str>, ctype: Option<&str>, filename: Option<&str>, body: Vec<u8>) -> (StatusCode, Vec<u8>) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(g) = groups {
        b = b.header("authorization", bearer(g));
    }
    if let Some(c) = ctype {
        b = b.header("content-type", c);
    }
    if let Some(f) = filename {
        b = b.header("x-filename", f);
    }
    let resp = app.oneshot(b.body(Body::from(body)).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, bytes)
}

#[tokio::test]
async fn attachment_lifecycle_and_gates() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let seed = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();
    let (host, att) = (m(&HOST), m(&ATTACHMENT));
    seed.drop_table(&att).await.unwrap();
    seed.drop_table(&host).await.unwrap();
    seed.create_table(&host).await.unwrap();
    seed.create_table(&att).await.unwrap();
    let hid = seed.insert_secured(&host, &su, ACLS, &[], json!({ "name": "doc holder" }).as_object().unwrap()).await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let blobs = std::sync::Arc::new(FsBlobStore::new(tmp.path()));
    let app = router_with_data(vec![m(&HOST), m(&ATTACHMENT)], Db::connect(&url).await.unwrap(), ACLS, &[], SECRET, blobs);

    let list_uri = format!("/api/att.host/{hid}/attachments");
    let payload = b"PDF-ish bytes \x00\x01 hello".to_vec();

    // No token → 401.
    let (st, _) = send(app.clone(), "GET", &list_uri, None, None, None, vec![]).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // A user with no host access → 403 (the gate is host visibility).
    let (st, _) = send(app.clone(), "GET", &list_uri, Some("none"), None, None, vec![]).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // A reader cannot upload (no host write) → 403; nothing stored.
    let (st, _) = send(app.clone(), "POST", &list_uri, Some("r"), Some("application/pdf"), Some("a.pdf"), payload.clone()).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "reader cannot upload");

    // A writer uploads → 201 with the checksum.
    let (st, body) = send(app.clone(), "POST", &list_uri, Some("w"), Some("application/pdf"), Some("a.pdf"), payload.clone()).await;
    assert_eq!(st, StatusCode::CREATED);
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let aid = created["id"].as_i64().unwrap();
    assert_eq!(created["file_size"].as_i64(), Some(payload.len() as i64));
    assert_eq!(created["checksum"].as_str().unwrap().len(), 64, "checksum is a sha256 hex");

    // A reader CAN list (host read) and see the one attachment.
    let (st, body) = send(app.clone(), "GET", &list_uri, Some("r"), None, None, vec![]).await;
    assert_eq!(st, StatusCode::OK);
    let listed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(listed["data"].as_array().unwrap().len(), 1);

    // Download returns the exact bytes.
    let (st, bytes) = send(app.clone(), "GET", &format!("/api/attachment/{aid}/content"), Some("r"), None, None, vec![]).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(bytes, payload, "downloaded bytes match the upload");

    // A reader cannot delete (no host write) → 403.
    let (st, _) = send(app.clone(), "DELETE", &format!("/api/attachment/{aid}"), Some("r"), None, None, vec![]).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // A writer deletes the attachment → 200; the list is empty again.
    let (st, _) = send(app.clone(), "DELETE", &format!("/api/attachment/{aid}"), Some("w"), None, None, vec![]).await;
    assert_eq!(st, StatusCode::OK);
    let (_, body) = send(app.clone(), "GET", &list_uri, Some("w"), None, None, vec![]).await;
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["data"].as_array().unwrap().len(), 0);

    // Delete-cleanup: re-upload, then delete the HOST record → its attachment rows are removed.
    send(app.clone(), "POST", &list_uri, Some("w"), Some("text/plain"), Some("b.txt"), b"again".to_vec()).await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM meshble_attachment WHERE res_model='att.host' AND res_id=$1").bind(hid).fetch_one(seed.pool()).await.unwrap();
    assert_eq!(n, 1, "attachment present before host delete");
    seed.delete_secured(&host, &su, ACLS, &[], hid).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM meshble_attachment WHERE res_model='att.host' AND res_id=$1").bind(hid).fetch_one(seed.pool()).await.unwrap();
    assert_eq!(n, 0, "attachments cleaned up when the host record is deleted");

    seed.drop_table(&att).await.unwrap();
    seed.drop_table(&host).await.unwrap();
}
