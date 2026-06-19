# Overview and quickstart

Meshble is a **headless**, **schema-driven** application framework written in Rust: a single **model definition** is the sole source of truth, and from it the framework **generates** the Postgres schema, the REST API, the OpenAPI specification, the agnostic JSON **UI contract**, and the security policy (ACLs and record rules). The core imposes neither a frontend nor an application protocol: everything is exposed through standards generated from the schema. Module composition is **resolved and verified at compile time** — the modules available in a binary are those whose crates are linked, and their dependency graph is validated before the database is ever touched. This page explains what Meshble is, describes its architecture, and provides a complete quickstart to take an instance from zero to an authenticated REST API, with a reference web app.

## What Meshble is

The central principle is **one model, one source of truth**. You describe an entity once, in Rust, with the `#[model]` macro; from that description (`ModelDescriptor`) the framework projects:

| Generated artifact | Responsible crate | What it is for |
|---|---|---|
| Postgres schema (DDL + versioned migrations) | `meshble-schema` / `meshble-db` | tables, columns, FKs, constraints |
| Headless REST API | `meshble-server` | secure CRUD and actions from the catalog |
| OpenAPI 3.1 specification | `meshble-schema` | `GET /openapi.json` |
| Agnostic UI contract (JSON) | `meshble-schema` | a generic frontend draws forms and tables *from the contract* |
| Security policy (ACLs + record rules) | `meshble-core` | access enforced by the code path, not just by the data |

Core characteristics:

- **Agnostic**: the core knows nothing about the frontend or the transport; the web app is just a consumer of the UI contract and the REST API.
- **Compile-time composition**: a module is a Rust crate that auto-registers its own models, ACLs, and record rules in the **compile-time registry** (via `inventory`). Only modules linked into the binary are available; the dependency graph is validated by `resolve_modules`, with a SemVer compatibility check against the framework version (`check_compat` / `FRAMEWORK_VERSION`).
- **Multi-company**: `res.company` is the unit of data isolation; per-company scoping is enforced by the security layer (`Ctx` with the active company plus a record rule).
- **Fail-fast on secrets**: the instance refuses to start if a required secret is missing (`DATABASE_URL`, `MESHBLE_JWT_SECRET`).

## Architecture map

The Cargo workspace (`Cargo.toml`, `members = ["crates/*", "modules/*", "apps/*"]`) is organized in three layers: the framework **crates**, the business **modules**, and the executable **apps**. There is also a reference web app under `web/`.

### Crates (`crates/`)

| Crate | Role |
|---|---|
| `meshble-core` | introspectable metamodel, AST domains, security (ACLs + record rules + sudo), compile-time registry, versioning |
| `meshble-macros` | `#[model]` / `#[extend]` proc-macros |
| `meshble-schema` | projections: Postgres DDL, JSON UI contract, OpenAPI 3.1 |
| `meshble-db` | Postgres persistence (sqlx): security-enforced CRUD + versioned migrations |
| `meshble-auth` | JWT HS256 auth (Bearer → trusted `Ctx`), password hashing |
| `meshble-config` | typed, validated boot-time configuration + reading secrets from the environment |
| `meshble-storage` | content-addressed blob store behind a trait (`BlobStore`) for attachments |
| `meshble-server` | headless axum server: metadata + CRUD from the catalog |
| `meshble` | facade (prelude): a module depends only on this crate |

The public prelude lives in `crates/meshble/src/lib.rs`: a module opens its definition with

```rust
use meshble::prelude::*;
```

and receives everything it needs — `#[model]`, `#[extend]`, `Ctx`, `Domain`, `Model`, `ModelDescriptor`, `ModuleManifest`, `ModuleDep`, the security types (`Acl`, `RecordRule`), and the registrars exposed as macros by the `meshble` crate (`register_module!`, `register_acls!`, `register_rules!`, `register_action!`, and the like).

### Modules (`modules/`)

Each module declares a `MANIFEST` (`ModuleManifest`) with a name, a version, a framework compatibility range (the `framework` field), and dependencies (`depends`), and auto-registers with `meshble::register_module!(MANIFEST)`.

| Module | Version | Depends on | Summary (from the manifest) |
|---|---|---|---|
| `base` | `1.0.0` | — | Foundational models: currency, partner, company |
| `mail` | `1.0.0` | `base` | Headless chatter: messages, tracking, followers, activities |
| `sales` | `1.0.0` | `base`, `mail` | Sales order management |
| `account` | `1.0.0` | `base`, `mail` | Double-entry general ledger |
| `stock` | `1.0.0` | `base`, `sales`, `mail` | Inventory — locations, quants, pickings and moves |

`base` is the root of the graph (no dependencies) and cannot be uninstalled. All modules declare `framework: ">=0.1, <0.2"`. The dependencies in the table are those declared in the `MANIFEST`; installing a module resolves its **transitive closure** (for example, installing `stock` pulls in `sales`, and therefore `base` and `mail`).

### Apps (`apps/`)

| App | Binary | Role |
|---|---|---|
| `meshble-cli` | `meshble` | the single CLI to operate an instance: `serve`, `migrate`, `config`, `user`, `acl`, `rule`, `module`, `version` |
| `renderer-demo` | `meshble-renderer-demo` | runnable demo: migrates + seeds a model and serves the API + a reference renderer |

The `meshble` CLI links the module crates (`meshble-mod-base`, `meshble-mod-mail`, `meshble-mod-sales`, `meshble-mod-account`, `meshble-mod-stock`); precisely because they are linked, their `inventory` registrations are present in the binary and the modules turn out to be *available* for installation. Only **installed** modules, however, are migrated and served.

### Web (`web/`)

`web/` is a Vite/React SPA: the reference web app for the admin UI. In development it runs as a separate process on port `5180` and proxies the `/api`, `/auth`, and `/openapi.json` paths to `meshble serve` (default `127.0.0.1:8099`), so the browser stays same-origin (no CORS). The Rust server is headless and does **not** serve static assets: the web app is a separate client of the UI contract and the REST API.

## Quickstart

This path takes an instance from zero to an authenticated REST API, with a business module installed and the reference web app opened.

### Prerequisites

- **Rust toolchain** (stable, edition 2021). Install with [rustup](https://rustup.rs):
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **PostgreSQL** running and reachable, with the `createdb`/`psql` client commands available.
- **Node.js** + npm (only if you want to open the reference web app under `web/`).

### 1. Build the `meshble` binary

From the workspace root:

```bash
cargo build --release -p meshble-cli
```

The produced binary is `target/release/meshble`. In the examples below you can use `cargo run -p meshble-cli -- <command>` during development, or `meshble <command>` if the binary is on the `PATH`.

### 2. Create the database

`meshble` connects to an **already existing** database (it does not create it): create it separately.

```bash
createdb meshble
```

### 3. Configure the secrets (env)

Secrets are read **only** from the environment, never from the configuration file; the instance fails fast if a required one is missing. Required are `DATABASE_URL` (a complete Postgres DSN: the sole source of connection identity) and `MESHBLE_JWT_SECRET` (the HS256 secret for signing tokens).

```bash
export DATABASE_URL="postgres://meshble:CHANGE_ME@127.0.0.1:5432/meshble"
export MESHBLE_JWT_SECRET="$(openssl rand -hex 32)"
```

`DATABASE_URL` must be a valid `postgres://` URL, otherwise secret validation fails at startup.

### 4. Prepare `meshble.toml`

Non-secret boot-time settings live in `meshble.toml` (default: `./meshble.toml`, or `$MESHBLE_CONFIG`, or `--config <path>`). The effective configuration is given by the layering `defaults < meshble.toml < environment variables with the MESHBLE_CONF_ prefix` (nested with `__`, for example `MESHBLE_CONF_SERVER__BIND=0.0.0.0:9000`). The file is optional for commands that do not start the blob store, but `serve` requires `storage.path` when the storage backend is `fs` (the default): without it, validation fails fast. Start from the example:

```bash
cp meshble.toml.example meshble.toml
```

A minimal `meshble.toml` for the quickstart:

```toml
[server]
bind = "127.0.0.1:8099"

[storage]
backend = "fs"
path = "/var/lib/meshble/blobs"
```

> In v1 only the `fs` storage backend is implemented (`FsBlobStore`, content-addressed files). The `s3` value is foreseen by the configuration schema but not yet implemented.

Verify the effective configuration (secrets redacted) with:

```bash
meshble config check
meshble config print
```

`config print` prints the effective configuration with secrets masked and, at the end, the runtime settings read from the database.

### 5. Migrate the catalog

```bash
meshble migrate
```

`migrate` ensures the framework schemas (auth, sequences, settings, accesses, modules) and, on a fresh database, automatically installs only `base` (plus its dependency closure). The other modules are opt-in. The migration creates the model tables of the installed modules in FK dependency order and seeds the reference data of `base` (a `EUR` currency and a default company).

### 6. Install a business module

The available modules are only those linked into the binary; installation resolves the **dependency closure** (dependencies before dependents) and migrates their tables, idempotently. List the modules and install `sales`:

```bash
meshble module list
meshble module install sales
```

Installing `sales` also pulls in `mail` (its dependency, in addition to `base`, which is already present). The output of `module list` shows, for each module, the name, version, status (`installed`/`available`), and summary.

### 7. Bootstrap the admin

On a fresh instance, the `admin` user is created from the `MESHBLE_ADMIN_PASSWORD` variable (no password is hardcoded). The bootstrap happens inside `serve`: if an `admin` user does not already exist and the variable is set, it is created with all the groups declared by the ACLs/record rules of the linked modules plus the base `user`/`admin` groups, and assigned to every existing company.

```bash
export MESHBLE_ADMIN_PASSWORD="$(openssl rand -base64 24)"
```

> If `MESHBLE_ADMIN_PASSWORD` is not set, `serve` still starts but prints a warning and no admin is created.

### 8. Start the server

```bash
meshble serve
```

`serve` runs in sequence: `migrate` → admin bootstrap → starting the axum server. At startup it prints the listening URL and the number of models registered in the binary, for example:

```
meshble serving on http://127.0.0.1:8099  (N models)
```

> The printed number is the total of **registered** models (that is, of all linked modules). The API, however, serves only the models of **installed** modules: a model whose module is not installed does not appear in the catalog exposed by the router. The effective access is the union of the compiled baseline and any runtime overrides (ACLs and record rules) present in the database.

### 9. Call the REST API with curl

First obtain an access token by logging in (`POST /auth/login`, body `{login, password}`); the response is `{ access_token, refresh_token, token_type, expires_in }` with `token_type` equal to `"Bearer"`. Then use the token as `Authorization: Bearer` on the data routes.

```bash
# 1) login → extract the access token
TOKEN=$(curl -s -X POST http://127.0.0.1:8099/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"login":"admin","password":"'"$MESHBLE_ADMIN_PASSWORD"'"}' \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["access_token"])')

# 2) list the records of a model (envelope: data/total/limit/offset)
curl -s http://127.0.0.1:8099/api/res.partner \
  -H "Authorization: Bearer $TOKEN"
```

The list responds with an envelope `{ "data": [...], "total": N, "limit": ..., "offset": ... }`. Other useful routes, all mounted from the catalog:

| Route | Method | What it does |
|---|---|---|
| `/openapi.json` | GET | generated OpenAPI 3.1 specification |
| `/api/models` | GET | list of served models |
| `/api/:name/view` | GET | UI contract of the model (form + table) |
| `/api/:name` | GET / POST | list (paginated) / create |
| `/api/:name/:id` | GET / PATCH / DELETE | read / update / delete a record |
| `/api/:name/:id/action/:action` | POST | runs a registered state-transition action |
| `/auth/login`, `/auth/refresh`, `/auth/logout`, `/auth/me` | — | JWT authentication flow |

### 10. Open the reference web app

The web app under `web/` runs as a separate process and, in development, proxies to the running server:

```bash
cd web
npm install
npm run dev      # http://localhost:5180
```

With `meshble serve` running on `127.0.0.1:8099`, the `/api`, `/auth`, and `/openapi.json` paths are proxied to the backend, so the web app's calls reach the real API from the same origin.

## Guide index

| Page | Content |
|---|---|
| [README.md](README.md) | This page: framework overview and end-to-end quickstart. |
| [architettura.md](architettura.md) | Architecture in detail: crates, metamodel, artifact generation, compile-time composition flow. |
| [installazione.md](installazione.md) | Full installation: prerequisites, build, database creation, first run. |
| [configurazione.md](configurazione.md) | Configuration: `meshble.toml`, secrets via the environment, `MESHBLE_CONF_*` overrides, runtime settings in the database. |
| [moduli.md](moduli.md) | The included modules (`base`, `mail`, `sales`, `account`, `stock`): exposed models, dependencies, install/uninstall. |
| [moduli-custom.md](moduli-custom.md) | How to write a module: `#[model]`, manifest, ACLs, record rules, actions, registration in the catalog. |
| [api.md](api.md) | The REST API and OpenAPI: routes, response envelope, UI contract, JWT authentication. |
| [sicurezza.md](sicurezza.md) | Security model: ACLs, record rules, groups, sudo, multi-company, additive runtime overrides. |
