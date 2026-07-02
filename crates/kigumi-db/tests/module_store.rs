//! The installed-module registry (approach B): install/uninstall a module is a row in
//! installed_module; uninstall keeps the row's absence non-destructive (tables untouched here, since
//! this test only exercises the registry, not migration). Live Postgres.

use kigumi_db::Db;

#[tokio::test]
async fn installed_module_registry_roundtrip() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    db.ensure_module_schema().await.unwrap();
    db.mark_module_uninstalled("mtest").await.unwrap(); // clean slate for re-runs

    assert!(!db.is_module_installed("mtest").await.unwrap(), "absent by default");

    db.mark_module_installed("mtest", "1.2.3").await.unwrap();
    assert!(db.is_module_installed("mtest").await.unwrap());
    assert!(db.installed_modules().await.unwrap().iter().any(|m| m == "mtest"));

    // Re-install updates the recorded version (idempotent), not a duplicate row.
    db.mark_module_installed("mtest", "1.3.0").await.unwrap();
    assert_eq!(db.installed_modules().await.unwrap().iter().filter(|m| *m == "mtest").count(), 1);

    db.mark_module_uninstalled("mtest").await.unwrap();
    assert!(!db.is_module_installed("mtest").await.unwrap(), "uninstall removes the row");
}
