//! Runnable demo wiring the whole stack: defines a `task` model with `#[model]`, migrates and
//! seeds it on Postgres, and serves the headless API (`meshble-server`) plus the agnostic
//! reference renderer (`webui/app.html`) on one port. Prints a ready-to-use deep link with a JWT.
//!
//! Run: `DATABASE_URL=postgres://you@127.0.0.1/meshble_test cargo run -p meshble-renderer-demo`

use axum::response::Html;
use axum::routing::get;
use meshble::prelude::*;
use meshble_auth::{hash_password, Authenticator};
use meshble_db::Db;
use meshble_server::router_with_data;

/// The demo model — note the field "types" are the `#[model]` DSL keywords.
#[model(name = "task", table = "task")]
pub struct Task {
    #[field(label = "Title", required)]
    name: Text,

    #[field(label = "Priority", required, selection = "low:Low,med:Medium,high:High")]
    priority: Selection,

    #[field(label = "Done")]
    done: Bool,

    #[field(label = "Notes")]
    notes: Text,
}

static ACLS: &[Acl] = &[Acl {
    model: "task", group: "user", read: true, write: true, create: true, delete: true,
}];
static RULES: &[RecordRule] = &[];

const UI: &str = include_str!("../../../webui/app.html");
const SECRET: &str = "meshble-demo-secret-change-me";

#[tokio::main]
async fn main() {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://127.0.0.1/meshble_test".to_string());
    let db = Db::connect(&url).await.expect("connect to Postgres (set DATABASE_URL)");
    let model = resolve(Task::descriptor(), &[]).unwrap();

    // Migrate the table, then seed a few rows (sudo bypasses ACL/rules for setup).
    db.install_or_upgrade(&model, "task", "1.0.0", &[]).await.expect("migrate");
    let su = Ctx::new(0, vec![]).sudo();
    if db.count_secured(&model, &su, ACLS, RULES, None).await.unwrap() == 0 {
        for (n, p, d) in [
            ("Write the reference renderer", "high", true),
            ("Ship the demo", "med", false),
            ("Add a login endpoint", "low", false),
        ] {
            let v = serde_json::json!({ "name": n, "priority": p, "done": d });
            db.insert_secured(&model, &su, ACLS, RULES, v.as_object().unwrap()).await.unwrap();
        }
    }

    // Auth: ensure the user/refresh tables and seed an admin (login: admin / admin).
    db.ensure_auth_schema().await.expect("auth schema");
    db.upsert_user("admin", &hash_password("admin").unwrap(), &["user"]).await.expect("seed admin");

    let token = Authenticator::new(SECRET).issue_access(1, vec!["user".to_string()], 86_400).unwrap();
    let db_app = Db::connect(&url).await.unwrap();
    let app = router_with_data(vec![model], db_app, ACLS, RULES, SECRET)
        .route("/", get(|| async { Html(UI) }));

    let addr = "127.0.0.1:8099";
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    println!("\n  Meshble reference renderer");
    println!("  Open:   http://{addr}/");
    println!("  Login:  admin / admin   (or deep link with a token below)");
    println!("  Token:  http://{addr}/?token={token}\n");
    axum::serve(listener, app).await.unwrap();
}
