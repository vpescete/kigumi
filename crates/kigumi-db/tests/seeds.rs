//! The register_seed! seam: seeds run only for INSTALLED modules, in dependency order (a module
//! seeds after what it depends on), on every migrate — bodies are idempotent by contract, so a
//! re-run adds nothing. Requires DATABASE_URL.

use kigumi_core::{ModuleDep, ModuleManifest, ModuleRegistration};
use kigumi_db::{Db, DbError, SeedRegistration};
use std::future::Future;
use std::pin::Pin;

type Fut<'a> = Pin<Box<dyn Future<Output = Result<(), DbError>> + Send + 'a>>;

static MANIFEST_A: ModuleManifest = ModuleManifest {
    name: "seedtest_a",
    version: "1.0.0",
    framework: ">=0.1, <0.2",
    depends: &[ModuleDep { name: "seedtest_b", req: "^1.0" }],
    summary: "seed-order test module A (depends on B)",
};
static MANIFEST_B: ModuleManifest = ModuleManifest {
    name: "seedtest_b",
    version: "1.0.0",
    framework: ">=0.1, <0.2",
    depends: &[],
    summary: "seed-order test module B",
};
kigumi_core::inventory::submit! { ModuleRegistration { manifest: || MANIFEST_A, crate_path: ::core::module_path!() } }
kigumi_core::inventory::submit! { ModuleRegistration { manifest: || MANIFEST_B, crate_path: ::core::module_path!() } }

/// Guarded probe insert — the idempotency contract every seed body must honor.
async fn probe(db: &Db, name: &str) -> Result<(), DbError> {
    sqlx::query("INSERT INTO seed_probe (name) VALUES ($1) ON CONFLICT (name) DO NOTHING")
        .bind(name)
        .execute(db.pool())
        .await?;
    Ok(())
}

fn seed_a(db: &Db) -> Fut<'_> {
    Box::pin(probe(db, "a"))
}
fn seed_b(db: &Db) -> Fut<'_> {
    Box::pin(probe(db, "b"))
}
kigumi_core::inventory::submit! { SeedRegistration { module: "seedtest_a", func: seed_a } }
kigumi_core::inventory::submit! { SeedRegistration { module: "seedtest_b", func: seed_b } }

#[tokio::test]
async fn seeds_run_for_installed_modules_in_dependency_order() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    sqlx::query("CREATE TABLE IF NOT EXISTS seed_probe (id BIGSERIAL, name TEXT PRIMARY KEY)")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("TRUNCATE seed_probe").execute(db.pool()).await.unwrap();

    async fn rows(db: &Db) -> Vec<String> {
        sqlx::query_scalar("SELECT name FROM seed_probe ORDER BY id")
            .fetch_all(db.pool())
            .await
            .unwrap()
    }

    // Nothing installed → nothing seeded.
    db.run_installed_seeds().await.unwrap();
    assert!(rows(db).await.is_empty(), "a not-installed module never seeds");

    // Both installed → both seed, B (the dependency) strictly before A.
    db.mark_module_installed("seedtest_a", "1.0.0").await.unwrap();
    db.mark_module_installed("seedtest_b", "1.0.0").await.unwrap();
    db.run_installed_seeds().await.unwrap();
    assert_eq!(rows(db).await, vec!["b".to_string(), "a".to_string()], "dependency seeds first");

    // Re-run (every later migrate): guarded bodies add nothing.
    db.run_installed_seeds().await.unwrap();
    assert_eq!(rows(db).await.len(), 2, "idempotent re-run");
}
