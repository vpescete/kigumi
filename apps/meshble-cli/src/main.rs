//! `meshble` — the single command to operate an instance: migrate the catalog, serve the secured
//! API, inspect config, and manage users. Wires the existing crates together driven by
//! `meshble-config` (DATABASE_URL / JWT secret / bind), so nothing is hardcoded.
//!
//! The linked modules (`meshble-mod-base`, `meshble-mod-mail`, `meshble-mod-sales`) self-register
//! their models, ACLs, and record rules into the catalog via `inventory`; this binary just
//! collects and serves them.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use meshble::prelude::*;
use meshble_auth::hash_password;
use meshble_config::Settings;
use meshble_db::Db;
use meshble_server::{refresh_custom_fields, router_with_data_dynamic};

type Fallible = Result<(), Box<dyn std::error::Error>>;

/// How often the scheduler checks for due cron jobs (each job's own interval lives in the DB).
const CRON_TICK_SECS: u64 = 60;
/// How often a running server re-reads the install ledger to pick up out-of-band module changes.
const MODULE_REFRESH_SECS: u64 = 8;

/// Forces the module crates to link so their `inventory` registrations are present in this binary.
fn link_modules() {
    let _ = (
        &meshble_mod_base::MANIFEST,
        &meshble_mod_mail::MANIFEST,
        &meshble_mod_sales::MANIFEST,
        &meshble_mod_account::MANIFEST,
        &meshble_mod_stock::MANIFEST,
    );
}

#[derive(Parser)]
#[command(name = "meshble", version, about = "Meshble instance CLI")]
struct Cli {
    /// Path to meshble.toml (default: $MESHBLE_CONFIG or ./meshble.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Migrate the catalog + auth schema, bootstrap an admin from env, then serve the secured API.
    Serve,
    /// Migrate all linked modules + the auth schema, then exit.
    Migrate,
    /// Inspect configuration.
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// Manage users.
    User {
        #[command(subcommand)]
        action: UserCmd,
    },
    /// Manage runtime ACL overrides (additive on top of the compiled-in baseline).
    Acl {
        #[command(subcommand)]
        action: AclCmd,
    },
    /// Manage runtime record rules (additive on top of the compiled-in baseline).
    Rule {
        #[command(subcommand)]
        action: RuleCmd,
    },
    /// Install/uninstall modules (only modules whose crate is linked into this binary are available).
    Module {
        #[command(subcommand)]
        action: ModuleCmd,
    },
    /// Print the framework version and the linked modules.
    Version,
}

#[derive(Subcommand)]
enum ModuleCmd {
    /// List the available (linked) modules and whether each is installed.
    List,
    /// Install a module and its dependency closure, then migrate their tables.
    Install { name: String },
    /// Uninstall a module (it stops being migrated/served; its tables and data are KEPT).
    Uninstall { name: String },
}

#[derive(Subcommand)]
enum RuleCmd {
    /// Add a runtime record rule. --groups is a CSV (empty = global); --ops a CSV of r/w/c/d;
    /// --domain the portable JSON AST, e.g. '{"field":"state","op":"!=","value":"done"}'.
    Add {
        model: String,
        #[arg(long, default_value = "")]
        groups: String,
        #[arg(long, default_value = "r")]
        ops: String,
        #[arg(long)]
        domain: String,
    },
    /// Remove a runtime record rule by id (the static baseline is unaffected).
    Remove { id: i64 },
    /// List the runtime DB record rules.
    List,
}

#[derive(Subcommand)]
enum AclCmd {
    /// Grant (or update) a runtime ACL for a group on a model. Flags pick the operations.
    Grant {
        model: String,
        group: String,
        #[arg(long)]
        read: bool,
        #[arg(long)]
        write: bool,
        #[arg(long)]
        create: bool,
        #[arg(long)]
        delete: bool,
    },
    /// Remove a runtime ACL override for a group on a model (the static baseline is unaffected).
    Revoke { model: String, group: String },
    /// List the effective ACLs: the compiled-in baseline + the runtime DB overrides.
    List,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Validate the effective configuration.
    Check,
    /// Print the effective configuration (secrets redacted) + runtime settings from the DB.
    Print,
    /// Set a runtime setting (stored in the DB — the authority for runtime keys).
    Set {
        key: String,
        value: String,
        #[arg(long, default_value = "string")]
        vtype: String,
    },
    /// Get a runtime setting's value.
    Get { key: String },
}

#[derive(Subcommand)]
enum UserCmd {
    /// Create or replace a user. Password via --password (dev) or $MESHBLE_NEW_PASSWORD.
    Create {
        login: String,
        #[arg(long)]
        password: Option<String>,
        #[arg(long, default_value = "user")]
        groups: String,
    },
    /// Reset a user's password (keeps their groups).
    SetPassword {
        login: String,
        #[arg(long)]
        password: Option<String>,
    },
    /// Add a group to a user.
    Grant { login: String, group: String },
    /// Assign a user's multi-company scope: --active <id> is the default company, --allowed a CSV
    /// of accessible company ids (the active company is always included). Empty = unrestricted.
    Company {
        login: String,
        #[arg(long)]
        active: Option<i64>,
        #[arg(long, default_value = "")]
        allowed: String,
    },
}

fn config_path(cli: &Cli) -> PathBuf {
    cli.config
        .clone()
        .or_else(|| std::env::var("MESHBLE_CONFIG").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("meshble.toml"))
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Fallible {
    link_modules();
    let path = config_path(&cli);
    match cli.cmd {
        Cmd::Config { action } => {
            let s = Settings::load(Some(&path))?;
            match action {
                ConfigCmd::Check => println!("ok: configuration is valid"),
                ConfigCmd::Print => {
                    print!("{}", s.redacted());
                    let db = Db::connect(&s.secrets.database_url).await?;
                    db.ensure_setting_schema().await?;
                    println!("[runtime settings (DB)]");
                    for (k, v, t) in db.all_settings().await? {
                        println!("  {k} = {v}  ({t})");
                    }
                }
                ConfigCmd::Set { key, value, vtype } => {
                    let db = Db::connect(&s.secrets.database_url).await?;
                    db.ensure_setting_schema().await?;
                    db.set_setting(&key, &value, &vtype).await?;
                    println!("set {key} = {value}");
                }
                ConfigCmd::Get { key } => {
                    let db = Db::connect(&s.secrets.database_url).await?;
                    db.ensure_setting_schema().await?;
                    match db.get_setting(&key).await? {
                        Some(v) => println!("{v}"),
                        None => eprintln!("(unset)"),
                    }
                }
            }
            Ok(())
        }
        Cmd::Version => {
            println!("meshble framework {FRAMEWORK_VERSION}");
            for m in resolve_modules().map_err(|e| format!("{e:?}"))? {
                println!("  module {} {}", m.name, m.version);
            }
            Ok(())
        }
        Cmd::Migrate => {
            let s = Settings::load(Some(&path))?;
            let db = Db::connect(&s.secrets.database_url).await?;
            migrate(&db).await
        }
        Cmd::User { action } => {
            let s = Settings::load(Some(&path))?;
            let db = Db::connect(&s.secrets.database_url).await?;
            db.ensure_auth_schema().await?;
            user_command(&db, action).await
        }
        Cmd::Acl { action } => {
            let s = Settings::load(Some(&path))?;
            let db = Db::connect(&s.secrets.database_url).await?;
            db.ensure_access_schema().await?;
            acl_command(&db, action).await
        }
        Cmd::Rule { action } => {
            let s = Settings::load(Some(&path))?;
            let db = Db::connect(&s.secrets.database_url).await?;
            db.ensure_access_schema().await?;
            rule_command(&db, action).await
        }
        Cmd::Module { action } => {
            let s = Settings::load(Some(&path))?;
            let db = Db::connect(&s.secrets.database_url).await?;
            db.ensure_module_schema().await?;
            module_command(&db, action).await
        }
        Cmd::Serve => {
            let s = Settings::load(Some(&path))?;
            let db = Db::connect(&s.secrets.database_url).await?;
            migrate(&db).await?;
            bootstrap_admin(&db).await?;
            serve(s).await
        }
    }
}

/// Ensures the framework schemas, then migrates the models of the INSTALLED modules (not every linked
/// crate). On a fresh database nothing is installed yet, so `base` (and its dependency closure) is
/// installed first — the rest is opt-in via `meshble module install <name>`, like Odoo's selective
/// install rather than installing everything available.
async fn migrate(db: &Db) -> Fallible {
    db.ensure_auth_schema().await?;
    db.ensure_sequence_schema().await?;
    db.ensure_setting_schema().await?;
    db.ensure_access_schema().await?;
    db.ensure_module_schema().await?;
    // Install-time runtime defaults (DB is the authority; never overwrites an operator change).
    db.seed_setting("base_url", "", "string").await?;
    db.seed_setting("mode", "production", "string").await?;

    // Seed the installed set if it is empty. A truly-fresh DB gets only `base` (+ its closure); a DB
    // migrated BEFORE module-selection existed (its per-model ledger has rows) keeps ALL modules it
    // already had, so upgrading does not silently hide previously-available models.
    if db.installed_modules().await?.is_empty() {
        let mods = resolve_modules().map_err(|e| format!("{e:?}"))?;
        let want: Vec<&str> = if db.has_prior_migration().await? {
            mods.iter().map(|m| m.name).collect()
        } else {
            module_closure("base").map_err(|e| e.to_string())?
        };
        for m in mods.iter().filter(|m| want.contains(&m.name)) {
            db.mark_module_installed(m.name, m.version).await?;
        }
        println!("installed modules: {}", want.join(", "));
    }

    migrate_installed(db).await
}

/// Migrates the models that belong to currently-installed modules, in FK-dependency order, then
/// seeds base reference data if `base` is installed.
async fn migrate_installed(db: &Db) -> Fallible {
    // Schema migration (tables, M2M junctions, framework indexes, cron ledger) is the reusable core,
    // shared with the server's live install. The CLI then layers reference-data seeding on top.
    db.migrate_installed_schema().await?;
    let installed = db.installed_modules().await?;
    if installed.iter().any(|m| m == "base") {
        seed_base_data(db).await?;
    }
    if installed.iter().any(|m| m == "account") {
        seed_account_data(db).await?;
    }
    if installed.iter().any(|m| m == "stock") {
        seed_stock_data(db).await?;
    }
    Ok(())
}

/// Seeds a default warehouse + the standard locations (Stock / Vendors / Customers / Inventory) for the
/// default company when `stock` is installed, so receipts and deliveries have somewhere to move stock.
/// Idempotent: only an empty set of locations is seeded.
async fn seed_stock_data(db: &Db) -> Fallible {
    let location = match resolve_registered("stock.location") {
        Ok(m) => m,
        Err(_) => return Ok(()), // stock module not linked
    };
    let warehouse = resolve_registered("stock.warehouse").map_err(|e| e.to_string())?;
    let company = resolve_registered("res.company").map_err(|e| e.to_string())?;
    let su = Ctx::new(0, vec![]).sudo();

    if db.count_secured(&location, &su, &[], &[], None).await? > 0 {
        return Ok(());
    }
    let Some(&comp_id) = db.find_ids_secured(&company, &su, &[], &[], None).await?.first() else {
        return Ok(());
    };

    let loc = |name: &str, usage: &str| {
        serde_json::json!({ "name": name, "usage": usage, "company_id": comp_id, "active": true })
    };
    let new_location = |v: serde_json::Value| {
        let location = &location;
        let su = &su;
        async move { db.insert_secured(location, su, &[], &[], v.as_object().unwrap()).await }
    };
    let stock = new_location(loc("Stock", "internal")).await?;
    new_location(loc("Vendors", "supplier")).await?;
    new_location(loc("Customers", "customer")).await?;
    new_location(loc("Inventory adjustment", "inventory")).await?;

    db.insert_secured(
        &warehouse,
        &su,
        &[],
        &[],
        serde_json::json!({ "name": "Main Warehouse", "code": "WH", "location_id": stock, "company_id": comp_id, "active": true })
            .as_object()
            .unwrap(),
    )
    .await?;
    println!("seeded default warehouse + stock locations");
    Ok(())
}

/// Seeds a minimal chart of accounts + journals for the default company when `account` is installed, so
/// the invoicing path (M16.4) finds a receivable / income / tax account and a sale journal out of the
/// box. Idempotent: only an empty chart is seeded.
async fn seed_account_data(db: &Db) -> Fallible {
    let account = match resolve_registered("account.account") {
        Ok(m) => m,
        Err(_) => return Ok(()), // account module not linked → nothing to seed
    };
    let journal = resolve_registered("account.journal").map_err(|e| e.to_string())?;
    let company = resolve_registered("res.company").map_err(|e| e.to_string())?;
    let su = Ctx::new(0, vec![]).sudo();

    if db.count_secured(&account, &su, &[], &[], None).await? > 0 {
        return Ok(()); // already has a chart
    }
    let Some(&comp_id) = db.find_ids_secured(&company, &su, &[], &[], None).await?.first() else {
        return Ok(()); // no company to scope the chart to yet
    };

    let acc = |code: &str, name: &str, atype: &str, reconcile: bool| {
        serde_json::json!({ "code": code, "name": name, "account_type": atype, "reconcile": reconcile, "company_id": comp_id, "active": true })
    };
    let mut new_account = |v: serde_json::Value| {
        let account = &account;
        let su = &su;
        async move { db.insert_secured(account, su, &[], &[], v.as_object().unwrap()).await }
    };
    let income = new_account(acc("400000", "Product Sales", "income", false)).await?;
    let expense = new_account(acc("600000", "Expenses", "expense", false)).await?;
    let bank = new_account(acc("101000", "Bank", "bank_cash", false)).await?;
    new_account(acc("121000", "Account Receivable", "receivable", true)).await?;
    new_account(acc("211000", "Account Payable", "payable", true)).await?;
    new_account(acc("251000", "Tax Received", "tax", false)).await?;

    let mut new_journal = |v: serde_json::Value| {
        let journal = &journal;
        let su = &su;
        async move { db.insert_secured(journal, su, &[], &[], v.as_object().unwrap()).await }
    };
    new_journal(serde_json::json!({ "name": "Customer Invoices", "code": "INV", "journal_type": "sale", "sequence_code": "INV", "default_account_id": income, "company_id": comp_id, "active": true })).await?;
    new_journal(serde_json::json!({ "name": "Vendor Bills", "code": "BILL", "journal_type": "purchase", "sequence_code": "BILL", "default_account_id": expense, "company_id": comp_id, "active": true })).await?;
    new_journal(serde_json::json!({ "name": "Bank", "code": "BNK", "journal_type": "bank", "sequence_code": "BNK", "default_account_id": bank, "company_id": comp_id, "active": true })).await?;
    new_journal(serde_json::json!({ "name": "Miscellaneous", "code": "MISC", "journal_type": "general", "sequence_code": "MISC", "company_id": comp_id, "active": true })).await?;
    println!("seeded chart of accounts + journals");
    Ok(())
}

/// Seeds one default currency + company on a fresh instance (multi-company needs a company to exist).
async fn seed_base_data(db: &Db) -> Fallible {
    let currency = match resolve_registered("res.currency") {
        Ok(m) => m,
        Err(_) => return Ok(()), // base module not linked → nothing to seed
    };
    let company = resolve_registered("res.company").map_err(|e| e.to_string())?;
    let su = Ctx::new(0, vec![]).sudo();

    // Document-numbering sequences used by registered actions (e.g. sale.order confirm → SO/00001,
    // purchase.order confirm → PO/00001).
    db.ensure_sequence("SO", "SO/", "", 5).await?;
    db.ensure_sequence("PO", "PO/", "", 5).await?;

    let cur_id = if db.count_secured(&currency, &su, &[], &[], None).await? == 0 {
        let v = serde_json::json!({
            "name": "Euro", "code": "EUR", "symbol": "€",
            "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
        });
        db.insert_secured(&currency, &su, &[], &[], v.as_object().unwrap()).await?
    } else {
        db.find_ids_secured(&currency, &su, &[], &[], None).await?[0]
    };

    if db.count_secured(&company, &su, &[], &[], None).await? == 0 {
        let v = serde_json::json!({ "name": "Main Company", "currency_id": cur_id, "active": true });
        db.insert_secured(&company, &su, &[], &[], v.as_object().unwrap()).await?;
        println!("seeded default company + currency");
    }

    // Seed the read-only res.groups list from every group referenced by registered ACLs/rules
    // (idempotent: insert only the ones not already present).
    if let Ok(groups) = resolve_registered("res.groups") {
        for name in registered_group_names() {
            let by_name = Domain::field("name").eq(name.as_str());
            let exists = db.count_secured(&groups, &su, &[], &[], Some(&by_name)).await? > 0;
            if !exists {
                let v = serde_json::json!({ "name": name });
                db.insert_secured(&groups, &su, &[], &[], v.as_object().unwrap()).await?;
            }
        }
    }
    Ok(())
}

/// Bootstraps an `admin` user from MESHBLE_ADMIN_PASSWORD if none exists (never hardcodes a password).
async fn bootstrap_admin(db: &Db) -> Fallible {
    if db.find_user("admin").await?.is_some() {
        return Ok(());
    }
    match std::env::var("MESHBLE_ADMIN_PASSWORD") {
        Ok(p) if !p.is_empty() => {
            // The admin holds every group any linked module declares (via ACLs/rules) plus the base
            // `user`/`admin`, so a freshly bootstrapped instance can operate every module — no per-module
            // edit here when a new module (e.g. account) introduces its own groups.
            let mut groups = registered_group_names();
            for g in ["user", "admin"] {
                if !groups.iter().any(|x| x == g) {
                    groups.push(g.to_string());
                }
            }
            let group_refs: Vec<&str> = groups.iter().map(String::as_str).collect();
            db.upsert_user("admin", &hash_password(&p)?, &group_refs).await?;
            // M7 default-deny: a user with no company sees only shared rows. Assign the admin to every
            // existing company so it can operate. (Companies created later must be granted explicitly —
            // Odoo behaves the same; only the superuser bypasses company scoping.)
            if let Ok(company) = resolve_registered("res.company") {
                let su = Ctx::new(0, vec![]).sudo();
                let ids = db.find_ids_secured(&company, &su, &[], &[], None).await?;
                if let Some(&first) = ids.first() {
                    db.set_user_companies("admin", Some(first), &ids).await?;
                }
            }
            println!("bootstrapped admin user");
        }
        _ => eprintln!("warning: no admin user; set MESHBLE_ADMIN_PASSWORD to bootstrap one"),
    }
    Ok(())
}

async fn serve(s: Settings) -> Fallible {
    let bind = s.config.server.bind.clone();
    let db = Db::connect(&s.secrets.database_url).await?;

    // The router is handed the FULL linked catalog; a LIVE installed set (below) gates which models are
    // actually served, per request — so installing/uninstalling a module from the UI takes effect
    // without restarting the process (approach B, but dynamic like Odoo's registry rather than fixed at
    // startup). A model whose owning module is not installed is simply not served until it is.
    db.ensure_module_schema().await?;
    db.ensure_custom_field_schema().await?;
    let models: Vec<_> = resolve_all_registered().map_err(|e| e.to_string())?.into_iter().collect();
    let installed_set: std::sync::Arc<std::sync::RwLock<std::collections::HashSet<String>>> =
        std::sync::Arc::new(std::sync::RwLock::new(db.installed_modules().await?.into_iter().collect()));
    // Runtime custom fields: the declarative-extension layer, loaded live and merged into models on
    // resolve — a field added via /api/<model>/_fields appears with no restart.
    let custom_fields: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<String, Vec<meshble::prelude::FieldDef>>>> =
        std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()));
    refresh_custom_fields(&custom_fields, &db).await;

    // Effective access = compiled-in baseline ∪ runtime DB overrides (hybrid, D12). For ACLs the DB
    // rows only widen access (union); for record rules they add restrictions/alternatives through the
    // same engine — either way the static baseline stays in force. Collected once at startup and
    // given the process lifetime (the server holds them for `'static`).
    db.ensure_access_schema().await?;
    let mut all_acls = registered_acls();
    let db_acls = db.load_acls_static().await?;
    let db_acl_count = db_acls.len();
    all_acls.extend(db_acls);
    let acls: &'static [Acl] = Box::leak(all_acls.into_boxed_slice());

    let mut all_rules = registered_rules();
    let db_rules = db.load_rules_static().await?;
    let db_rule_count = db_rules.len();
    all_rules.extend(db_rules);
    let rules: &'static [RecordRule] = Box::leak(all_rules.into_boxed_slice());

    if db_acl_count > 0 || db_rule_count > 0 {
        println!("loaded {db_acl_count} runtime ACL override(s), {db_rule_count} runtime rule(s)");
    }
    // Background scheduler: each registered cron job has its own interval persisted in meshble_cron;
    // this fixed tick only bounds how promptly a due job is observed. The claim is atomic + SKIP
    // LOCKED, so running several server processes is safe (no double-run).
    let cron_db = db.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(CRON_TICK_SECS)).await;
            if let Err(e) = cron_db.run_due_crons().await {
                eprintln!("meshble cron tick failed: {e:?}");
            }
        }
    });

    // Live served-catalog refresh: re-read the install ledger periodically so a module installed or
    // uninstalled by ANOTHER process (or the CLI) is reflected without a restart (Odoo's registry
    // signaling, polled). The acting server process also refreshes its own set synchronously on
    // install/uninstall, so this only matters for multi-process / out-of-band changes.
    let refresh_db = db.clone();
    let refresh_set = installed_set.clone();
    let refresh_custom = custom_fields.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(MODULE_REFRESH_SECS)).await;
            if let Ok(names) = refresh_db.installed_modules().await {
                if let Ok(mut w) = refresh_set.write() {
                    *w = names.into_iter().collect();
                }
            }
            refresh_custom_fields(&refresh_custom, &refresh_db).await;
        }
    });

    // Content-addressed blob store for attachments. The root is config-driven (validated present for
    // the fs backend); identical bytes deduplicate to one immutable file.
    let blob_root = s
        .config
        .storage
        .path
        .clone()
        .ok_or("storage.path is required for the fs blob store")?;
    let blobs: std::sync::Arc<dyn meshble_storage::BlobStore> =
        std::sync::Arc::new(meshble_storage::FsBlobStore::new(blob_root));

    let app = router_with_data_dynamic(models, installed_set, custom_fields, db, acls, rules, s.secrets.jwt_secret.clone(), blobs);

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("meshble serving on http://{bind}  ({} models)", registered_model_names().len());
    axum::serve(listener, app).await?;
    Ok(())
}

async fn user_command(db: &Db, action: UserCmd) -> Fallible {
    match action {
        UserCmd::Create { login, password, groups } => {
            let pw = password_or_env(password)?;
            let gs: Vec<&str> = groups.split(',').map(|g| g.trim()).filter(|g| !g.is_empty()).collect();
            db.upsert_user(&login, &hash_password(&pw)?, &gs).await?;
            println!("user '{login}' created with groups: {}", gs.join(", "));
        }
        UserCmd::SetPassword { login, password } => {
            let user = db.find_user(&login).await?.ok_or_else(|| format!("no such user: {login}"))?;
            let pw = password_or_env(password)?;
            let gs: Vec<&str> = user.groups.iter().map(|g| g.as_str()).collect();
            db.upsert_user(&login, &hash_password(&pw)?, &gs).await?;
            println!("password updated for '{login}'");
        }
        UserCmd::Grant { login, group } => {
            let user = db.find_user(&login).await?.ok_or_else(|| format!("no such user: {login}"))?;
            let mut groups = user.groups.clone();
            if !groups.iter().any(|g| g == &group) {
                groups.push(group.clone());
            }
            let gs: Vec<&str> = groups.iter().map(|g| g.as_str()).collect();
            db.upsert_user(&login, &user.password_hash, &gs).await?;
            println!("granted '{group}' to '{login}' (groups: {})", gs.join(", "));
        }
        UserCmd::Company { login, active, allowed } => {
            let ids: Vec<i64> = allowed.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            db.set_user_companies(&login, active, &ids).await?;
            println!("company scope for '{login}': active={active:?}, allowed={ids:?}");
        }
    }
    Ok(())
}

fn password_or_env(flag: Option<String>) -> Result<String, Box<dyn std::error::Error>> {
    flag.or_else(|| std::env::var("MESHBLE_NEW_PASSWORD").ok())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "no password: pass --password or set MESHBLE_NEW_PASSWORD".into())
}

fn flags(read: bool, write: bool, create: bool, delete: bool) -> String {
    let mut on: Vec<&str> = Vec::new();
    for (name, set) in [("read", read), ("write", write), ("create", create), ("delete", delete)] {
        if set {
            on.push(name);
        }
    }
    if on.is_empty() { "none".to_string() } else { on.join(",") }
}

async fn acl_command(db: &Db, action: AclCmd) -> Fallible {
    match action {
        AclCmd::Grant { model, group, read, write, create, delete } => {
            if !(read || write || create || delete) {
                return Err("grant at least one of --read/--write/--create/--delete".into());
            }
            db.set_db_acl(&model, &group, read, write, create, delete).await?;
            println!("acl: '{group}' on '{model}' = {}", flags(read, write, create, delete));
        }
        AclCmd::Revoke { model, group } => {
            db.remove_db_acl(&model, &group).await?;
            println!("acl override removed: '{group}' on '{model}' (static baseline unchanged)");
        }
        AclCmd::List => {
            println!("[compiled-in baseline]");
            for a in registered_acls() {
                println!("  {} / {} = {}", a.model, a.group, flags(a.read, a.write, a.create, a.delete));
            }
            let db_acls = db.list_db_acls().await?;
            println!("[runtime DB overrides]");
            if db_acls.is_empty() {
                println!("  (none)");
            }
            for a in db_acls {
                println!("  {} / {} = {}", a.model, a.group, flags(a.read, a.write, a.create, a.delete));
            }
        }
    }
    Ok(())
}

async fn rule_command(db: &Db, action: RuleCmd) -> Fallible {
    match action {
        RuleCmd::Add { model, groups, ops, domain } => {
            let id = db.set_db_rule(&model, &groups, &ops, &domain).await?;
            let scope = if groups.trim().is_empty() { "global".to_string() } else { format!("groups={groups}") };
            println!("rule #{id} added on '{model}' ({scope}, ops={ops})");
        }
        RuleCmd::Remove { id } => {
            db.remove_db_rule(id).await?;
            println!("rule #{id} removed (static baseline unchanged)");
        }
        RuleCmd::List => {
            let rules = db.list_db_rules().await?;
            if rules.is_empty() {
                println!("(no runtime rules)");
            }
            for r in rules {
                let scope = if r.groups.trim().is_empty() { "global".to_string() } else { format!("groups={}", r.groups) };
                let act = if r.active { "" } else { " [inactive]" };
                println!("  #{} {} ({}, ops={}){}  {}", r.id, r.model, scope, r.ops, act, r.domain);
            }
        }
    }
    Ok(())
}

async fn module_command(db: &Db, action: ModuleCmd) -> Fallible {
    let mods = resolve_modules().map_err(|e| format!("{e:?}"))?;
    match action {
        ModuleCmd::List => {
            let installed = db.installed_modules().await?;
            for m in &mods {
                let state = if installed.iter().any(|i| i == m.name) { "installed" } else { "available" };
                println!("  {:<10} {:<8} [{state}]  {}", m.name, m.version, m.summary);
            }
        }
        ModuleCmd::Install { name } => {
            let want = module_closure(&name)?; // name + transitive dependencies, deps first
            let mut any = false;
            for m in mods.iter().filter(|m| want.contains(&m.name)) {
                if !db.is_module_installed(m.name).await? {
                    db.mark_module_installed(m.name, m.version).await?;
                    println!("installing {} {}", m.name, m.version);
                    any = true;
                }
            }
            if !any {
                println!("'{name}' and its dependencies are already installed");
            }
            migrate_installed(db).await?; // create the newly-installed modules' tables (idempotent)
        }
        ModuleCmd::Uninstall { name } => {
            if name == "base" {
                return Err("cannot uninstall 'base' (the foundational module)".into());
            }
            if !db.is_module_installed(&name).await? {
                return Err(format!("module '{name}' is not installed").into());
            }
            // Refuse if an installed module still depends on it (downstream guard, like Odoo).
            let installed = db.installed_modules().await?;
            let dependents: Vec<&str> = mods
                .iter()
                .filter(|m| installed.iter().any(|i| i == m.name) && m.depends.iter().any(|d| d.name == name))
                .map(|m| m.name)
                .collect();
            if !dependents.is_empty() {
                return Err(format!("uninstall {dependents:?} first — they depend on '{name}'").into());
            }
            db.mark_module_uninstalled(&name).await?;
            println!("uninstalled '{name}' (its tables and data are kept; re-install to restore)");
        }
    }
    Ok(())
}
