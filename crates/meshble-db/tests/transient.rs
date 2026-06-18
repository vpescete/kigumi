//! Transient (wizard) subsystem, storage half: `ensure_transient_defaults` gives a transient model's
//! `create_date` a `DEFAULT now()` so EVERY insert path stamps it, and `sweep_transient_records` (the
//! body of the `gc_transient_records` cron) reclaims rows older than the TTL while keeping fresh ones.
//! Synthetic transient model under the engine's exact registries. Live Postgres.

use meshble_core::{FieldDef, FieldKind, ModelDescriptor, ModelRegistration, TransientRegistration};
use meshble_db::Db;

const fn txt(n: &'static str) -> FieldDef { FieldDef { name: n, label: n, kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None } }
const fn dtm(n: &'static str) -> FieldDef { FieldDef { name: n, label: n, kind: FieldKind::Datetime, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None } }

static WIZARD: ModelDescriptor =
    ModelDescriptor { name: "test.wizard", table: "test_wizard", fields: &[txt("name"), dtm("create_date")] };
fn dw() -> &'static ModelDescriptor { &WIZARD }
meshble_core::inventory::submit! { ModelRegistration { name: "test.wizard", module: "test", descriptor: dw } }
meshble_core::inventory::submit! { TransientRegistration { model: "test.wizard" } }

#[tokio::test]
async fn transient_create_date_defaults_and_gc_reclaims_old_rows() {
    let url = match std::env::var("DATABASE_URL") { Ok(u) => u, Err(_) => { eprintln!("skipping"); return; } };
    let db = Db::connect(&url).await.unwrap();
    let m = meshble_core::resolve(&WIZARD, &[]).unwrap();
    db.drop_table(&m).await.unwrap();
    db.create_table(&m).await.unwrap();

    // Migration step: give create_date a DEFAULT now().
    db.ensure_transient_defaults().await.unwrap();

    // A raw insert that OMITS create_date must still get it stamped by the column default.
    sqlx::query("INSERT INTO test_wizard (name) VALUES ('fresh')").execute(db.pool()).await.unwrap();
    let null_dates: i64 =
        sqlx::query_scalar("SELECT count(*) FROM test_wizard WHERE create_date IS NULL").fetch_one(db.pool()).await.unwrap();
    assert_eq!(null_dates, 0, "create_date default must populate on an insert that omits it");

    // Plant an aged row, then run the GC sweep (the cron's body).
    sqlx::query("INSERT INTO test_wizard (name, create_date) VALUES ('old', now() - interval '2 hours')")
        .execute(db.pool()).await.unwrap();
    db.sweep_transient_records().await.unwrap();

    let old_left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM test_wizard WHERE name = 'old'").fetch_one(db.pool()).await.unwrap();
    let fresh_left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM test_wizard WHERE name = 'fresh'").fetch_one(db.pool()).await.unwrap();
    assert_eq!(old_left, 0, "GC must reclaim a transient row older than the TTL");
    assert_eq!(fresh_left, 1, "GC must keep a fresh transient row");

    db.drop_table(&m).await.unwrap();
}
