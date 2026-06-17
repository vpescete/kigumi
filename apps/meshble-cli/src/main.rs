//! `meshble` — the single command to operate an instance: migrate the catalog, serve the secured
//! API, inspect config, and manage users. Wires the existing crates together driven by
//! `meshble-config` (DATABASE_URL / JWT secret / bind), so nothing is hardcoded.
//!
//! The linked modules (`meshble-mod-base`, `meshble-mod-sales`) self-register their models, ACLs,
//! and record rules into the catalog via `inventory`; this binary just collects and serves them.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use meshble::prelude::*;
use meshble_auth::hash_password;
use meshble_config::Settings;
use meshble_db::Db;
use meshble_server::router_with_data;

type Fallible = Result<(), Box<dyn std::error::Error>>;

/// Forces the module crates to link so their `inventory` registrations are present in this binary.
fn link_modules() {
    let _ = (&meshble_mod_base::MANIFEST, &meshble_mod_sales::MANIFEST);
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
    /// Print the framework version and the linked modules.
    Version,
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
        Cmd::Serve => {
            let s = Settings::load(Some(&path))?;
            let db = Db::connect(&s.secrets.database_url).await?;
            migrate(&db).await?;
            bootstrap_admin(&db).await?;
            serve(s).await
        }
    }
}

/// Migrates every linked module's models in dependency order (FK targets first), plus the auth schema.
async fn migrate(db: &Db) -> Fallible {
    db.ensure_auth_schema().await?;
    db.ensure_sequence_schema().await?;
    db.ensure_setting_schema().await?;
    db.ensure_access_schema().await?;
    // Install-time runtime defaults (DB is the authority; never overwrites an operator change).
    db.seed_setting("base_url", "", "string").await?;
    db.seed_setting("mode", "production", "string").await?;
    let plan = migration_plan().map_err(|e| e.to_string())?;
    for t in &plan {
        // Ledger key is the model name: each model/table is its own migration unit (install_or_upgrade
        // tracks one table per key). The owning module name/version is informational here.
        db.install_or_upgrade(&t.model, t.model.name, &t.version, &[]).await?;
        println!("migrated {} ({} {})", t.model.name, t.module, t.version);
    }
    seed_base_data(db).await?;
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

    // Document-numbering sequences used by registered actions (e.g. sale.order confirm → SO/00001).
    db.ensure_sequence("SO", "SO/", "", 5).await?;

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
            db.upsert_user("admin", &hash_password(&p)?, &["user", "sales.user", "sales.manager", "admin"])
                .await?;
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
    let models = resolve_all_registered().map_err(|e| e.to_string())?;
    let bind = s.config.server.bind.clone();
    let db = Db::connect(&s.secrets.database_url).await?;

    // Effective ACLs = compiled-in baseline ∪ runtime DB overrides (hybrid, D12). DB grants only
    // widen access (union), so the static baseline stays a floor. Collected once at startup and
    // given the process lifetime (the server holds them for `'static`).
    db.ensure_access_schema().await?;
    let mut all_acls = registered_acls();
    let db_acls = db.load_acls_static().await?;
    let db_acl_count = db_acls.len();
    all_acls.extend(db_acls);
    let acls: &'static [Acl] = Box::leak(all_acls.into_boxed_slice());
    let rules: &'static [RecordRule] = Box::leak(registered_rules().into_boxed_slice());
    if db_acl_count > 0 {
        println!("loaded {db_acl_count} runtime ACL override(s)");
    }
    let app = router_with_data(models, db, acls, rules, s.secrets.jwt_secret.clone());

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
