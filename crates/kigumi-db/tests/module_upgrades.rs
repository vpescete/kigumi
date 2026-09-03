//! The register_migration! upgrade contract: pending steps run in semver order and bump the
//! ledger per step; already-passed versions never run; a fresh install (ledger already at the
//! linked version) runs nothing; re-running is a no-op; a linked crate OLDER than the ledger is a
//! refused downgrade. Requires DATABASE_URL.

use kigumi_core::{ModuleManifest, ModuleRegistration};
use kigumi_db::{DataMigrationRegistration, Db, DbError};
use std::future::Future;
use std::pin::Pin;

type Fut<'a> = Pin<Box<dyn Future<Output = Result<(), DbError>> + Send + 'a>>;

static MANIFEST: ModuleManifest = ModuleManifest {
    name: "upgtest",
    version: "1.2.0",
    framework: ">=0.2, <0.3",
    depends: &[],
    summary: "upgrade-contract test module",
};
kigumi_core::inventory::submit! { ModuleRegistration { manifest: || MANIFEST, crate_path: ::core::module_path!() } }

async fn probe(db: &Db, name: &str) -> Result<(), DbError> {
    sqlx::query("INSERT INTO upgrade_probe (name) VALUES ($1) ON CONFLICT (name) DO NOTHING")
        .bind(name)
        .execute(db.pool())
        .await?;
    Ok(())
}

// Registered deliberately OUT of semver order — the engine must sort.
fn to_1_2_0(db: &Db) -> Fut<'_> {
    Box::pin(probe(db, "to-1.2.0"))
}
fn to_1_1_0(db: &Db) -> Fut<'_> {
    Box::pin(probe(db, "to-1.1.0"))
}
/// Below the test's starting ledger version — must never run.
fn to_0_9_0(db: &Db) -> Fut<'_> {
    Box::pin(probe(db, "to-0.9.0"))
}
kigumi_core::inventory::submit! { DataMigrationRegistration { module: "upgtest", to_version: "1.2.0", func: to_1_2_0 } }
kigumi_core::inventory::submit! { DataMigrationRegistration { module: "upgtest", to_version: "1.1.0", func: to_1_1_0 } }
kigumi_core::inventory::submit! { DataMigrationRegistration { module: "upgtest", to_version: "0.9.0", func: to_0_9_0 } }

async fn ledger_version(db: &Db, name: &str) -> String {
    sqlx::query_scalar("SELECT installed_version FROM installed_module WHERE name = $1")
        .bind(name)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn upgrades_run_pending_steps_in_order_and_refuse_downgrades() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    sqlx::query("CREATE TABLE IF NOT EXISTS upgrade_probe (id BIGSERIAL, name TEXT PRIMARY KEY)")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("TRUNCATE upgrade_probe").execute(db.pool()).await.unwrap();

    async fn rows(db: &Db) -> Vec<String> {
        sqlx::query_scalar("SELECT name FROM upgrade_probe ORDER BY id")
            .fetch_all(db.pool())
            .await
            .unwrap()
    }

    // Installed at 1.0.0, binary links 1.2.0 → the two pending steps run in semver order (the
    // 0.9.0 step is already passed and must not run), and the ledger lands on the linked version.
    db.mark_module_installed("upgtest", "1.0.0").await.unwrap();
    let applied = db.run_pending_upgrades().await.unwrap();
    assert_eq!(
        applied,
        vec![("upgtest".to_string(), "1.1.0".to_string()), ("upgtest".to_string(), "1.2.0".to_string())]
    );
    assert_eq!(rows(db).await, vec!["to-1.1.0".to_string(), "to-1.2.0".to_string()]);
    assert_eq!(ledger_version(db, "upgtest").await, "1.2.0");

    // Re-run: nothing pending, nothing applied (idempotent migrate).
    assert!(db.run_pending_upgrades().await.unwrap().is_empty());
    assert_eq!(rows(db).await.len(), 2);

    // Fresh-install semantics: a ledger already at the linked version runs no migrations.
    sqlx::query("TRUNCATE upgrade_probe").execute(db.pool()).await.unwrap();
    db.mark_module_installed("upgtest", "1.2.0").await.unwrap();
    assert!(db.run_pending_upgrades().await.unwrap().is_empty());
    assert!(rows(db).await.is_empty(), "a fresh install never replays history");

    // Downgrade: ledger ahead of the linked crate is refused with a clear error.
    db.mark_module_installed("upgtest", "9.9.9").await.unwrap();
    let err = db.run_pending_upgrades().await.unwrap_err();
    assert!(format!("{err:?}").contains("downgrades are not supported"), "got: {err:?}");

    // Uninstall/re-install (review must-fix): uninstall keeps the ledger row flagged with the
    // DATA's version; re-installing at the linked version must NOT overwrite it, so the pending
    // steps replay against the kept old-shape data instead of being skipped silently.
    sqlx::query("TRUNCATE upgrade_probe").execute(db.pool()).await.unwrap();
    db.mark_module_installed("upgtest", "1.0.0").await.unwrap();
    db.mark_module_uninstalled("upgtest").await.unwrap();
    assert!(!db.is_module_installed("upgtest").await.unwrap());
    assert!(db.run_pending_upgrades().await.unwrap().is_empty(), "uninstalled modules never migrate");
    db.mark_module_installed("upgtest", "1.2.0").await.unwrap(); // what a re-install records
    assert_eq!(ledger_version(db, "upgtest").await, "1.0.0", "re-install keeps the data's version");
    let applied = db.run_pending_upgrades().await.unwrap();
    assert_eq!(applied.len(), 2, "the kept data replays its pending migrations");
    assert_eq!(ledger_version(db, "upgtest").await, "1.2.0");
}
