//! Integration-test kit: a FAST, correct database reset plus the fixtures every test used to
//! copy-paste (the DATABASE_URL skip, the superuser Ctx, the insert helper).
//!
//! The reset is fingerprinted. A full drop/create of the whole migration plan costs ~minutes per
//! binary; most runs don't change the schema at all. So [`TestDb::new`]:
//!
//! 1. takes a global advisory lock on a DEDICATED connection (concurrent test binaries sharing one
//!    DATABASE_URL serialize instead of clobbering each other; the lock lives until the connection
//!    drops at the end of the test),
//! 2. computes a CODE fingerprint (the DDL projection of every registered model + the M2M junction
//!    shapes + [`KIT_SCHEMA_VERSION`]) and a DATABASE snapshot (columns + indexes from the catalog),
//! 3. if both match what the last full build recorded → `TRUNCATE <everything> RESTART IDENTITY
//!    CASCADE` (milliseconds) and re-run the idempotent `ensure_*` framework DDL (an additive
//!    framework change self-applies here and the stored snapshot is refreshed),
//! 4. otherwise → full drop/create of every table in the schema, then record the new fingerprints.
//!
//! The DB snapshot check also catches schema "dirt" left by DDL-exercising tests (e.g. runtime
//! custom fields ALTERing a table): the snapshot no longer matches, so the next reset rebuilds.
//! Residual manual knob: a DESTRUCTIVE change to framework `ensure_*` DDL (a column retyped in
//! place — additive `IF NOT EXISTS` changes self-apply) needs a [`KIT_SCHEMA_VERSION`] bump.
//!
//! The fingerprint is per test BINARY (it hashes the models linked into it): binaries linking
//! different module sets still rebuild when they alternate — the win is the dev loop (re-running
//! one binary) and consecutive binaries sharing a module set.

use kigumi_core::{migration_plan, Ctx, ResolvedModel};
use kigumi_db::{Db, DbError};
use sqlx::{Connection, PgConnection, Row};

/// Bump when a framework DDL change guarded by `IF NOT EXISTS` cannot self-apply on the TRUNCATE
/// path (destructive shape changes to `ensure_*` tables; junction-template changes are fingerprinted
/// verbatim via `m2m_junction_ddl` and self-detect). Additive `CREATE/ADD ... IF NOT EXISTS` changes
/// do not need a bump.
pub const KIT_SCHEMA_VERSION: u32 = 1;

/// One fixed advisory-lock key for the whole kit ("kigumi-test", spelled in hex).
const LOCK_KEY: i64 = 0x6b69_6775_6d69;

/// The meta table the kit records its fingerprints in (excluded from snapshots and truncation).
const META: &str = "kigumi_test_meta";

/// A connected, reset test database. Holds the advisory lock for the lifetime of the value — drop
/// it (end of test) and the dedicated lock connection closes, releasing the lock server-side.
pub struct TestDb {
    pub db: Db,
    _lock: PgConnection,
}

impl TestDb {
    /// Connects to `DATABASE_URL` and hands back a database reset to a pristine schema. Returns
    /// `None` (after printing the conventional skip line) when the variable is unset, so tests keep
    /// the established "skip without a database" behavior:
    ///
    /// ```ignore
    /// let Some(t) = kigumi_test::TestDb::new().await else { return };
    /// let db = &t.db;
    /// ```
    ///
    /// Real failures (bad URL, SQL errors) panic — a broken test database should fail loudly.
    ///
    /// ONE `TestDb` per test, never nested: the advisory lock is per-session and a second `new()`
    /// inside the same test would wait forever on the first one's lock.
    pub async fn new() -> Option<TestDb> {
        let url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => {
                eprintln!("skipping: DATABASE_URL not set");
                return None;
            }
        };
        // The lock connection is dedicated (NOT from the pool): a session advisory lock is released
        // when its session ends, and a pooled connection outlives the guard by design.
        let mut lock = PgConnection::connect(&url).await.expect("kigumi-test: connect (lock)");
        // try-lock loop so a contended (or wedged) lock is attributable instead of a silent hang.
        loop {
            let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
                .bind(LOCK_KEY)
                .fetch_one(&mut lock)
                .await
                .expect("kigumi-test: advisory lock");
            if got {
                break;
            }
            eprintln!("kigumi-test: waiting for the kit lock (another test binary is resetting)…");
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        let db = Db::connect(&url).await.expect("kigumi-test: connect");
        prepare(&db).await.expect("kigumi-test: prepare schema");
        Some(TestDb { db, _lock: lock })
    }
}

/// The superuser context every test seeds with.
pub fn su() -> Ctx {
    Ctx::new(0, vec![]).sudo()
}

/// The shared insert fixture (the `ins` closure every test used to define): a secured insert as
/// `ctx` with no ACL/rule overlays, unwrapped. Panics on failure — it's a fixture, not the subject.
pub async fn ins(db: &Db, model: &ResolvedModel, ctx: &Ctx, v: serde_json::Value) -> i64 {
    db.insert_secured(model, ctx, &[], &[], v.as_object().expect("ins: JSON object"))
        .await
        .expect("ins: insert_secured")
}

/// Resets the database for this binary's registered model set (see the module doc for the
/// fingerprint strategy). Public so a bespoke harness can drive it without [`TestDb`] — such a
/// caller MUST hold the kit advisory lock ([`TestDb::new`] does) for the duration, or concurrent
/// binaries can interleave with the whole-schema TRUNCATE/DROP below.
pub async fn prepare(db: &Db) -> Result<(), DbError> {
    let plan = migration_plan().map_err(DbError::Migration)?;
    assert!(
        plan.iter().all(|t| t.model.table != META),
        "kigumi-test: a model table is named '{META}', which collides with the kit's meta table"
    );

    sqlx::query(&format!(
        "CREATE TABLE IF NOT EXISTS {META} (id INT PRIMARY KEY DEFAULT 1, code_fp TEXT NOT NULL, schema_snap TEXT NOT NULL)"
    ))
    .execute(db.pool())
    .await?;

    let code_fp = code_fingerprint(db, &plan).await?;
    let stored: Option<(String, String)> = sqlx::query(&format!("SELECT code_fp, schema_snap FROM {META} WHERE id = 1"))
        .fetch_optional(db.pool())
        .await?
        .map(|r| (r.get(0), r.get(1)));
    // First contact with a database the kit has never built: the reset below wipes EVERY public
    // table, which is only safe on a dedicated test database. Require one explicit opt-in.
    if stored.is_none() {
        let existing = public_tables(db, false).await?;
        if !existing.is_empty() && std::env::var("KIGUMI_TEST_ALLOW_RESET").is_err() {
            panic!(
                "kigumi-test: DATABASE_URL points at a non-empty database the kit has never reset \
                 ({} tables). If this IS your dedicated test database, set KIGUMI_TEST_ALLOW_RESET=1 \
                 once; the kit then tracks it via its {META} table.",
                existing.len()
            );
        }
    }
    let snap_now = schema_snapshot(db).await?;

    match stored {
        Some((fp, snap)) if fp == code_fp && snap == snap_now => {
            truncate_all(db).await?;
            ensure_all(db).await?;
            // An additive framework DDL change self-applies through ensure_*; refresh the record.
            let snap_after = schema_snapshot(db).await?;
            if snap_after != snap_now {
                sqlx::query(&format!("UPDATE {META} SET schema_snap = $1 WHERE id = 1"))
                    .bind(&snap_after)
                    .execute(db.pool())
                    .await?;
            }
        }
        _ => {
            drop_all(db).await?; // meta included — recreated below
            for t in &plan {
                db.create_table(&t.model).await?;
            }
            for t in &plan {
                db.create_m2m_relations(&t.model).await?;
            }
            ensure_all(db).await?;
            let snap_after = schema_snapshot(db).await?;
            sqlx::query(&format!(
                "CREATE TABLE IF NOT EXISTS {META} (id INT PRIMARY KEY DEFAULT 1, code_fp TEXT NOT NULL, schema_snap TEXT NOT NULL)"
            ))
            .execute(db.pool())
            .await?;
            sqlx::query(&format!(
                "INSERT INTO {META} (id, code_fp, schema_snap) VALUES (1, $1, $2) \
                 ON CONFLICT (id) DO UPDATE SET code_fp = EXCLUDED.code_fp, schema_snap = EXCLUDED.schema_snap"
            ))
            .bind(&code_fp)
            .bind(&snap_after)
            .execute(db.pool())
            .await?;
        }
    }
    Ok(())
}

/// Every idempotent framework `ensure_*` (schemas + indexes + seeded registries). The index helpers
/// tolerate absent module tables, so the bundle is safe for any linked-module set.
async fn ensure_all(db: &Db) -> Result<(), DbError> {
    db.ensure_sequence_schema().await?;
    db.ensure_event_schema().await?;
    db.ensure_access_schema().await?;
    db.ensure_auth_schema().await?;
    db.ensure_api_key_schema().await?;
    db.ensure_module_schema().await?;
    db.ensure_setting_schema().await?;
    db.ensure_view_schema().await?;
    db.ensure_translation_schema().await?;
    db.ensure_oidc_schema().await?;
    db.ensure_custom_field_schema().await?;
    db.ensure_crons().await?;
    db.ensure_transient_defaults().await?;
    db.ensure_mail_indexes().await?;
    db.ensure_stock_indexes().await?;
    db.ensure_registered_sequences().await?;
    Ok(())
}

/// The code side of the fingerprint: the DDL of every registered model (name-sorted for stability)
/// plus each Many2many junction shape, hashed server-side (md5 — stable, no extra dependency).
async fn code_fingerprint(db: &Db, plan: &[kigumi_core::MigrationTarget]) -> Result<String, DbError> {
    let mut targets: Vec<&kigumi_core::MigrationTarget> = plan.iter().collect();
    targets.sort_by(|a, b| a.model.name.cmp(b.model.name));
    let mut blob = format!("kit:v{KIT_SCHEMA_VERSION}\n");
    for t in &targets {
        blob.push_str(&kigumi_schema::to_ddl(&t.model));
        blob.push('\n');
        // Junction DDL verbatim from the SAME function the migration executes — a template change
        // (FK action, extra column) must change the fingerprint, or IF NOT EXISTS would keep a
        // stale junction alive through the fast path.
        for ddl in kigumi_db::m2m_junction_ddl(&t.model) {
            blob.push_str(&ddl);
            blob.push('\n');
        }
    }
    let fp: String = sqlx::query_scalar("SELECT md5($1)").bind(&blob).fetch_one(db.pool()).await?;
    Ok(fp)
}

/// The database side of the fingerprint: every column and index in the public schema (minus the
/// kit's own meta table), hashed in one round trip. Catches schema dirt (runtime-DDL tests) and
/// verifies a TRUNCATE-only reset is landing on the schema the last full build produced.
async fn schema_snapshot(db: &Db) -> Result<String, DbError> {
    let snap: String = sqlx::query_scalar(&format!(
        "SELECT md5(COALESCE(string_agg(x, '|' ORDER BY x), '')) FROM ( \
             SELECT table_name || '.' || column_name || ':' || data_type || ':' || is_nullable || ':' || COALESCE(column_default, '') AS x \
             FROM information_schema.columns WHERE table_schema = 'public' AND table_name <> '{META}' \
           UNION ALL \
             SELECT indexname || '=' || indexdef FROM pg_indexes \
             WHERE schemaname = 'public' AND tablename <> '{META}' \
           UNION ALL \
             SELECT conrelid::regclass::text || '#' || conname || ':' || pg_get_constraintdef(oid) \
             FROM pg_constraint \
             WHERE connamespace = 'public'::regnamespace AND conrelid::regclass::text <> '{META}' \
         ) s"
    ))
    .fetch_one(db.pool())
    .await?;
    Ok(snap)
}

/// Every table in the public schema except the kit's meta table and extension-owned relations
/// (e.g. PostGIS's spatial_ref_sys — truncating extension data breaks the extension, and dropping
/// it errors outright).
async fn public_tables(db: &Db, include_meta: bool) -> Result<Vec<String>, DbError> {
    let rows = sqlx::query(
        "SELECT t.tablename FROM pg_tables t \
         WHERE t.schemaname = 'public' AND NOT EXISTS ( \
             SELECT 1 FROM pg_depend d \
             JOIN pg_class c ON c.oid = d.objid \
             WHERE d.deptype = 'e' AND c.relname = t.tablename AND c.relnamespace = 'public'::regnamespace)",
    )
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .iter()
        .map(|r| r.get::<String, _>(0))
        .filter(|t| include_meta || t != META)
        .collect())
}

/// One `TRUNCATE t1, t2, … RESTART IDENTITY CASCADE` over the whole schema: BIGSERIAL counters
/// restart (tests assert concrete ids and sequence numbers) and FK order is irrelevant.
async fn truncate_all(db: &Db) -> Result<(), DbError> {
    let tables = public_tables(db, false).await?;
    if tables.is_empty() {
        return Ok(());
    }
    let list = tables.iter().map(|t| quoted(t)).collect::<Vec<_>>().join(", ");
    sqlx::query(&format!("TRUNCATE {list} RESTART IDENTITY CASCADE")).execute(db.pool()).await?;
    Ok(())
}

/// Drops every table in the public schema (meta included) — the full-rebuild path.
async fn drop_all(db: &Db) -> Result<(), DbError> {
    let tables = public_tables(db, true).await?;
    if tables.is_empty() {
        return Ok(());
    }
    let list = tables.iter().map(|t| quoted(t)).collect::<Vec<_>>().join(", ");
    sqlx::query(&format!("DROP TABLE IF EXISTS {list} CASCADE")).execute(db.pool()).await?;
    Ok(())
}

/// Quotes a catalog identifier, doubling embedded quotes (robustness — the framework never
/// generates such names, but the catalog could hold one).
fn quoted(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}
