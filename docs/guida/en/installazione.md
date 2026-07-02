# Environment installation

This guide describes the installation and deployment of a Kigumi instance, from source code all the way to a production server. Kigumi is a headless, schema-driven application framework written in Rust: it compiles into a single binary (`kigumi`) that migrates the catalog, applies security, and serves the API. The path is always the same: you compile the binary, provision a PostgreSQL database, supply the secrets through the environment, write the non-secret configuration file, and finally bring the instance up with the sequence `migrate` → `module install` → `serve`. For a product overview see [README.md](README.md); for the architecture [architettura.md](architettura.md); for the details of every configuration key [configurazione.md](configurazione.md).

## Prerequisites

| Component | Requirement |
|---|---|
| Rust toolchain | A recent stable toolchain. The workspace pins `edition = "2021"`. Install via [rustup](https://rustup.rs). |
| PostgreSQL | A reachable, already-running PostgreSQL server; `DATABASE_URL` is the only connection identity. The `createdb`/`psql` client commands come in handy for creating the database. |
| Node.js + npm | Only for the optional web frontend in `web/` (Vite 5, React 18). |

> The workspace declares `edition = "2021"` but does not pin an explicit `rust-version` (MSRV); use a recent stable toolchain. See the **Uncertainties** at the end.

## Creating an application (`kigumi new`)

The recommended path for building your own vertical: scaffold an out-of-tree workspace with the `kigumi` CLI (from a framework checkout today; `cargo install kigumi-cli` once published):

```bash
kigumi new myshop            # asks which extra modules to include (sales, account, stock)
cd myshop
createdb myshop
export DATABASE_URL=postgres://localhost/myshop
export KIGUMI_JWT_SECRET=change-me
KIGUMI_ADMIN_PASSWORD=change-me cargo run -p app -- migrate
cargo run -p app -- serve    # http://127.0.0.1:8600 (override with KIGUMI_BIND)
```

The workspace contains a module crate (a starter ticket model — see [moduli-custom.md](moduli-custom.md)) and a ~45-line server binary on `kigumi-runtime`, which owns the operational wiring: framework schemas, module install with data-migration replay, reference-data seeding, admin bootstrap, the cron/job workers, and the static-catalog server. `migrate` is idempotent — run it on every deploy; it also applies any pending `register_migration!` steps.

The rest of this page covers operating the framework repository itself (the full `kigumi` CLI with its configuration file, dynamic module install, and the admin SPA).

## Getting the source and building

Clone the repository and build the workspace in release mode:

```bash
git clone https://github.com/vpescete/kigumi
cd kigumi
cargo build --release
```

The operational binary is named `kigumi` and is produced by the `kigumi-cli` crate, which explicitly declares the bin name in its own `Cargo.toml`:

```toml
[[bin]]
name = "kigumi"
path = "src/main.rs"
```

`cargo build --release` builds the entire workspace. To build only the CLI you can narrow the target to the package:

```bash
cargo build --release -p kigumi-cli
```

In both cases, once finished the binary is located at:

```
target/release/kigumi
```

All the commands below use this binary. In the examples it is abbreviated as `kigumi`; in a real environment invoke it with the full path `target/release/kigumi`, or copy it to a directory on the `PATH`. During development you can also use `cargo run -p kigumi-cli -- <command>`.

The application modules (`base`, `mail`, `sales`, `account`, `stock`) are statically linked into the binary through their respective crates: their models, ACLs, and record rules self-register in the registry at compile time (via `inventory`). Only the modules whose crate is linked into the binary are available for installation at runtime — see [moduli.md](moduli.md).

Check the framework version and the linked modules:

```bash
kigumi version
```

The command prints the framework version and, line by line, the linked modules with their version (e.g. `module base 1.0.0`).

## Preparing the database

`kigumi` connects to an **already existing** database — it does not create one. Create it separately:

```bash
createdb kigumi
```

There is no need to run DDL by hand: `kigumi migrate` (and `kigumi serve`) create and version all the schemas. It is enough for the database to exist and for the user in `DATABASE_URL` to be able to create tables in it.

### Format of the `DATABASE_URL` DSN

`DATABASE_URL` is a complete PostgreSQL DSN and is the **only** source of the connection identity (host, port, database, user, password, sslmode). The value is validated at startup: it must be a URL with the `postgres://` or `postgresql://` scheme, otherwise the instance refuses to start with `"DATABASE_URL is not a valid postgres:// URL"`.

```bash
# general form
DATABASE_URL=postgres://UTENTE:PASSWORD@HOST:PORTA/NOME_DB

# example
DATABASE_URL=postgres://kigumi:CHANGE_ME@127.0.0.1:5432/kigumi
```

The `[database]` section of the configuration file contains **only** tuning parameters (`pool_max`, `connect_timeout`): there is no `host`/`name` field there, precisely to avoid an ambiguous overlap with the DSN. Putting a `host` in `[database]` is an unknown key and triggers a fail-fast.

## Required secrets and environment variables

Secrets must **never** go into the `kigumi.toml` file: they are supplied exclusively from the environment (or from a secret manager), and their presence is verified at startup. The instance fails fast if a required secret is missing.

| Variable | Required | Purpose |
|---|---|---|
| `DATABASE_URL` | **Required** | Complete PostgreSQL DSN: connection identity. |
| `KIGUMI_JWT_SECRET` | **Required** | HS256 signing secret for access/refresh tokens. |
| `KIGUMI_ADMIN_PASSWORD` | For bootstrap | Password of the `admin` user created on the first `serve` (see below). |
| `KIGUMI_SMTP_PASSWORD` | Conditional | Required **only** if `[mail].smtp_host` is configured; otherwise loading the `Settings` fails. |
| `KIGUMI_JWT_SECRET_OLD` | Optional | Previous JWT secret, **reserved** for rotation: in v1 it is loaded into the configuration but not yet passed to the verifier (the `Authenticator` receives only `KIGUMI_JWT_SECRET`). |
| `KIGUMI_ADMIN_TOKEN` | Optional | Secret **reserved** for the future protection of destructive database operations (dump/restore/gc): in v1 it is loaded but the enforcement is not yet wired up (the endpoints do not exist yet). |
| `KIGUMI_NEW_PASSWORD` | Optional | Password for `kigumi user create` / `set-password` when `--password` is not provided. |

`DATABASE_URL` and `KIGUMI_JWT_SECRET` are the two absolutely **required** variables: reading the environment fails if either of them is missing or empty. The SMTP cross-check is explicit: if `mail.smtp_host` is present in the configuration but `KIGUMI_SMTP_PASSWORD` is missing, loading the `Settings` returns the error `"mail.smtp_host is set but KIGUMI_SMTP_PASSWORD is not"`.

The `.env.example` file in the repository lists the secrets as a template. A minimal example to get started:

```bash
export DATABASE_URL=postgres://kigumi:CHANGE_ME@127.0.0.1:5432/kigumi
export KIGUMI_JWT_SECRET="$(openssl rand -hex 32)"
export KIGUMI_ADMIN_PASSWORD="$(openssl rand -base64 24)"
```

To inspect the effective configuration with the secrets redacted (the DSN password is masked, while host/db/user remain visible; the other secrets appear as `set (****)` / `unset`):

```bash
kigumi config check    # validates the effective configuration
kigumi config print    # prints the redacted config + the runtime settings from the db
```

## The configuration file

The non-secret (boot-time) settings live in a TOML file. Copy the provided template and adapt it:

```bash
cp kigumi.toml.example kigumi.toml
```

### File path resolution

The binary resolves the file path in this order:

1. the global flag `--config <path>` (available on every subcommand);
2. the environment variable `KIGUMI_CONFIG`;
3. the default `./kigumi.toml` in the current directory.

```bash
kigumi --config /etc/kigumi/kigumi.toml serve
# or
export KIGUMI_CONFIG=/etc/kigumi/kigumi.toml
kigumi serve
```

### Layering and environment overrides

The configuration is composed in layers, from the least to the most prioritized:

```
defaults < kigumi.toml < KIGUMI_CONF_* environment variables
```

The `KIGUMI_CONF_*` variables map onto the file's sections using the **double underscore** `__` as the nesting separator. The `KIGUMI_CONF_` prefix is deliberately kept distinct from the secret variables (`DATABASE_URL`, `KIGUMI_JWT_SECRET`, …), so that secrets are never captured by the configuration layer.

```bash
# equivalent to [server] bind = "0.0.0.0:9000"
export KIGUMI_CONF_SERVER__BIND=0.0.0.0:9000
```

Validation is fail-fast: an unknown section or an unknown key in a core section makes startup refuse (instead of silently ignoring typos). The `[modules.<name>]` subtrees, on the other hand, are **open**: they are captured verbatim and validated by the owning module.

### Main keys of the template

```toml
[instance]
name = "acme-prod"

[server]
bind = "0.0.0.0:8099"          # host:port the server listens on (validated as a SocketAddr)
workers = 8
proxy_mode = true              # see production notes

[database]                     # tuning ONLY — the identity is the DATABASE_URL DSN
pool_max = 10
connect_timeout = "5s"

[storage]
backend = "fs"                 # fs | s3
path = "/var/lib/kigumi/blobs"

[auth]
access_ttl = 900               # 15 min (JWT secret via env KIGUMI_JWT_SECRET)
refresh_ttl = 2592000          # 30 days

[mail]
smtp_host = "smtp.acme.com"    # SMTP password via env KIGUMI_SMTP_PASSWORD
smtp_port = 587
from = "erp@acme.com"
```

Two keys are load-bearing for bringing the instance into operation:

- **`server.bind`** — the `host:port` address the server listens on. It must be a valid `host:port` (default `127.0.0.1:8099`); a value that cannot be parsed as a socket makes validation fail with `"server.bind is not a host:port"`.
- **`storage.path`** — the root of the filesystem blob store. With `backend = "fs"` (the default), `storage.path` is **required**: validation rejects `backend = fs` without `path` (`"storage.backend = fs requires storage.path"`), and `serve` returns `"storage.path is required for the fs blob store"` if it is missing. With `backend = "s3"`, it is instead `storage.bucket` that is required by validation (`"storage.backend = s3 requires storage.bucket"`).

For the complete details of every key see [configurazione.md](configurazione.md).

## Bring-up sequence

Bringing up a fresh instance follows three steps. Each one is idempotent and can be re-run.

### 1. `kigumi migrate`

```bash
kigumi migrate
```

Ensures the framework schemas (auth, sequences, settings, accesses, modules) and then migrates the models of the **installed** modules. On a truly fresh database there is no module installed yet: in that case `migrate` automatically installs only `base` and its dependency closure (the other modules are opt-in). It migrates their tables in FK dependency order, creates the Many2many relation tables in a second pass, and — if `base` is installed — seeds the minimal reference data (an `EUR` currency and a default `Main Company`, plus the read-only list of the groups referenced by ACLs/record rules). It also seeds the default runtime settings (`base_url`, `mode = production`) without ever overwriting an operator's change.

> Upgrade note: if the database was migrated **before** module selection existed (its per-model registry already has rows), `migrate` keeps **all** the modules it had, so the upgrade does not silently hide models that were previously available.

### 2. `kigumi module install <NAME>`

```bash
kigumi module install sales
```

Installs a module **and its dependency closure** (the dependencies first), then migrates their tables (idempotent). Modules that are already installed are skipped. Installing `sales`, for example, also pulls in `mail` (in addition to the already-present `base`). Related commands:

```bash
kigumi module list               # lists the linked modules with version, status, and summary
kigumi module install account    # installs account + dependencies, then migrates
kigumi module uninstall sales    # stops migrating/serving the module; tables and data remain
```

`base` cannot be uninstalled. Uninstalling a module is rejected if another installed module still depends on it. For the module model see [moduli.md](moduli.md), and for custom modules [moduli-custom.md](moduli-custom.md).

### 3. `kigumi serve`

```bash
kigumi serve
```

`serve` performs three things in sequence and then keeps listening:

1. **re-migrates** the installed modules (it internally invokes the same `migrate`), so starting the server always aligns the schema;
2. **bootstraps the admin** from `KIGUMI_ADMIN_PASSWORD` (see below);
3. **serves** the secured API on `server.bind`, exposing only the models of the installed modules.

At startup the server merges the ACL/record-rule baseline compiled into the binary with any runtime overrides present in the database, starts a background scheduler for the registered cron jobs, and initializes the filesystem blob store from the `storage.path` root. It prints the URL it is serving on and the number of exposed models:

```
kigumi serving on http://127.0.0.1:8099  (N models)
```

The main exposed routes include `/openapi.json`, `/api/models`, `/api/:name/view`, the CRUD `/api/:name` and `/api/:name/:id`, authentication `/auth/login` · `/auth/refresh` · `/auth/logout` · `/auth/me`, and the health checks `/health` · `/ready`. For the API and security see [api.md](api.md) and [sicurezza.md](sicurezza.md).

### Complete example sequence

```bash
export DATABASE_URL=postgres://kigumi:CHANGE_ME@127.0.0.1:5432/kigumi
export KIGUMI_JWT_SECRET="$(openssl rand -hex 32)"
export KIGUMI_ADMIN_PASSWORD="$(openssl rand -base64 24)"
export KIGUMI_CONFIG=/etc/kigumi/kigumi.toml

createdb kigumi
kigumi migrate                 # framework schemas + base + its closure
kigumi module install sales    # desired application modules (+ dependencies)
kigumi serve                   # re-migrate, bootstrap admin, serve
```

## Admin bootstrap

On the first `serve`, if an `admin` user does not already exist, the binary creates one from `KIGUMI_ADMIN_PASSWORD` (the password is never hardcoded). If `KIGUMI_ADMIN_PASSWORD` is not set, the bootstrap is skipped with the warning `"warning: no admin user; set KIGUMI_ADMIN_PASSWORD to bootstrap one"` and no admin is created.

The bootstrapped admin receives **all** the groups declared by the linked modules (via ACL/record rule) plus the base groups `user` and `admin`, and is assigned to all existing companies as its multi-company scope (with the first one as the active company), so that a freshly created instance can operate every module. The password is stored as a hash (argon2). Companies created later must be granted to the admin explicitly.

Alternatively, you can manage users via the CLI without going through the automatic bootstrap:

```bash
kigumi user create alice --password 's3cret' --groups user,sales
kigumi user set-password alice --password 'nuova'
kigumi user grant alice admin
kigumi user company alice --active 1 --allowed 1,2
```

The password can also come from `KIGUMI_NEW_PASSWORD` instead of `--password`.

## Web frontend

The administration frontend is an optional React/Vite SPA in `web/`. In development it is a process separate from the Rust server and routes API calls to a live `kigumi serve` instance.

```bash
cd web
npm install
npm run dev        # development server on http://localhost:5180
```

The Vite dev server listens on port **5180** and proxies to the running `kigumi serve` (default `127.0.0.1:8099`), so the browser stays same-origin and CORS is not needed. The forwarded paths are:

```ts
proxy: {
  '/api': 'http://127.0.0.1:8099',
  '/auth': 'http://127.0.0.1:8099',
  '/openapi.json': 'http://127.0.0.1:8099',
}
```

So in development, start the backend (`kigumi serve`) and the frontend (`npm run dev`) in parallel.

For the production build:

```bash
cd web
npm run build      # produces the static assets in web/dist/
```

The static assets produced in `web/dist/` are to be served by a web server / reverse proxy, routing `/api`, `/auth`, and `/openapi.json` to the Kigumi backend. The Rust server is headless and does **not** serve static assets.

> Note: the SPA in `web/` runs by default on in-memory mock data (navigable design-system mockups) and does not require the backend to be browsed; the proxy to `kigumi serve` is needed when connecting it to the real API. Other available commands: `npm run preview` (preview of the build) and `npm run typecheck` (`tsc --noEmit`).

## Production notes

### Reverse proxy and `server.proxy_mode`

In production, put the instance behind a reverse proxy (TLS, forwarding headers). The `[server] proxy_mode = true` key is intended to trust the `X-Forwarded-*` headers when behind a reverse proxy. Set `server.bind` accordingly (e.g. `0.0.0.0:8099` to listen on all interfaces behind the proxy, or an internal address).

### Workers

`[server] workers` expresses the desired number of workers. The async runtime uses Tokio's multi-thread scheduler.

### Storage backend: `fs` vs `s3`

`[storage] backend` accepts `fs` or `s3`:

- **`fs`** (default): a content-addressed blob store on the filesystem, with the root `storage.path` (required). Identical bytes deduplicate into a single immutable file. This is the backend currently implemented and the one that `serve` instantiates (`FsBlobStore`).
- **`s3`**: configuration validation requires `storage.bucket` (and you can specify `region`, with the credentials via the environment). Note, however, that at the storage level, in v1, only the filesystem backend is available: the `kigumi-storage` crate documents `S3BlobStore` as out of v1. See the **Uncertainties**.

### JWT secret rotation

The design provides `KIGUMI_JWT_SECRET_OLD` to rotate the JWT signing secret without invalidating already-issued tokens: you set `KIGUMI_JWT_SECRET` to the new value and `KIGUMI_JWT_SECRET_OLD` to the previous one, to be kept accepted in verification during the rotation window. **Status in v1**: the segment is loaded into the configuration but not yet wired up at runtime — the `Authenticator` receives a single secret (`KIGUMI_JWT_SECRET`), so verification with the previous secret is not yet active. See also [sicurezza.md](sicurezza.md).

### Destructive operations

`KIGUMI_ADMIN_TOKEN` is a secret **reserved** for the future protection of destructive database operations (dump/restore/gc): in v1 it is loaded into the configuration but the enforcement is not yet wired up, and those commands/endpoints do not exist yet. Consistent with [configurazione.md](configurazione.md).
