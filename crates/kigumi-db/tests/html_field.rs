//! FieldKind::Html: a text-backed rich-text field sanitized on write (allowlist) so stored XSS can
//! never land. Safe formatting survives; <script>, event-handler attributes and javascript: URLs are
//! stripped before the value is stored. Live Postgres.

use kigumi_core::{resolve, Acl, Ctx, FieldDef, FieldKind, ModelDescriptor};
use kigumi_db::Db;
use kigumi_schema::to_ddl;
use serde_json::json;

static DOC: ModelDescriptor = ModelDescriptor {
    name: "html.doc",
    table: "html_doc",
    fields: &[FieldDef { name: "body", label: "Body", kind: FieldKind::Html, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
};
static ACLS: &[Acl] = &[Acl { model: "html.doc", group: "u", read: true, write: true, create: true, delete: true }];

#[tokio::test]
async fn html_is_sanitized_on_write_and_stored_as_text() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let m = resolve(&DOC, &[]).unwrap();
    // Html is a plain text column at the DB level.
    assert!(to_ddl(&m).contains("body text"), "Html is a text column");

    let db = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();
    db.drop_table(&m).await.unwrap();
    db.create_table(&m).await.unwrap();

    let dirty = "<p>Hello <b>world</b></p>\
                 <script>alert('xss')</script>\
                 <a href=\"javascript:alert(1)\">click</a>\
                 <p onclick=\"steal()\">x</p>";
    let id = db.insert_secured(&m, &su, ACLS, &[], json!({ "body": dirty }).as_object().unwrap()).await.unwrap();
    let body = db.find_one_secured(&m, &su, ACLS, &[], id).await.unwrap().unwrap()["body"].as_str().unwrap().to_string();

    // The executable vectors are gone.
    assert!(!body.contains("<script"), "script tag stripped: {body}");
    assert!(!body.to_lowercase().contains("onclick"), "event handler stripped: {body}");
    assert!(!body.to_lowercase().contains("javascript:"), "javascript: URL stripped: {body}");
    // Safe formatting is preserved.
    assert!(body.contains("<b>world</b>"), "safe formatting kept: {body}");
    assert!(body.contains("<p>Hello"), "paragraph kept: {body}");

    // The stored value is ALREADY sanitized (the DB column holds the clean HTML, not the input).
    let raw: String = sqlx::query_scalar("SELECT body FROM html_doc WHERE id = $1").bind(id).fetch_one(db.pool()).await.unwrap();
    assert_eq!(raw, body, "stored value equals the sanitized projection");
    assert!(!raw.contains("<script"), "no script in the stored column");

    // An update is sanitized too.
    db.update_secured(&m, &su, ACLS, &[], id, json!({ "body": "<img src=x onerror=alert(1)><i>ok</i>" }).as_object().unwrap()).await.unwrap();
    let body2 = db.find_one_secured(&m, &su, ACLS, &[], id).await.unwrap().unwrap()["body"].as_str().unwrap().to_string();
    assert!(!body2.to_lowercase().contains("onerror"), "update sanitized: {body2}");
    assert!(body2.contains("<i>ok</i>"), "safe tag kept on update: {body2}");

    db.drop_table(&m).await.unwrap();
}
