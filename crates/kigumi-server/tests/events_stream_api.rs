//! The live event stream over a REAL socket (SSE is an infinite response — a oneshot can't read
//! it): a permitted client receives the create event as a data frame with its outbox id; a client
//! whose groups grant NO Read ACL on the model receives nothing for the same write; a client
//! resuming with Last-Event-ID immediately catches up the missed event from the query path (no
//! poller wait). Requires DATABASE_URL.

use axum::http::StatusCode;
use kigumi_auth::Authenticator;
use kigumi_core::{resolve, Acl, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, ResolvedModel};
use kigumi_server::router_with_data;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SECRET: &str = "events-stream-secret";

fn bearer(groups: &str) -> String {
    let g: Vec<String> = groups.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
    Authenticator::new(SECRET).issue_access(1, g, None, vec![], 3600).unwrap()
}

const fn txt(name: &'static str, required: bool) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Text, required, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}
static DOC: ModelDescriptor = ModelDescriptor { name: "sse.doc", table: "sse_doc", fields: &[txt("name", true)] };
fn f_doc() -> &'static ModelDescriptor { &DOC }
kigumi_core::inventory::submit! { ModelRegistration { name: "sse.doc", module: "test", descriptor: f_doc } }

static ACLS: &[Acl] = &[Acl { model: "sse.doc", group: "watcher", read: true, write: true, create: true, delete: true }];

fn m(d: &'static ModelDescriptor) -> ResolvedModel { resolve(d, &[]).unwrap() }

/// Opens the SSE stream and reads whatever arrives within `window`, returning the raw text.
async fn read_stream(addr: std::net::SocketAddr, groups: &str, last_event_id: Option<i64>, window: std::time::Duration) -> String {
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    let extra = last_event_id.map(|id| format!("last-event-id: {id}\r\n")).unwrap_or_default();
    let req = format!(
        "GET /api/events/stream?models=sse.doc HTTP/1.1\r\nhost: t\r\naccept: text/event-stream\r\nauthorization: Bearer {}\r\n{extra}\r\n",
        bearer(groups)
    );
    sock.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    let mut buf = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, sock.read(&mut buf)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(n)) => out.extend_from_slice(&buf[..n]),
            Ok(Err(_)) => break,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stream_delivers_filtered_events_and_resumes() {
    // sse.doc is REGISTERED — the kit's reset created its table, the event schema, and an empty outbox.
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let seed = &t.db;
    let doc = m(&DOC);

    let blobs = std::sync::Arc::new(kigumi_server::FsBlobStore::new(std::env::temp_dir().join("kigumi_test_blobs")));
    let app = router_with_data(vec![m(&DOC)], t.db.clone(), ACLS, &[], SECRET, blobs);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Two live clients connect FIRST (so "from now" covers the write below): one may read sse.doc,
    // one may not. Then a superuser write lands an event.
    let su = kigumi_test::su();
    let seed2 = seed.clone();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        seed2
            .insert_secured(&m(&DOC), &su, &[], &[], json!({ "name": "live-widget" }).as_object().unwrap())
            .await
            .unwrap()
    });
    let (watcher_out, blind_out) = tokio::join!(
        read_stream(addr, "watcher", None, std::time::Duration::from_secs(4)),
        read_stream(addr, "other", None, std::time::Duration::from_secs(4)),
    );
    let created_id = writer.await.unwrap();

    assert!(watcher_out.starts_with("HTTP/1.1 200"), "stream opens: {watcher_out}");
    assert!(
        watcher_out.contains("model.created") && watcher_out.contains("sse.doc"),
        "the permitted client received the create event: {watcher_out}"
    );
    assert!(
        watcher_out.contains(&format!("\"record_id\":{created_id}")),
        "the event names the created record: {watcher_out}"
    );
    assert!(blind_out.starts_with("HTTP/1.1 200"), "the stream itself opens for any authenticated caller");
    assert!(
        !blind_out.contains("data:"),
        "a caller with no Read ACL receives no event frames: {blind_out}"
    );

    // Resume: a client reconnecting with Last-Event-ID BEFORE the event catches it up immediately
    // from the query path — no poller tick needed.
    let resumed = read_stream(addr, "watcher", Some(0), std::time::Duration::from_millis(900)).await;
    assert!(
        resumed.contains("model.created") && resumed.contains(&format!("\"record_id\":{created_id}")),
        "Last-Event-ID catch-up replays the missed event: {resumed}"
    );
}
