//! The adopter runtime: what a third-party binary previously had to copy from kigumi-cli by hand
//! (the dogfood counted ~100 lines — the ensure chain in folklore order, the install ledger, the
//! admin bootstrap, the worker ticks, the router incantations). An adopter's main is now:
//!
//! ```ignore
//! let db = Db::connect(&url).await?;
//! kigumi_runtime::migrate(&db).await?;
//! kigumi_runtime::bootstrap_admin(&db, &password).await?;
//! kigumi_runtime::serve(db, ServeOptions { bind, jwt_secret, blob_root }).await?;
//! ```
//!
//! kigumi-cli predates this crate and keeps its own dynamic wiring (live install, runtime custom
//! fields/views, SPA, PDF): this is the STATIC-catalog runtime — the linked modules are the served
//! catalog, a restart picks up changes. Adopters who outgrow it graduate to the CLI's approach.

use kigumi_core::{registered_acls, registered_group_names, registered_rules, resolve_all_registered, resolve_modules, Acl, Ctx, RecordRule};
use kigumi_db::{Db, DbError};
use std::sync::Arc;

/// How often the workers observe due work (the claims themselves are SKIP LOCKED — several
/// processes are safe). Crons carry their own per-job interval in the DB; this only bounds
/// observation promptness.
const CRON_TICK_SECS: u64 = 60;
const JOB_TICK_SECS: u64 = 5;

fn migration_err<E: std::fmt::Debug>(e: E) -> DbError {
    DbError::Migration(format!("{e:?}"))
}

/// Ensures every framework schema (auth, jobs, sequences, settings, access, modules, custom
/// fields, views, events) — the canonical order previously known only to kigumi-cli's source.
pub async fn ensure_framework_schemas(db: &Db) -> Result<(), DbError> {
    db.ensure_auth_schema().await?;
    db.ensure_job_schema().await?;
    db.ensure_sequence_schema().await?;
    db.ensure_setting_schema().await?;
    db.ensure_access_schema().await?;
    db.ensure_module_schema().await?;
    db.ensure_custom_field_schema().await?;
    db.ensure_view_schema().await?;
    db.ensure_event_schema().await?;
    Ok(())
}

/// Full migrate: framework schemas, ledger reconciliation, then schema migration + registered
/// sequences + pending data migrations + module seeds via `migrate_installed_schema`.
///
/// Reconciliation: a linked module the ledger has NEVER seen is installed (so a release that ADDS
/// a module just works — an empty-ledger-only check would drop it silently). A module the
/// operator explicitly uninstalled stays uninstalled, and a ledger row whose crate is no longer
/// linked is left untouched.
pub async fn migrate(db: &Db) -> Result<(), DbError> {
    ensure_framework_schemas(db).await?;
    let known: std::collections::HashSet<String> = db.ledger_modules().await?.into_iter().collect();
    let mods = resolve_modules().map_err(migration_err)?;
    let newly: Vec<&str> = mods.iter().filter(|m| !known.contains(m.name)).map(|m| m.name).collect();
    for m in mods.iter().filter(|m| newly.contains(&m.name)) {
        db.mark_module_installed(m.name, m.version).await?;
    }
    if !newly.is_empty() {
        println!("installed modules: {}", newly.join(", "));
    }
    db.migrate_installed_schema().await?;
    Ok(())
}

/// Bootstraps an `admin` user holding every group any linked module declares (plus `user`/`admin`)
/// and scoped to every existing company, exactly like kigumi-cli. Returns false when an admin
/// already exists (never touches it).
pub async fn bootstrap_admin(db: &Db, password: &str) -> Result<bool, DbError> {
    if db.find_user("admin").await?.is_some() {
        return Ok(false);
    }
    // The CLI's guard, kept here too (review must-fix): an empty password — the classic
    // unwrap_or_default() on an unset env var — must never become an authenticable superuser.
    if password.is_empty() {
        return Err(DbError::BadInput("refusing to bootstrap admin with an empty password".to_string()));
    }
    let hash = kigumi_auth::hash_password(password).map_err(migration_err)?;
    let mut groups = registered_group_names();
    for g in ["user", "admin"] {
        if !groups.iter().any(|x| x == g) {
            groups.push(g.to_string());
        }
    }
    let refs: Vec<&str> = groups.iter().map(String::as_str).collect();
    db.upsert_user("admin", &hash, &refs).await?;
    if let Ok(company) = kigumi_core::resolve_registered("res.company") {
        let su = Ctx::new(0, vec![]).sudo();
        let ids = db.find_ids_secured(&company, &su, &[], &[], None).await?;
        if let Some(&first) = ids.first() {
            db.set_user_companies("admin", Some(first), &ids).await?;
        }
    }
    Ok(true)
}

/// Spawns the background workers: the cron scheduler and the ad-hoc job runner (reap expired
/// leases, then claim + run due jobs). Call once per process; safe with multiple processes.
pub fn spawn_workers(db: &Db) {
    let cron_db = db.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(CRON_TICK_SECS)).await;
            if let Err(e) = cron_db.run_due_crons().await {
                eprintln!("kigumi cron tick failed: {e:?}");
            }
        }
    });
    let job_db = db.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(JOB_TICK_SECS)).await;
            if let Err(e) = job_db.reap_stuck_jobs().await {
                eprintln!("kigumi job reap failed: {e:?}");
            }
            if let Err(e) = job_db.run_due_jobs().await {
                eprintln!("kigumi job tick failed: {e:?}");
            }
        }
    });
}

pub struct ServeOptions {
    /// e.g. "127.0.0.1:8600".
    pub bind: String,
    pub jwt_secret: String,
    /// Root directory of the filesystem blob store.
    pub blob_root: std::path::PathBuf,
}

/// Serves the full secured API (data, auth, actions/services, module routes, reports, SSE,
/// OpenAPI/contract) for the linked catalog, with the background workers running. Blocks forever.
///
/// STATIC catalog: the served model set is fixed at startup from the linked modules. An
/// out-of-band ledger change (e.g. an uninstall issued by another process or by SQL) does NOT
/// stop this process serving those models until it restarts — unlike kigumi-cli's serve, which
/// re-reads the ledger live. If that matters operationally, restart after ledger changes or
/// graduate to the CLI's dynamic wiring.
pub async fn serve(db: Db, opts: ServeOptions) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let models: Vec<_> = resolve_all_registered().map_err(|e| e.to_string())?.into_iter().collect();
    // The static router wants 'static security data; the registry Vecs live for the process anyway.
    let acls: &'static [Acl] = Box::leak(registered_acls().into_boxed_slice());
    let rules: &'static [RecordRule] = Box::leak(registered_rules().into_boxed_slice());
    let blobs: Arc<dyn kigumi_storage::BlobStore> = Arc::new(kigumi_storage::FsBlobStore::new(opts.blob_root));
    spawn_workers(&db);
    let app = kigumi_server::router_with_data(models, db, acls, rules, opts.jwt_secret, blobs);
    let listener = tokio::net::TcpListener::bind(&opts.bind).await?;
    println!("kigumi serving on http://{}", opts.bind);
    axum::serve(listener, app).await?;
    Ok(())
}
