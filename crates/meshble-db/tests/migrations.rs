//! Versioned migration engine test against a live Postgres. Requires `DATABASE_URL`.

use meshble_core::{resolve, FieldDef, FieldKind, ModelDescriptor, ResolvedModel};
use meshble_db::{Db, Migration, MigrationOutcome};

static MODEL: ModelDescriptor = ModelDescriptor {
    name: "thing",
    table: "thing_mig_test",
    fields: &[FieldDef {
        name: "name", label: "Name", kind: FieldKind::Text,
        required: true, stored: true, compute: None, depends: &[],
    }],
};

fn model() -> ResolvedModel {
    resolve(&MODEL, &[]).unwrap()
}

static MIGRATIONS: &[Migration] = &[Migration {
    version: "1.1.0",
    statements: &[
        "ALTER TABLE thing_mig_test ADD COLUMN note text",
        "UPDATE thing_mig_test SET note = 'migrated'",
    ],
}];

#[tokio::test]
async fn versioned_migrations_apply_idempotently() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let m = model();

    // Clean slate (install_or_upgrade creates the bookkeeping tables; pre-create so DELETE works).
    sqlx::query("CREATE TABLE IF NOT EXISTS meshble_module (name text PRIMARY KEY, version text NOT NULL)")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("CREATE TABLE IF NOT EXISTS meshble_migration (module text NOT NULL, version text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (module, version))")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM meshble_module WHERE name = 'thing'").execute(db.pool()).await.unwrap();
    sqlx::query("DELETE FROM meshble_migration WHERE module = 'thing'").execute(db.pool()).await.unwrap();
    db.drop_table(&m).await.unwrap();

    // Fresh install at 1.0.0: table created from the model, version recorded, no migrations run.
    let r1 = db.install_or_upgrade(&m, "thing", "1.0.0", MIGRATIONS).await.unwrap();
    assert_eq!(r1, MigrationOutcome::Installed);
    sqlx::query("INSERT INTO thing_mig_test (name) VALUES ('x')")
        .execute(db.pool())
        .await
        .unwrap();

    // Upgrade to 1.1.0: runs the one pending migration (adds + fills `note`).
    let r2 = db.install_or_upgrade(&m, "thing", "1.1.0", MIGRATIONS).await.unwrap();
    assert!(matches!(r2, MigrationOutcome::Upgraded { steps: 1, .. }));
    let note: String = sqlx::query_scalar("SELECT note FROM thing_mig_test LIMIT 1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(note, "migrated");

    // Re-running at the same version is an idempotent no-op.
    let r3 = db.install_or_upgrade(&m, "thing", "1.1.0", MIGRATIONS).await.unwrap();
    assert_eq!(r3, MigrationOutcome::UpToDate);

    // Reachability: a target above the highest pending migration is rejected (no version outrun).
    static UNREACHABLE: &[Migration] = &[Migration { version: "2.0.0", statements: &["SELECT 1"] }];
    let r4 = db.install_or_upgrade(&m, "thing", "3.0.0", UNREACHABLE).await;
    assert!(matches!(r4, Err(meshble_db::DbError::Migration(_))));

    // Cleanup.
    db.drop_table(&m).await.unwrap();
    sqlx::query("DELETE FROM meshble_module WHERE name = 'thing'").execute(db.pool()).await.unwrap();
    sqlx::query("DELETE FROM meshble_migration WHERE module = 'thing'").execute(db.pool()).await.unwrap();
}
