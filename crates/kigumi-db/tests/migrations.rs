//! Versioned migration engine test against a live Postgres. Requires `DATABASE_URL`.

use kigumi_core::{resolve, FieldDef, FieldKind, ModelDescriptor, ResolvedModel};
use kigumi_db::{Migration, MigrationOutcome};

static MODEL: ModelDescriptor = ModelDescriptor {
    name: "thing",
    table: "thing_mig_test",
    fields: &[FieldDef {
        name: "name", label: "Name", kind: FieldKind::Text,
        required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
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
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let m = model();

    // The kit's reset left a clean slate (install_or_upgrade creates its own bookkeeping tables).
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
    assert!(matches!(r4, Err(kigumi_db::DbError::Migration(_))));

    // Cleanup.
    db.drop_table(&m).await.unwrap();
}
