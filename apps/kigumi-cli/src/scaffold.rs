//! `kigumi new` — scaffolds an adopter workspace: one application module crate (a starter
//! `<name>.ticket` model with ACLs, actions and numbering) plus a binary on kigumi-runtime, a
//! README quickstart, and a .gitignore. The template is embedded here so the installed CLI needs
//! no network or filesystem source; placeholders are plain `__APP__`-style tokens (no format!
//! brace-escaping in Rust/TOML content).

use std::path::{Path, PathBuf};

/// Where the generated Cargo.toml points its kigumi dependencies.
pub enum FrameworkSource {
    /// Published versions from crates.io (the default).
    CratesIo,
    /// A git URL (tracking the repo's main).
    Git(String),
    /// A local checkout of the framework repo (development of the framework itself).
    Path(PathBuf),
}

pub struct ScaffoldOptions {
    /// Sanitized crate ident (lowercase, [a-z0-9_], not digit-leading).
    pub name: String,
    /// Extra stdlib/ERP modules beyond base+mail; dependency closure already applied.
    pub extra_modules: Vec<String>,
    pub framework: FrameworkSource,
}

/// Lowercases and maps every non-alphanumeric to '_'; prefixes '_' if digit-leading.
pub fn sanitize_name(raw: &str) -> String {
    let mut s: String = raw
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    s
}

/// Expands `extras` to include module dependencies (picking stock without sales cannot boot).
pub fn module_closure(extras: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |name: &str, out: &mut Vec<String>| {
        if !out.iter().any(|x| x == name) {
            out.push(name.to_string());
        }
    };
    for m in extras {
        match m.as_str() {
            "sales" => add("sales", &mut out),
            "account" => add("account", &mut out),
            "stock" => {
                add("sales", &mut out);
                add("stock", &mut out);
            }
            other => {
                // Unknown names were validated by the caller; keep this total anyway.
                add(other, &mut out);
            }
        }
    }
    out
}

const KNOWN_EXTRAS: [&str; 3] = ["sales", "account", "stock"];

pub fn validate_extras(extras: &[String]) -> Result<(), String> {
    for m in extras {
        if !KNOWN_EXTRAS.contains(&m.as_str()) {
            return Err(format!(
                "unknown module '{m}' (available: {})",
                KNOWN_EXTRAS.join(", ")
            ));
        }
    }
    Ok(())
}

/// One dependency line for a kigumi crate, in the chosen source form. `version` is the crates.io
/// requirement (framework crates "0.2", modules "2.0").
fn dep(framework: &FrameworkSource, crate_name: &str, subdir: &str, version: &str) -> String {
    match framework {
        FrameworkSource::CratesIo => format!("{crate_name} = \"{version}\""),
        FrameworkSource::Git(url) => format!("{crate_name} = {{ git = \"{url}\" }}"),
        FrameworkSource::Path(root) => {
            format!("{crate_name} = {{ path = \"{}/{subdir}\" }}", root.display())
        }
    }
}

const WORKSPACE_TOML: &str = r#"[workspace]
resolver = "2"
members = ["__APP__", "app"]
"#;

const GITIGNORE: &str = "/target\n/blobs\n";

// Emitted as AGENTS.md (the cross-tool convention: Codex, Cursor, ...) with CLAUDE.md importing
// it, so every scaffolded app is born agent-ready: the coding agent learns the seams, the DSL,
// the idioms and the failure modes without spelunking the framework source.
const AGENTS_MD: &str = r#"# __APP__ — agent guide

This workspace is a [Kigumi](https://github.com/vpescete/kigumi) application: a headless,
schema-driven business app in Rust. One declarative model generates schema, REST API, UI
contract and security; modules compose at compile time — if it builds, it fits.

- `__APP__/` — the application module: ALL business declarations live in `__APP__/src/lib.rs`
  (split into submodules when it grows). This is where you work.
- `app/` — the server binary on `kigumi-runtime`: four calls, it should almost never change.

## Commands

```sh
cargo build                       # the compile check IS the composition check
cargo test -p __APP__             # module tests
DATABASE_URL=postgres://localhost/__APP__ KIGUMI_ADMIN_PASSWORD=... cargo run -p app -- migrate
DATABASE_URL=postgres://localhost/__APP__ KIGUMI_JWT_SECRET=... cargo run -p app -- serve
```

`migrate` is idempotent — run it after every model change and on every deploy. It creates
tables additively, ensures sequences, applies pending data migrations, runs seeds.
`serve` binds 127.0.0.1:8600 (override with `KIGUMI_BIND`).
`cargo run -p app -- mcp <login>` serves this app over MCP (stdio): an AI agent operates it AS
that user, with ACLs and record rules enforced on every tool.

## The seams — one macro each, declared next to the model they serve

| Macro | Use for | Body signature |
|---|---|---|
| `#[model(name, table)]` | a business model; fields via `#[field(...)]` | struct DSL, not real types |
| `register_acls!(&ACLS)` | access (default-deny without it) | `static ACLS: [Acl; N]` |
| `register_action!(model, name, fn, groups)` | state transitions + numbering | `fn(&ActionInput) -> Result<ActionOutcome, String>` |
| `register_sequence!(module, code, prefix, suffix, pad)` | document numbering used by `assign_sequence` | declarative |
| `register_compute!(name, fn)` | stored computed fields (`compute=`/`depends=`) | `fn(&ComputeInput) -> Value` |
| `register_constraint!(model, fields, fn)` | validation → structured 400 with per-field errors | `fn(&ComputeInput) -> Result<(), String>` |
| `register_service!(model, name, fn, write_gate, groups)` | cross-record work, ONE transaction | `async fn(&mut ServiceCtx, ServiceInput) -> Result<ServiceOutput, DbError>` |
| `register_job!(name, max_attempts, fn)` | background work, retries + backoff | `async fn(&Db, Json) -> Result<(), DbError>` |
| `register_route!(name, Get\|Post, auth, groups, fn)` | bespoke HTTP under `/api/x/<name>` (webhooks) | `async fn(&Db, RouteInput) -> Result<RouteOutput, DbError>` |
| `register_seed!(module, fn)` | reference data at migrate, idempotent | `async fn(&Db) -> Result<(), DbError>` |
| `register_migration!(module, to_version, fn)` | data migrations between module versions | `async fn(&Db) -> Result<(), DbError>` |
| `register_mailed!(model)` | chatter thread on the model | declarative |

## Model DSL quick reference

Field kinds: `Text`, `Html`, `Integer`, `Float`, `Decimal`, `Bool`, `Date`, `Datetime`,
`Selection`, `Many2one` (`target=`), `One2many` (`target=`, `inverse=`), `Many2many`, `Image`.
Common attrs: `label` (required), `required`, `unique`, `default="..."`,
`selection="k:Label,k2:Label2"`, `compute="fn_name"` + `depends="a,b"` + `store`, `tracked`,
`groups="group"` (field-level visibility), `check="sql_expr"`, `related="path.field"`.

## Idioms this framework expects

- Every elevation is explicit and greppable: gate permissions first, then `let elevated = ctx.sudo();`.
- Seeds and migration bodies are IDEMPOTENT (at-least-once): guard inserts with exists-checks.
- Ship data changes as `register_migration!` steps and bump `MANIFEST.version` in the same change;
  migrate applies pending steps in order and resumes after failures.
- Jobs are idempotent; enqueue from services with `cx.enqueue_job(...)` (transactional: the job
  exists iff the business write commits).
- Webhook routes verify signatures with `RouteInput::verify_hmac_sha256` (constant-time) —
  never a hand-rolled hash compare — then elevate.
- Namespace globals with the module name: computes (`__APP___total`), jobs, sequence codes.

## API quick reference (for curl checks)

- Auth: `POST /auth/login {"login","password"}` → `access_token`. NOT under `/api/`.
- CRUD: `GET|POST /api/:model`, `GET|PATCH|DELETE /api/:model/:id` (Bearer token).
- Actions: `POST /api/:model/:id/action/:name`. Services: `POST /api/:model/:id/service/:name`.
- Module routes: `GET|POST /api/x/:route`. Chatter: `POST /api/:model/:id/message`.
- Live events: `GET /api/events/stream` (SSE, `Last-Event-ID` resume).
- Machine-readable: `GET /openapi.json`, `GET /api/:model/view` (UI contract), `GET /api/models`.
- Errors: `{"error":{"code","message","fields"}}` — `fields` maps field → messages on validation.

## Common failures

- `unknown sequence code 'X'` → declare `kigumi::register_sequence!("__APP__", "X", ...)`, re-run migrate.
- `405` with `Allow` header on `/api/auth/login` → auth lives at `/auth/login`.
- A `auth:false` route can't read/write → expected (guest is default-deny): verify the sender, then `.sudo()`.
- `downgrades are not supported` at migrate → the DB ledger is ahead of the linked crate version.
- New model 404s → re-run migrate (the model's table and install ledger entry are created there).
"#;

const CLAUDE_MD: &str = r#"@AGENTS.md
"#;

// The project-local Claude Code skill: deep recipes (models, actions, services, jobs, routes,
// migrations) discovered automatically when an agent works in the generated app. Single source:
// packaged with this crate and embedded at compile time.
const SKILL_MD: &str = include_str!("../skill/SKILL.md");

const AGENT_DEF: &str = r#"---
name: kigumi-module-author
description: Implements features in this Kigumi app - models, ACLs, actions with numbering, computed fields, validation, cross-record services, background jobs, webhook routes, seeds and data migrations. Use for any business-logic change in the module crate.
tools: Read, Write, Edit, Bash, Grep, Glob
---

You implement features in a Kigumi application module. Read AGENTS.md and the kigumi skill
recipes first; all business declarations live in the module crate's src/lib.rs.

Rules: follow the seam idiom (one register_*! per capability, declared next to the model);
namespace every global (computes, jobs, sequence codes) with the module name; keep seeds,
migrations and jobs idempotent; gate permissions before any ctx.sudo(); verify signatures with
RouteInput::verify_hmac_sha256. Ship model/data changes with a MANIFEST version bump and a
register_migration! step.

Verify your work: cargo build (composition is checked by the compiler), cargo test -p <module>,
and remind the operator to re-run migrate after model changes.
"#;

const MODULE_TOML: &str = r#"[package]
name = "__APP__"
version = "1.0.0"
edition = "2021"
description = "__APP__ - a Kigumi application module"

[dependencies]
__KIGUMI_DEP__
serde_json = "1"
"#;

const MODULE_LIB_RS: &str = r#"//! __APP__: a Kigumi application module. This file is the whole surface a module needs —
//! models, access, actions and numbering — declared next to each other and collected by the
//! binary through inventory. Grow it by adding models and seams; the framework serves them.

use kigumi::prelude::*;

pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "__APP__",
    version: "1.0.0",
    framework: ">=0.2, <0.3",
    depends: &[ModuleDep { name: "base", req: "^1.0" }, ModuleDep { name: "mail", req: "^1.0" }],
    summary: "__APP__ application",
};
kigumi::register_module!(MANIFEST);

// Document numbering for tickets, assigned by the open action below and ensured at migrate.
kigumi::register_sequence!("__APP__", "TK", "TK/", "", 5);

/// A work ticket: the starter model — rename or replace it with your domain.
#[model(name = "__APP__.ticket", table = "__APP___ticket")]
pub struct Ticket {
    /// Assigned from the TK sequence when the ticket is opened.
    #[field(label = "Number")]
    name: Text,

    #[field(label = "Title", required)]
    title: Text,

    #[field(label = "State", default = "draft", selection = "draft:Draft,open:Open,done:Done")]
    state: Selection,

    #[field(label = "Assignee", target = "res.partner")]
    assignee_id: Many2one,

    #[field(label = "Notes")]
    notes: Text,
}
// Chatter thread: POST /api/__APP__.ticket/:id/message, GET /api/__APP__.ticket/:id/messages.
kigumi::register_mailed!("__APP__.ticket");

fn open_ticket(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "draft" => Ok(ActionOutcome::new()
            .set("state", Value::Str("open".to_string()))
            .assign_sequence("name", "TK")),
        s => Err(format!("can only open a draft ticket (state is '{s}')")),
    }
}
kigumi::register_action!("__APP__.ticket", "open", open_ticket, &["__APP__.user"]);

fn close_ticket(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "open" => Ok(ActionOutcome::new().set("state", Value::Str("done".to_string()))),
        s => Err(format!("can only close an open ticket (state is '{s}')")),
    }
}
kigumi::register_action!("__APP__.ticket", "close", close_ticket, &["__APP__.user"]);

static ACLS: [Acl; 2] = [
    Acl { model: "__APP__.ticket", group: "__APP__.user", read: true, write: true, create: true, delete: false },
    Acl { model: "__APP__.ticket", group: "admin", read: true, write: true, create: true, delete: true },
];
kigumi::register_acls!(&ACLS);

// The other seams, when you need them (each is one macro + one fn — see the framework docs):
//   register_compute!    stored/computed fields          register_constraint!  validation with field errors
//   register_service!    cross-record transactions       register_job!         background work with retries
//   register_route!      bespoke HTTP (webhooks, HMAC)   register_seed!        reference data at migrate
//   register_migration!  data migrations between module versions (bump MANIFEST.version)

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_compatible() {
        assert!(check_compat(&MANIFEST, FRAMEWORK_VERSION).is_ok());
    }

    #[test]
    fn ticket_resolves_and_produces_ddl() {
        let m = resolve_registered("__APP__.ticket").unwrap();
        assert!(to_ddl(&m).contains("title"));
    }
}
"#;

const APP_TOML: &str = r#"[package]
name = "app"
version = "0.1.0"
edition = "2021"
description = "__APP__ server binary on kigumi-runtime"

[dependencies]
__KIGUMI_DEP__
__RUNTIME_DEP__
__MCP_DEP__
__BASE_DEP__
__MAIL_DEP__
__EXTRA_DEPS__
__APP__ = { path = "../__APP__" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
"#;

const APP_MAIN_RS: &str = r#"//! The __APP__ server: migrate then serve, on kigumi-runtime.

use kigumi::prelude::*;

// Link the modules; inventory picks their registrations up at startup.
use kigumi_mod_base as _;
use kigumi_mod_mail as _;
__EXTRA_LINKS__
use __APP__ as _;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is required")?;
    let db = Db::connect(&url).await?;
    match std::env::args().nth(1).as_deref() {
        Some("migrate") => {
            kigumi_runtime::migrate(&db).await?;
            if let Ok(pw) = std::env::var("KIGUMI_ADMIN_PASSWORD") {
                if kigumi_runtime::bootstrap_admin(&db, &pw).await? {
                    println!("bootstrapped admin");
                }
            }
            println!("migrated");
            Ok(())
        }
        Some("serve") => {
            // ServeOptions is #[non_exhaustive]: build it with new() and assign what you need, so a
            // field added by a future framework release never breaks this file.
            let mut opts = kigumi_runtime::ServeOptions::new(
                std::env::var("KIGUMI_JWT_SECRET").map_err(|_| "KIGUMI_JWT_SECRET is required")?,
            );
            if let Ok(bind) = std::env::var("KIGUMI_BIND") {
                opts.bind = bind;
            }
            kigumi_runtime::serve(db, opts).await
        }
        // MCP over stdio: an AI agent operates this app AS the given user - ACLs and record
        // rules enforced on every tool by the data layer.
        Some("mcp") => {
            let login = std::env::args().nth(2).ok_or("usage: app mcp <login>")?;
            let server = kigumi_mcp::KigumiMcp::for_login(db, &login).await?;
            server.serve_stdio().await
        }
        _ => Err("usage: app <migrate|serve|mcp <login>>".into()),
    }
}
"#;

const README_MD: &str = r#"# __APP__

A [Kigumi](__REPO_URL__) application: a schema-driven, security-first business app served
headlessly from the models declared in `__APP__/src/lib.rs`.

## Quickstart

```sh
createdb __APP__
export DATABASE_URL=postgres://localhost/__APP__
export KIGUMI_JWT_SECRET=change-me
KIGUMI_ADMIN_PASSWORD=change-me cargo run -p app -- migrate
cargo run -p app -- serve   # http://127.0.0.1:8600 (override with KIGUMI_BIND)
```

## Tour (curl)

Auth lives at `/auth/*` (not `/api/auth/*`):

```sh
TOKEN=$(curl -s -X POST localhost:8600/auth/login \
  -H 'content-type: application/json' \
  -d '{"login":"admin","password":"change-me"}' | jq -r .access_token)

# Create a ticket, open it (state machine + TK/00001 numbering), close it.
curl -s -X POST localhost:8600/api/__APP__.ticket \
  -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"title":"First ticket"}'
curl -s -X POST localhost:8600/api/__APP__.ticket/1/action/open  -H "Authorization: Bearer $TOKEN"
curl -s -X POST localhost:8600/api/__APP__.ticket/1/action/close -H "Authorization: Bearer $TOKEN"

# Chatter, live events, machine-readable surface.
curl -s -X POST localhost:8600/api/__APP__.ticket/1/message \
  -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' -d '{"body":"On it."}'
curl -sN localhost:8600/api/events/stream -H "Authorization: Bearer $TOKEN"   # SSE
# The catalog follows the ACLs too: without a token you see only what the `public` group may read.
curl -s localhost:8600/openapi.json -H "Authorization: Bearer $TOKEN"
curl -s localhost:8600/api/__APP__.ticket/view -H "Authorization: Bearer $TOKEN"  # UI contract
```

## Where things go

- Models, ACLs, actions, sequences: `__APP__/src/lib.rs` — one file is fine for a long time.
- Validation: `register_constraint!` (structured 400s with per-field messages).
- Cross-record transactions: `register_service!`; background work: `register_job!` (retries,
  transactional enqueue from services); webhooks/bespoke HTTP: `register_route!` (HMAC helper).
- Reference data: `register_seed!` (idempotent, runs at migrate).
- Upgrades: bump `MANIFEST.version` and ship `register_migration!("__APP__", "1.1.0", step)` —
  migrate applies pending steps in order and records progress per step.

The server binary (`app/src/main.rs`) is four runtime calls; it should rarely change.
"#;

fn render(template: &str, opts: &ScaffoldOptions) -> String {
    let (kigumi_dep, runtime_dep, mcp_dep, base_dep, mail_dep) = (
        dep(&opts.framework, "kigumi", "crates/kigumi", "0.2"),
        dep(&opts.framework, "kigumi-runtime", "crates/kigumi-runtime", "0.2"),
        dep(&opts.framework, "kigumi-mcp", "crates/kigumi-mcp", "0.2"),
        dep(&opts.framework, "kigumi-mod-base", "modules/base", "2.0"),
        dep(&opts.framework, "kigumi-mod-mail", "modules/mail", "2.0"),
    );
    let extra_deps: String = opts
        .extra_modules
        .iter()
        .map(|m| dep(&opts.framework, &format!("kigumi-mod-{m}"), &format!("modules/{m}"), "2.0") + "\n")
        .collect();
    let extra_links: String = opts
        .extra_modules
        .iter()
        .map(|m| format!("use kigumi_mod_{m} as _;\n"))
        .collect();
    let repo_url = match &opts.framework {
        FrameworkSource::Git(url) => url.trim_end_matches(".git").to_string(),
        _ => "https://github.com/vpescete/kigumi".to_string(),
    };
    // __APP__ FIRST: the dep lines carry a user-supplied filesystem path that may itself contain
    // the literal token (review finding) — substituted afterwards, they are never rescanned.
    template
        .replace("__APP__", &opts.name)
        .replace("__KIGUMI_DEP__", &kigumi_dep)
        .replace("__RUNTIME_DEP__", &runtime_dep)
        .replace("__MCP_DEP__", &mcp_dep)
        .replace("__BASE_DEP__", &base_dep)
        .replace("__MAIL_DEP__", &mail_dep)
        .replace("__EXTRA_DEPS__", extra_deps.trim_end())
        .replace("__EXTRA_LINKS__", extra_links.trim_end())
        .replace("__REPO_URL__", &repo_url)
}

/// Writes the workspace under `dest` (which must not already exist).
pub fn scaffold(dest: &Path, opts: &ScaffoldOptions) -> Result<(), String> {
    // Defense in depth for the CLI-side name guard: an empty/degenerate ident would make `dest`
    // the CWD (its exists() is false for "") and the module paths absolute (review must-fix).
    if opts.name.is_empty() || opts.name.chars().all(|c| c == '_') {
        return Err(format!("'{}' is not a usable app name", opts.name));
    }
    if dest.as_os_str().is_empty() || dest.exists() {
        return Err(format!("'{}' already exists — refusing to write into it", dest.display()));
    }
    let files: [(&str, &str); 10] = [
        ("Cargo.toml", WORKSPACE_TOML),
        (".gitignore", GITIGNORE),
        ("README.md", README_MD),
        ("AGENTS.md", AGENTS_MD),
        ("CLAUDE.md", CLAUDE_MD),
        (".claude/skills/kigumi/SKILL.md", SKILL_MD),
        (".claude/agents/kigumi-module-author.md", AGENT_DEF),
        ("__MOD__/Cargo.toml", MODULE_TOML),
        ("__MOD__/src/lib.rs", MODULE_LIB_RS),
        ("app/Cargo.toml", APP_TOML),
    ];
    for (rel, template) in files {
        let rel = rel.replace("__MOD__", &opts.name);
        let path = dest.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, render(template, opts)).map_err(|e| format!("write {rel}: {e}"))?;
    }
    let main = dest.join("app/src/main.rs");
    std::fs::create_dir_all(main.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&main, render(APP_MAIN_RS, opts)).map_err(|e| format!("write app/src/main.rs: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(name: &str, extras: &[&str]) -> ScaffoldOptions {
        ScaffoldOptions {
            name: name.to_string(),
            extra_modules: extras.iter().map(|s| s.to_string()).collect(),
            framework: FrameworkSource::Git("https://github.com/vpescete/kigumi.git".to_string()),
        }
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_name("My App-2"), "my_app_2");
        assert_eq!(sanitize_name("2fast"), "_2fast");
        assert_eq!(sanitize_name(".."), "__");
        assert_eq!(sanitize_name(""), "");
    }

    #[test]
    fn refuses_degenerate_names() {
        // An empty ident would make dest the CWD and the module paths absolute (review must-fix).
        for name in ["", "__"] {
            let err = scaffold(Path::new(name), &opts(name, &[])).unwrap_err();
            assert!(err.contains("not a usable app name"), "got: {err}");
        }
    }

    #[test]
    fn framework_path_containing_the_token_survives_rendering() {
        let dest = std::env::temp_dir().join(format!("kigumi_new_tok_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let o = ScaffoldOptions {
            name: "demo".to_string(),
            extra_modules: vec![],
            framework: FrameworkSource::Path(PathBuf::from("/checkout/__APP__/framework")),
        };
        scaffold(&dest, &o).unwrap();
        let toml = std::fs::read_to_string(dest.join("app/Cargo.toml")).unwrap();
        assert!(toml.contains("/checkout/__APP__/framework/crates/kigumi"), "dep path untouched");
        std::fs::remove_dir_all(&dest).unwrap();
    }

    #[test]
    fn crates_io_deps_render_as_versions() {
        let dest = std::env::temp_dir().join(format!("kigumi_new_cio_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let o = ScaffoldOptions {
            name: "demo".to_string(),
            extra_modules: vec!["sales".to_string()],
            framework: FrameworkSource::CratesIo,
        };
        scaffold(&dest, &o).unwrap();
        let toml = std::fs::read_to_string(dest.join("app/Cargo.toml")).unwrap();
        assert!(toml.contains("kigumi = \"0.2\""), "framework crates pinned to 0.2: {toml}");
        assert!(toml.contains("kigumi-mod-sales = \"2.0\""), "modules pinned to 2.0");
        std::fs::remove_dir_all(&dest).unwrap();
    }

    /// The scaffolded main must never construct ServeOptions as a struct literal: the struct is
    /// #[non_exhaustive], so a literal does not even compile from the generated crate (E0639), and
    /// the whole point of the constructor is that a field added by a later framework release is a
    /// no-op here instead of a compile error in every adopter binary.
    #[test]
    fn scaffolded_main_builds_serve_options_through_the_constructor() {
        let dest = std::env::temp_dir().join(format!("kigumi_new_so_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        scaffold(&dest, &opts("demoapp", &["sales"])).unwrap();
        let main_rs = std::fs::read_to_string(dest.join("app/src/main.rs")).unwrap();
        assert!(main_rs.contains("ServeOptions::new("), "constructed via new(): {main_rs}");
        assert!(!main_rs.contains("ServeOptions {"), "no exhaustive struct literal: {main_rs}");
        std::fs::remove_dir_all(&dest).unwrap();
    }

    #[test]
    fn closure_pulls_sales_for_stock() {
        let c = module_closure(&["stock".to_string()]);
        assert_eq!(c, vec!["sales".to_string(), "stock".to_string()]);
        assert!(validate_extras(&["bogus".to_string()]).is_err());
    }

    #[test]
    fn scaffold_writes_a_complete_placeholder_free_tree() {
        let dest = std::env::temp_dir().join(format!("kigumi_new_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        scaffold(&dest, &opts("demoapp", &["sales"])).unwrap();
        for rel in [
            "Cargo.toml",
            ".gitignore",
            "README.md",
            "AGENTS.md",
            "CLAUDE.md",
            ".claude/skills/kigumi/SKILL.md",
            ".claude/agents/kigumi-module-author.md",
            "demoapp/Cargo.toml",
            "demoapp/src/lib.rs",
            "app/Cargo.toml",
            "app/src/main.rs",
        ] {
            let content = std::fs::read_to_string(dest.join(rel)).unwrap_or_else(|_| panic!("{rel} missing"));
            assert!(!content.contains("__APP__"), "{rel} has unexpanded placeholders");
            assert!(!content.contains("__KIGUMI"), "{rel} has unexpanded dep placeholders");
            assert!(!content.contains("__EXTRA"), "{rel} has unexpanded extras");
        }
        let app_toml = std::fs::read_to_string(dest.join("app/Cargo.toml")).unwrap();
        assert!(app_toml.contains("kigumi-mod-sales"), "extra module dep present");
        let main = std::fs::read_to_string(dest.join("app/src/main.rs")).unwrap();
        assert!(main.contains("use kigumi_mod_sales as _;"), "extra module linked");
        // Refuses to overwrite.
        assert!(scaffold(&dest, &opts("demoapp", &[])).is_err());
        std::fs::remove_dir_all(&dest).unwrap();
    }
}
