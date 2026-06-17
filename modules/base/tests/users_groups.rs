//! M6/D11: res.users + res.groups as read-only metamodel models. res.users is an EXTERNAL table
//! (a projection of the auth subsystem's `meshble_user`) — excluded from migration and projected
//! WITHOUT the password hash. res.groups is a normal seeded list. Live Postgres; skipped without
//! DATABASE_URL. Lightweight on purpose (no full-catalog migrate) to avoid racing other test
//! binaries on the shared reference tables.

use meshble::prelude::*;
use meshble_auth::hash_password;
use meshble_db::Db;

fn link() {
    let _ = &meshble_mod_base::MANIFEST;
}

#[tokio::test]
async fn res_users_is_an_external_readonly_projection() {
    link();

    // Registry facts (pure — no DB needed): res.users is external (not migrated), res.groups is.
    assert!(external_tables().contains(&"res.users"), "res.users is marked external");
    let migrated: Vec<&str> = migration_plan().unwrap().iter().map(|t| t.model.name).collect();
    assert!(!migrated.contains(&"res.users"), "external table excluded from migration");
    assert!(migrated.contains(&"res.groups"), "res.groups is a normal migrated table");

    // The seeding source for res.groups: every group referenced by registered ACLs/rules.
    let groups = registered_group_names();
    assert!(groups.contains(&"user".to_string()) && groups.contains(&"admin".to_string()));

    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping DB part: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();
    db.ensure_auth_schema().await.unwrap(); // owns meshble_user (the external table)

    // A user created through the AUTH subsystem (upsert is idempotent on login)...
    db.upsert_user("ada-d11", &hash_password("x").unwrap(), &["user", "admin"]).await.unwrap();

    // ...is visible through the res.users model — login + groups, but never the password hash.
    let users = resolve_registered("res.users").unwrap();
    let by_login = Domain::field("login").eq("ada-d11");
    let rows = db.find_secured(&users, &su, &[], &[], Some(&by_login)).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["login"], "ada-d11");
    assert_eq!(rows[0]["groups"], "user,admin");
    assert!(rows[0].get("password_hash").is_none(), "password hash is never projected");
}
