//! `kigumi` — the single command to operate an instance: migrate the catalog, serve the secured
//! API, inspect config, and manage users. Wires the existing crates together driven by
//! `kigumi-config` (DATABASE_URL / JWT secret / bind), so nothing is hardcoded.
//!
//! The linked modules (`kigumi-mod-base`, `kigumi-mod-mail`, `kigumi-mod-sales`) self-register
//! their models, ACLs, and record rules into the catalog via `inventory`; this binary just
//! collects and serves them.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use kigumi::prelude::*;
use kigumi_auth::hash_password;
use kigumi_config::Settings;
use kigumi_db::{Db, OutgoingMail, WebhookDelivery};
use kigumi_server::{
    access_fingerprint, refresh_access, refresh_custom_fields, refresh_view_overrides,
    router_with_data_dynamic_rasterized, GenpdfRasterizer,
};

mod scaffold;

type Fallible = Result<(), Box<dyn std::error::Error>>;

/// How often the scheduler checks for due cron jobs (each job's own interval lives in the DB).
const CRON_TICK_SECS: u64 = 60;
/// Ad-hoc jobs deserve a tighter observation bound than crons (a queued job should start in
/// seconds, not up to a minute) — the claim itself stays SKIP LOCKED so multiple workers are safe.
const JOB_TICK_SECS: u64 = 5;
/// How often a running server re-reads the install ledger to pick up out-of-band module changes.
const MODULE_REFRESH_SECS: u64 = 8;

/// Forces the feature-enabled module crates to link so their `inventory` registrations are present in this
/// binary. A module absent at build time (its feature off) simply never registers — `resolve_modules` and
/// the rest of the CLI are inventory-driven, so nothing else needs to know it is gone. With every feature
/// off (`--no-default-features`) this is empty and the binary is a bare framework server.
fn link_modules() {
    #[cfg(feature = "base")]
    let _ = &kigumi_mod_base::MANIFEST;
    #[cfg(feature = "mail")]
    let _ = &kigumi_mod_mail::MANIFEST;
    #[cfg(feature = "sales")]
    let _ = &kigumi_mod_sales::MANIFEST;
    #[cfg(feature = "account")]
    let _ = &kigumi_mod_account::MANIFEST;
    #[cfg(feature = "stock")]
    let _ = &kigumi_mod_stock::MANIFEST;
}

#[derive(Parser)]
#[command(name = "kigumi", version, about = "Kigumi instance CLI")]
struct Cli {
    /// Path to kigumi.toml (default: $KIGUMI_CONFIG or ./kigumi.toml).
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
    /// Manage API keys (long-lived machine credentials that impersonate a user).
    Apikey {
        #[command(subcommand)]
        action: ApikeyCmd,
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
    /// Serve this instance over the Model Context Protocol (stdio), impersonating a user: every
    /// tool call runs under that user's ACLs and record rules. No credential is checked - running
    /// this command already requires the database URL, so the gate is operator trust.
    Mcp {
        /// Login of the user the MCP client acts as.
        user: String,
    },
    /// Serve MCP over streamable HTTP (network-facing): each request authenticates with an API key
    /// (Authorization: Bearer kg_...) and acts as that key's user. Endpoint /mcp.
    McpHttp {
        /// Bind address (default 127.0.0.1:8601).
        #[arg(long, default_value = "127.0.0.1:8601")]
        bind: String,
    },
    /// Scaffold a new Kigumi application workspace (module crate + server binary on kigumi-runtime).
    New {
        /// Name of the new app (also the directory and module crate name).
        name: String,
        /// Extra modules beyond base+mail, CSV of sales,account,stock. Prompted when omitted.
        #[arg(long)]
        modules: Option<String>,
        /// Point the generated Cargo.toml at a local framework checkout instead of crates.io.
        #[arg(long)]
        framework_path: Option<PathBuf>,
        /// Use git dependencies on this URL instead of crates.io versions.
        #[arg(long)]
        git: Option<String>,
        /// Accept defaults without prompting.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

/// One-line interactive prompt with a default (used only on a TTY).
fn ask(question: &str, default: &str) -> String {
    use std::io::Write;
    print!("{question} [{default}]: ");
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).ok();
    let t = s.trim();
    if t.is_empty() { default.to_string() } else { t.to_string() }
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
    /// Create or replace a user. Password via --password (dev) or $KIGUMI_NEW_PASSWORD.
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

#[derive(Subcommand)]
enum ApikeyCmd {
    /// Mint a key for a user. Prints the secret ONCE. --scopes is a CSV subset of the user's groups
    /// (empty = all); --expires-days sets an optional expiry.
    Create {
        user: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        scopes: String,
        #[arg(long)]
        expires_days: Option<i64>,
    },
    /// List a user's live keys (never the secret).
    List { user: String },
    /// Revoke a key by id.
    Revoke { id: i64 },
}

fn config_path(cli: &Cli) -> PathBuf {
    cli.config
        .clone()
        .or_else(|| std::env::var("KIGUMI_CONFIG").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("kigumi.toml"))
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
        Cmd::Mcp { user } => {
            // MCP servers are launched by agent clients with env-var config (no kigumi.toml next
            // to them): DATABASE_URL from the environment wins, the config file is the fallback.
            let url = match std::env::var("DATABASE_URL") {
                Ok(u) => u,
                Err(_) => Settings::load(Some(&path))?.secrets.database_url,
            };
            let db = Db::connect(&url).await?;
            let server = kigumi_mcp::KigumiMcp::for_login(db, &user).await?;
            server.serve_stdio().await.map_err(|e| e.to_string())?;
            Ok(())
        }
        Cmd::McpHttp { bind } => {
            let url = match std::env::var("DATABASE_URL") {
                Ok(u) => u,
                Err(_) => Settings::load(Some(&path))?.secrets.database_url,
            };
            let db = Db::connect(&url).await?;
            kigumi_mcp::serve_http(db, &bind).await.map_err(|e| e.to_string())?;
            Ok(())
        }
        Cmd::New { name, modules, framework_path, git, yes } => {
            let ident = scaffold::sanitize_name(&name);
            if ident.is_empty() || ident.chars().all(|c| c == '_') {
                return Err(format!("'{name}' is not a valid app name").into());
            }
            if ident != name {
                println!("note: using '{ident}' as the crate/module name");
            }
            let extras_csv = match modules {
                Some(csv) => csv,
                None => {
                    use std::io::IsTerminal;
                    if !yes && std::io::stdin().is_terminal() {
                        ask("Extra modules (sales,account,stock)", "none")
                    } else {
                        eprintln!("note: non-interactive, no extra modules (use --modules to pick some)");
                        "none".to_string()
                    }
                }
            };
            let extras: Vec<String> = extras_csv
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && s != "none")
                .collect();
            scaffold::validate_extras(&extras)?;
            let extras = scaffold::module_closure(&extras);
            let framework = match (framework_path, git) {
                (Some(p), _) => scaffold::FrameworkSource::Path(
                    p.canonicalize().map_err(|e| format!("--framework-path '{}': {e}", p.display()))?,
                ),
                (None, Some(url)) => scaffold::FrameworkSource::Git(url),
                (None, None) => scaffold::FrameworkSource::CratesIo,
            };
            let dest = PathBuf::from(&ident);
            let opts = scaffold::ScaffoldOptions { name: ident.clone(), extra_modules: extras, framework };
            scaffold::scaffold(&dest, &opts)?;
            println!("created {ident}/");
            println!("next steps:");
            println!("  cd {ident}");
            println!("  createdb {ident}");
            println!("  export DATABASE_URL=postgres://localhost/{ident}");
            println!("  export KIGUMI_JWT_SECRET=change-me");
            println!("  KIGUMI_ADMIN_PASSWORD=change-me cargo run -p app -- migrate");
            println!("  cargo run -p app -- serve");
            Ok(())
        }
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
            println!("kigumi framework {FRAMEWORK_VERSION}");
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
            db.ensure_api_key_schema().await?;
            user_command(&db, action).await
        }
        Cmd::Apikey { action } => {
            // Env-first (like `mcp`/`mcp-http`): key management is often run without a kigumi.toml.
            let url = match std::env::var("DATABASE_URL") {
                Ok(u) => u,
                Err(_) => Settings::load(Some(&path))?.secrets.database_url,
            };
            let db = Db::connect(&url).await?;
            db.ensure_auth_schema().await?;
            db.ensure_api_key_schema().await?;
            apikey_command(&db, action).await
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
/// installed first — the rest is opt-in via `kigumi module install <name>`, like Odoo's selective
/// install rather than installing everything available.
async fn migrate(db: &Db) -> Fallible {
    db.ensure_auth_schema().await?;
    db.ensure_job_schema().await?;
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

/// Migrates the models that belong to currently-installed modules, in FK-dependency order.
/// Reference-data seeding now rides inside `migrate_installed_schema` via the `register_seed!`
/// seam (each module owns its seed), so adopter binaries get it too.
async fn migrate_installed(db: &Db) -> Fallible {
    db.migrate_installed_schema().await?;
    Ok(())
}




/// Bootstraps an `admin` user from KIGUMI_ADMIN_PASSWORD if none exists (never hardcodes a password).
async fn bootstrap_admin(db: &Db) -> Fallible {
    if db.find_user("admin").await?.is_some() {
        return Ok(());
    }
    match std::env::var("KIGUMI_ADMIN_PASSWORD") {
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
        _ => eprintln!("warning: no admin user; set KIGUMI_ADMIN_PASSWORD to bootstrap one"),
    }
    Ok(())
}

/// Builds an SMTP send closure from the instance config, or None if SMTP is not configured (the queue
/// then just accumulates). The closure sends one queued mail over a BLOCKING lettre transport (STARTTLS
/// on 587, implicit TLS on 465); it runs in the mail-flush task, so blocking on a slow server is contained.
fn build_mail_sender(s: &Settings) -> Option<Box<dyn Fn(&OutgoingMail) -> Result<(), String> + Send + Sync>> {
    use lettre::message::header::ContentType;
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{Message, SmtpTransport, Transport};

    let host = s.config.mail.smtp_host.clone()?;
    let port = s.config.mail.smtp_port.unwrap_or(587);
    let from_default = s.config.mail.from.clone().unwrap_or_else(|| "no-reply@localhost".to_string());
    let builder = if port == 465 { SmtpTransport::relay(&host) } else { SmtpTransport::starttls_relay(&host) };
    let mut builder = match builder {
        Ok(b) => b.port(port),
        Err(e) => {
            eprintln!("smtp transport build failed ({e}); outbound mail disabled");
            return None;
        }
    };
    if let Some(pw) = s.secrets.smtp_password.clone() {
        builder = builder.credentials(Credentials::new(from_default.clone(), pw));
    }
    let mailer = builder.build();
    println!("outbound mail enabled (smtp {host}:{port})");
    Some(Box::new(move |m: &OutgoingMail| -> Result<(), String> {
        // Force the envelope From to the configured address — the relay is authorized only for it, so a
        // queued row can never be sent as a spoofed sender. `m.from` is informational only.
        let from = from_default.clone();
        let email = Message::builder()
            .from(from.parse().map_err(|e| format!("bad from address: {e}"))?)
            .to(m.to.parse().map_err(|e| format!("bad to address: {e}"))?)
            .subject(m.subject.clone())
            .header(ContentType::TEXT_HTML)
            .body(m.body.clone())
            .map_err(|e| e.to_string())?;
        mailer.send(&email).map(|_| ()).map_err(|e| e.to_string())
    }))
}

/// True if an IP must never be a webhook target — loopback, private, link-local (incl. 169.254.169.254
/// cloud metadata), CGNAT, unspecified, and the IPv6 equivalents. The SSRF guard: a tenant-supplied URL
/// must not be coercible into reaching the instance's own network.
fn ip_is_blocked(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()
                || v4.is_broadcast() || v4.is_documentation()
                || o[0] == 0                              // 0.0.0.0/8
                || (o[0] == 100 && (o[1] & 0xc0) == 64)   // 100.64.0.0/10 CGNAT
        }
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified() || v6.is_multicast()
                || (v6.segments()[0] & 0xfe00) == 0xfc00  // fc00::/7 unique-local
                || (v6.segments()[0] & 0xffc0) == 0xfe80  // fe80::/10 link-local
                || v6.to_ipv4_mapped().map(|m| ip_is_blocked(IpAddr::V4(m))).unwrap_or(false)
        }
    }
}

/// Rejects a webhook URL that is not https (unless KIGUMI_WEBHOOK_ALLOW_INSECURE=1 for local dev) or
/// whose host resolves to a blocked address. ponytail: resolve-then-check has a TOCTOU window against DNS
/// rebinding (reqwest re-resolves on connect); redirect::none + https + this check cover the common case —
/// pin to the resolved IP if a hostile-tenant threat model demands it.
fn webhook_url_is_safe(url: &str) -> Result<(), String> {
    use std::net::ToSocketAddrs;
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("bad url: {e}"))?;
    let insecure_ok = std::env::var("KIGUMI_WEBHOOK_ALLOW_INSECURE").as_deref() == Ok("1");
    if parsed.scheme() != "https" && !(insecure_ok && parsed.scheme() == "http") {
        return Err("webhook url must be https".into());
    }
    let host = parsed.host_str().ok_or("url has no host")?;
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs: Vec<_> = (host, port).to_socket_addrs().map_err(|e| format!("dns resolution failed: {e}"))?.collect();
    if addrs.is_empty() {
        return Err("host did not resolve".into());
    }
    if addrs.iter().any(|a| ip_is_blocked(a.ip())) {
        return Err("webhook host resolves to a blocked (private/loopback) address".into());
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The webhook transport — the ONLY place HTTP + HMAC live. Each pending delivery is POSTed as the frozen
/// JSON envelope over a blocking client (redirect::none, 10s timeout) and signed Stripe-style:
/// `X-Kigumi-Signature: t=<unix>,v1=<hex HMAC-SHA256(secret, "<t>.<body>")>`, so a consumer can verify
/// authenticity + reject replays. A non-2xx (or a transport/SSRF error) returns Err -> the db layer retries
/// with backoff and dead-letters after the cap. ponytail: blocking send on the flush task's worker, fine at
/// this cadence; move to spawn_blocking/async if delivery volume grows.
fn build_webhook_sender() -> Box<dyn Fn(&WebhookDelivery) -> Result<(), String> + Send + Sync> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("blocking http client");

    Box::new(move |d: &WebhookDelivery| -> Result<(), String> {
        webhook_url_is_safe(&d.url)?;
        let body = serde_json::to_string(&d.payload).map_err(|e| e.to_string())?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs();
        let mut mac = Hmac::<Sha256>::new_from_slice(d.secret.as_bytes()).map_err(|e| e.to_string())?;
        mac.update(format!("{ts}.{body}").as_bytes());
        let sig = hex_encode(&mac.finalize().into_bytes());
        let event_type = d.payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let idem = d.payload.get("id").and_then(|v| v.as_str()).unwrap_or("");

        let resp = client
            .post(&d.url)
            .header("Content-Type", "application/json")
            .header("User-Agent", "kigumi-webhooks/1")
            .header("X-Kigumi-Signature", format!("t={ts},v1={sig}"))
            .header("X-Kigumi-Event", event_type)
            .header("X-Kigumi-Delivery", d.id.to_string())
            .header("X-Kigumi-Idempotency", idem)
            .body(body)
            .send()
            .map_err(|e| format!("transport error: {e}"))?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(format!("endpoint returned {status}"))
        }
    })
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
    db.ensure_view_schema().await?;
    let models: Vec<_> = resolve_all_registered().map_err(|e| e.to_string())?.into_iter().collect();
    let installed_set: std::sync::Arc<std::sync::RwLock<std::collections::HashSet<String>>> =
        std::sync::Arc::new(std::sync::RwLock::new(db.installed_modules().await?.into_iter().collect()));
    // Runtime custom fields: the declarative-extension layer, loaded live and merged into models on
    // resolve — a field added via /api/<model>/_fields appears with no restart.
    let custom_fields: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<String, Vec<kigumi::prelude::FieldDef>>>> =
        std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()));
    refresh_custom_fields(&custom_fields, &db).await;
    // Runtime view overrides: relabel/hide/lock/re-widget a field, merged into the contract on serve.
    let view_overrides: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<String, Vec<kigumi_db::ViewOverride>>>> =
        std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()));
    refresh_view_overrides(&view_overrides, &db).await;

    // Effective access = compiled-in baseline ∪ runtime DB overrides (hybrid, D12). For ACLs the DB
    // rows only widen access (union); for record rules they add restrictions/alternatives through the
    // same engine — either way the static baseline stays in force. Held as a LIVE snapshot (not leaked
    // to `'static`), so a DB ACL/rule added via the CLI or the `/api/_acl` `/api/_rule` endpoints takes
    // effect without a restart: the poll loop below reloads on change, the endpoints reload at once.
    db.ensure_access_schema().await?;
    db.ensure_event_schema().await?;
    db.ensure_job_schema().await?;
    let acls: kigumi_server::AclState =
        std::sync::Arc::new(std::sync::RwLock::new(std::sync::Arc::from(registered_acls())));
    let rules: kigumi_server::RuleState =
        std::sync::Arc::new(std::sync::RwLock::new(std::sync::Arc::from(registered_rules())));
    refresh_access(&acls, &rules, &db).await;
    // Background scheduler: each registered cron job has its own interval persisted in kigumi_cron;
    // this fixed tick only bounds how promptly a due job is observed. The claim is atomic + SKIP
    // LOCKED, so running several server processes is safe (no double-run).
    let cron_db = db.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(CRON_TICK_SECS)).await;
            if let Err(e) = cron_db.run_due_crons().await {
                eprintln!("kigumi cron tick failed: {e:?}");
            }
        }
    });
    // Ad-hoc job runner: reap expired leases (crashed-worker recovery), then claim + run due jobs.
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

    // Outbound mail: if SMTP is configured, flush the mail.mail queue each tick over a blocking SMTP
    // transport (lettre). Without SMTP config the queue just accumulates. Its own task so a slow SMTP
    // server never delays the cron scheduler.
    if let Some(send) = build_mail_sender(&s) {
        let mail_db = db.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(CRON_TICK_SECS)).await;
                if let Err(e) = mail_db.flush_outgoing_mail(send.as_ref()).await {
                    eprintln!("kigumi mail flush failed: {e:?}");
                }
            }
        });
    }

    // Outbound webhooks: each tick, materialize undispatched domain events into per-subscription
    // deliveries (fan-out), reap any deliveries a crashed flusher left leased, then POST the pending ones
    // over the signed blocking transport (retry/backoff/dead-letter handled in the db layer). Its own
    // task so a slow endpoint never delays the cron scheduler or mail flush.
    let wh_db = db.clone();
    let wh_send = build_webhook_sender();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(CRON_TICK_SECS)).await;
            if let Err(e) = wh_db.fan_out_events().await {
                eprintln!("kigumi webhook fan-out failed: {e:?}");
            }
            if let Err(e) = wh_db.reap_stuck_deliveries().await {
                eprintln!("kigumi webhook reap failed: {e:?}");
            }
            if let Err(e) = wh_db.flush_webhooks(wh_send.as_ref()).await {
                eprintln!("kigumi webhook flush failed: {e:?}");
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
    let refresh_views = view_overrides.clone();
    let refresh_acls = acls.clone();
    let refresh_rules = rules.clone();
    let mut access_fp = access_fingerprint(&db).await;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(MODULE_REFRESH_SECS)).await;
            if let Ok(names) = refresh_db.installed_modules().await {
                if let Ok(mut w) = refresh_set.write() {
                    *w = names.into_iter().collect();
                }
            }
            refresh_custom_fields(&refresh_custom, &refresh_db).await;
            refresh_view_overrides(&refresh_views, &refresh_db).await;
            // Reload the access policy only when the DB rows actually changed — avoids the
            // load_*_static identifier-string leak on every idle tick, while still picking up
            // out-of-band edits (the `kigumi acl/rule` CLI, or direct SQL) without a restart. The
            // cursor advances only when the reload SUCCEEDED, so a transient DB error leaves the prior
            // good policy in force and retries next tick (never silently drops a restricting rule).
            let fp = access_fingerprint(&refresh_db).await;
            if fp != access_fp && refresh_access(&refresh_acls, &refresh_rules, &refresh_db).await {
                access_fp = fp;
            }
        }
    });

    // Content-addressed blob store for attachments, chosen by [storage] backend (config validates the
    // required fields). Identical bytes deduplicate to one immutable object on either backend.
    let blobs: std::sync::Arc<dyn kigumi_storage::BlobStore> = match s.config.storage.backend {
        kigumi_config::StorageBackend::Fs => {
            let blob_root = s
                .config
                .storage
                .path
                .clone()
                .ok_or("storage.path is required for the fs blob store")?;
            std::sync::Arc::new(kigumi_storage::FsBlobStore::new(blob_root))
        }
        kigumi_config::StorageBackend::S3 => {
            // bucket/region from config; endpoint (MinIO/R2/custom) + credentials from the env.
            let bucket = s
                .config
                .storage
                .bucket
                .clone()
                .ok_or("storage.bucket is required for the s3 blob store")?;
            let region = s.config.storage.region.clone().unwrap_or_else(|| "us-east-1".into());
            let endpoint = std::env::var("KIGUMI_S3_ENDPOINT").ok();
            std::sync::Arc::new(kigumi_storage::S3BlobStore::new(&bucket, &region, endpoint.as_deref())?)
        }
    };

    let app = router_with_data_dynamic_rasterized(
        models,
        installed_set,
        custom_fields,
        view_overrides,
        db,
        acls,
        rules,
        s.secrets.jwt_secret.clone(),
        blobs,
        Some(std::sync::Arc::new(GenpdfRasterizer::new())),
    );

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("kigumi serving on http://{bind}  ({} models)", registered_model_names().len());
    axum::serve(listener, app).await?;
    Ok(())
}

async fn apikey_command(db: &Db, action: ApikeyCmd) -> Fallible {
    match action {
        ApikeyCmd::Create { user, name, scopes, expires_days } => {
            let u = db.find_user(&user).await?.ok_or_else(|| format!("unknown user '{user}'"))?;
            let requested: Vec<String> =
                scopes.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            // A key can only NARROW: every scope must be one of the user's groups.
            if let Some(bad) = requested.iter().find(|g| !u.groups.contains(g)) {
                return Err(format!("scope '{bad}' is not one of {user}'s groups ({})", u.groups.join(", ")).into());
            }
            let minted = kigumi_auth::new_api_key().map_err(|e| format!("{e:?}"))?;
            let expires = expires_days.map(|d| d * 86_400);
            db.create_api_key(&minted.prefix, &minted.hash, u.id, &name, &requested, expires).await?;
            println!("{}", minted.plain);
            eprintln!("store this key now — it is not recoverable");
        }
        ApikeyCmd::List { user } => {
            let u = db.find_user(&user).await?.ok_or_else(|| format!("unknown user '{user}'"))?;
            for k in db.list_api_keys(u.id).await? {
                println!("{}\t{}\t[{}]\t{}", k.id, k.prefix, k.scopes.join(","), k.name);
            }
        }
        ApikeyCmd::Revoke { id } => {
            // The CLI is operator-trusted: revoke by id regardless of owner (the HTTP path scopes
            // revocation to the caller).
            let revoked = db.revoke_api_key_admin(id).await?;
            println!("{}", if revoked { "revoked" } else { "no such live key" });
        }
    }
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
    flag.or_else(|| std::env::var("KIGUMI_NEW_PASSWORD").ok())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "no password: pass --password or set KIGUMI_NEW_PASSWORD".into())
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
