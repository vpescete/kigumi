//! D12 (part 1): runtime DB-backed ACL overrides union with the compiled-in baseline. A DB grant
//! can only WIDEN access (the engine's ACL semantics are a union), never revoke a static grant, so
//! the static set stays a floor. check_access is pure, so no table is needed. Live Postgres.

use kigumi_core::{check_access, Acl, Ctx, Operation};
use kigumi_db::Db;

#[tokio::test]
async fn db_acl_overrides_widen_access_additively() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    db.ensure_access_schema().await.unwrap();
    db.remove_db_acl("acltest.doc", "clerk").await.unwrap(); // clean slate for re-runs

    let clerk = Ctx::new(1, vec!["clerk".to_string()]);

    // No static ACL, no DB override → denied.
    assert!(!check_access(Operation::Read, "acltest.doc", &clerk, &[]));

    // Grant read at runtime → loaded ACLs widen access, but only for the granted operation.
    db.set_db_acl("acltest.doc", "clerk", true, false, false, false).await.unwrap();
    let acls = db.load_acls_static().await.unwrap();
    assert!(check_access(Operation::Read, "acltest.doc", &clerk, &acls), "DB grant widens read");
    assert!(!check_access(Operation::Write, "acltest.doc", &clerk, &acls), "only the granted op");

    // Union with a compiled-in baseline: the static grant is preserved alongside the DB grants.
    let baseline = [Acl { model: "acltest.doc", group: "viewer", read: true, write: false, create: false, delete: false }];
    let mut merged = baseline.to_vec();
    merged.extend(db.load_acls_static().await.unwrap());
    let viewer = Ctx::new(2, vec!["viewer".to_string()]);
    assert!(check_access(Operation::Read, "acltest.doc", &viewer, &merged), "static baseline preserved");
    assert!(check_access(Operation::Read, "acltest.doc", &clerk, &merged), "DB grant also present");

    // Revoking the override removes only the DB grant; the static baseline is untouched.
    db.remove_db_acl("acltest.doc", "clerk").await.unwrap();
    let acls2 = db.load_acls_static().await.unwrap();
    assert!(!check_access(Operation::Read, "acltest.doc", &clerk, &acls2), "revoke removes the DB grant");
}
