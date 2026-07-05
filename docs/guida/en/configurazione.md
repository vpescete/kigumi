# Configuration (reference)

This page documents **every** Kigumi configuration key, its default value, and its meaning. Kigumi's configuration is split into two clearly separate planes: the **boot configuration** (non-secret, typed, loaded from `kigumi.toml` and from environment variables prefixed with `KIGUMI_CONF_`) and the **secrets** (read exclusively from the environment, never from the file). On top of these are the **runtime settings** stored in the database, which are mutable without a restart and for which the DB is the sole authority. The page also includes the full reference of the `kigumi` CLI commands. For installation see [installazione.md](installazione.md); for the architectural context see [architettura.md](architettura.md); for the security topics (JWT rotation, groups, ACLs, and record rules) see [sicurezza.md](sicurezza.md).

## The two planes: boot-time and secrets

The boot configuration is everything that is serializable into `kigumi.toml` and that is **not** a secret. It is loaded with the layering `defaults < kigumi.toml < env KIGUMI_CONF_*`, parsed into the typed `Config` struct, and validated **fail-fast**: a typo in a core section prevents startup, instead of being silently ignored.

Secrets never live in the file: they are read only from the environment via `Secrets::from_env`, and the presence of the required ones is checked at startup. The identity of the database connection is the single `DATABASE_URL` (a complete DSN); the `[database]` section carries only the *tuning* not tied to the URL, so there is no ambiguous overlap.

`Settings::load` combines the two planes into a `Settings { config, secrets }` struct and also verifies their interaction (for example: if `[mail].smtp_host` is set but `KIGUMI_SMTP_PASSWORD` is not, startup fails).

## `kigumi.toml` file

Complete example (a copy of `kigumi.toml.example`):

```toml
[instance]
name = "acme-prod"

[server]
bind = "0.0.0.0:8099"
workers = 8
proxy_mode = true              # trust X-Forwarded-* behind a reverse proxy

[database]                     # TUNING ONLY — the connection identity is the DATABASE_URL env var
pool_max = 10
connect_timeout = "5s"

[storage]
backend = "fs"                 # fs | s3
path = "/var/lib/kigumi/blobs"
# bucket = "kigumi-blobs"     # for backend = s3 (keys via env)
# region = "eu-west-1"

[auth]
access_ttl = 900               # 15 min  (jwt secret via env KIGUMI_JWT_SECRET)
refresh_ttl = 2592000          # 30 days

[mail]
smtp_host = "smtp.acme.com"    # smtp password via env KIGUMI_SMTP_PASSWORD
smtp_port = 587
from = "erp@acme.com"

[modules]
load = ["base", "sales"]

[modules.sales]                # OPEN subtree — validated by the "sales" module, not the core schema
default_tax = "0.22"

[log]
level = "info"                 # error | warn | info | debug | trace
format = "json"                # json | text  (code default: text)
```

The core sections are declared with `deny_unknown_fields`: a key or a section with a typo causes the load to fail. The exception is `[modules]` (see below), whose per-module subtree is intentionally open.

## Key reference

A note on defaults: the default `Config` is complete but **not** automatically valid. In particular, with `storage.backend = "fs"` you must provide `storage.path` (see [Validation](#validation)). The defaults listed below are the ones applied when the key is absent.

### `[instance]`

| Key | Type | Default | Meaning |
|--------|------|---------|-------------|
| `name` | `String` | `"kigumi"` | Logical name of the instance. |

> Note: the instance's runtime values (`base_url`, `mode`, `neutralized`, `banner`) do **not** live here: they live in the database and are authoritative there. See [Runtime settings in the database](#runtime-settings-in-the-database).

### `[server]`

| Key | Type | Default | Meaning |
|--------|------|---------|-------------|
| `bind` | `String` | `"127.0.0.1:8099"` | `host:port` address the server listens on. It must be parseable as a `SocketAddr`, otherwise validation fails. |
| `workers` | `usize` | `4` | Number of workers. |
| `proxy_mode` | `bool` | `false` | When `true`, the instance trusts the `X-Forwarded-*` headers behind a reverse proxy. |

### `[database]` (tuning only)

The connection identity (host, port, db, user, password, sslmode) is the single `DATABASE_URL`. This section contains **only** tuning not tied to the URL; a key such as `host` here is an unknown field and is rejected (verified by the `host_in_database_section_is_rejected` test).

| Key | Type | Default | Meaning |
|--------|------|---------|-------------|
| `pool_max` | `u32` | `10` | Maximum size of the connection pool. |
| `connect_timeout` | `String` | `"5s"` | Connection timeout (duration string). |

### `[storage]`

| Key | Type | Default | Meaning |
|--------|------|---------|-------------|
| `backend` | enum `fs` \| `s3` | `fs` | Backend of the content-addressed blob store (`StorageBackend`). |
| `path` | `Option<String>` | absent | Root directory for the `fs` backend. **Required** when `backend = fs`. |
| `bucket` | `Option<String>` | absent | Bucket for the `s3` backend. **Required** when `backend = s3`. |
| `region` | `Option<String>` | absent | Region for the `s3` backend. |

For the `fs` backend, `serve` instantiates an `FsBlobStore` (an `Arc<dyn BlobStore>`) rooted at `storage.path`, and identical bytes deduplicate into a single immutable file.

For the `s3` backend, `serve` instantiates an `S3BlobStore` over `bucket`/`region` with the same content-addressed dedup (keys `ab/cd/<sha256>`). The `kigumi` binary ships the `s3` feature built in — no rebuild is needed. Two things come from the **environment**, never from this file:

- **Credentials** — the standard AWS chain: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and optionally `AWS_SESSION_TOKEN`; falling back to the shared profile and then IAM role.
- **Endpoint** — `KIGUMI_S3_ENDPOINT` selects an S3-compatible service other than AWS (MinIO, Cloudflare R2, LocalStack). Setting it switches to path-style addressing automatically. Leave it unset for real AWS S3. When `region` is absent it defaults to `us-east-1`.

Example — MinIO:

```bash
export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin
export KIGUMI_S3_ENDPOINT=http://127.0.0.1:9000
# kigumi.toml: [storage] backend = "s3", bucket = "kigumi-blobs"
kigumi serve
```

### `[auth]`

| Key | Type | Default | Meaning |
|--------|------|---------|-------------|
| `access_ttl` | `u64` | `900` | Access token lifetime in seconds (15 minutes). |
| `refresh_ttl` | `u64` | `2592000` | Refresh token lifetime in seconds (30 days). |

The HS256 signing secret does not live here: it comes from `KIGUMI_JWT_SECRET` (see [Secrets](#secrets-environment-variables)).

### `[mail]`

All fields are optional.

| Key | Type | Default | Meaning |
|--------|------|---------|-------------|
| `smtp_host` | `Option<String>` | absent | SMTP host. If set, it requires `KIGUMI_SMTP_PASSWORD` in the environment, otherwise startup fails. |
| `smtp_port` | `Option<u16>` | absent | SMTP port (for example `587`). |
| `from` | `Option<String>` | absent | Default sender address. |

### `[modules]`

| Key | Type | Default | Meaning |
|--------|------|---------|-------------|
| `load` | `Vec<String>` | `[]` | **Inert in v1**: the key is read but does not select the installed modules. Installation is governed by the registry in the DB via `kigumi module install` (see [moduli.md](moduli.md)). |
| `[modules.<name>]` | open subtree | `{}` | Per-module configuration. |

The core keys of `[modules]` are strict, but each `[modules.<name>]` is an **open subtree**: it is captured verbatim (field `per_module`, a `BTreeMap<String, figment::value::Value>` with `#[serde(flatten)]`) and validated by the owning module at load, so a module can carry its own settings without the instance refusing to start. In the example, `[modules.sales]` with `default_tax = "0.22"` is validated by the `sales` module, not by the core schema. See [moduli.md](moduli.md) and [moduli-custom.md](moduli-custom.md).

### `[log]`

| Key | Type | Default | Meaning |
|--------|------|---------|-------------|
| `level` | `String` | `"info"` | Log level: `error` \| `warn` \| `info` \| `debug` \| `trace`. |
| `format` | `String` | `"text"` | Log format: `json` \| `text`. (The example uses `json`.) |

`serve` installs a `tracing` subscriber from these values: `format = "json"` emits structured logs for a production log pipeline, `text` is human-readable. The **`RUST_LOG`** environment variable overrides `level` when set (e.g. `RUST_LOG=kigumi_server=debug,info`). Each HTTP request is wrapped in a span logging the method, path, status, and latency (completed requests at `info`, failures at `error`) — **metadata only; request and response bodies are never logged**. Metrics/traces export to an OpenTelemetry collector is the opt-in next layer.

### `[oidc]`

Optional SSO via OpenID Connect (Authorization Code + PKCE), alongside password login. It is **all-or-nothing**: either omit the section entirely (SSO off, the `/auth/oidc/*` routes 404) or set all four keys (a partial block fails validation).

| Key | Type | Meaning |
|--------|------|-------------|
| `issuer` | `String` | The IdP's issuer URL — `<issuer>/.well-known/openid-configuration` is discovered for the authorization/token/JWKS endpoints. Any compliant IdP (Google, Microsoft, Okta, Keycloak, …). |
| `client_id` | `String` | The OAuth client id registered with the IdP. |
| `redirect_uri` | `String` | The server's own `…/auth/oidc/callback` URL, registered with the IdP. |
| `post_login_url` | `String` | Where the browser lands after a successful login; the minted tokens arrive in the URL **fragment** (`#access_token=…&refresh_token=…`) for the SPA to read. |

The client **secret** comes from the `KIGUMI_OIDC_CLIENT_SECRET` env var, never this file. On first login an unknown (verified) email is provisioned just-in-time with **no groups** (it can authenticate but sees nothing until an admin grants groups) and no usable password; a known email logs into the existing user. Only emails the IdP marks **verified** are accepted.

## Validation

`Config::validate` performs the cross-checks that the serde schema cannot express:

- `storage.backend = fs` requires `storage.path` (otherwise the error `storage.backend = fs requires storage.path`).
- `storage.backend = s3` requires `storage.bucket` (otherwise the error `storage.backend = s3 requires storage.bucket`).
- `server.bind` must parse as `host:port` (`SocketAddr`), otherwise the error `server.bind is not a host:port (...)`.

At the `Settings::load` level there is also the cross-check with the secrets: `mail.smtp_host` set without `KIGUMI_SMTP_PASSWORD` produces the error `mail.smtp_host is set but KIGUMI_SMTP_PASSWORD is not`.

To validate and inspect the effective configuration without starting the server you can use the standalone `kigumi-config` binary:

```bash
kigumi-config check    # validates the effective configuration (config + secrets)
kigumi-config print    # prints the effective config with secrets redacted
```

The file path is taken from `$KIGUMI_CONFIG` or, in its absence, from `./kigumi.toml`. The two commands are also available as the `kigumi config check` / `kigumi config print` subcommands (the latter, unlike the standalone binary, also adds the runtime settings from the DB).

## Override from environment variables: `KIGUMI_CONF_`

Every boot key can be overridden from the environment with the `KIGUMI_CONF_` prefix and the **double underscore** (`__`) as the nesting separator. The env provider is loaded as the last layer (`Env::prefixed("KIGUMI_CONF_").split("__")`), so it wins over file and defaults.

| TOML key | Environment variable |
|-------------|----------------------|
| `[server] bind` | `KIGUMI_CONF_SERVER__BIND` |
| `[server] workers` | `KIGUMI_CONF_SERVER__WORKERS` |
| `[server] proxy_mode` | `KIGUMI_CONF_SERVER__PROXY_MODE` |
| `[storage] backend` | `KIGUMI_CONF_STORAGE__BACKEND` |
| `[storage] path` | `KIGUMI_CONF_STORAGE__PATH` |
| `[auth] access_ttl` | `KIGUMI_CONF_AUTH__ACCESS_TTL` |
| `[log] level` | `KIGUMI_CONF_LOG__LEVEL` |
| `[instance] name` | `KIGUMI_CONF_INSTANCE__NAME` |

Example:

```bash
export KIGUMI_CONF_SERVER__BIND=0.0.0.0:9000
export KIGUMI_CONF_SERVER__WORKERS=8
export KIGUMI_CONF_LOG__FORMAT=json
```

The `KIGUMI_CONF_` prefix is deliberately distinct from the one for secrets (`DATABASE_URL`, `KIGUMI_JWT_SECRET`, …), so the secrets are never captured by the configuration layer.

## Secrets (environment variables)

Secrets are read only from the environment (never from `kigumi.toml`) via `Secrets::from_env`. The presence of the **required** ones is checked at startup: the instance refuses to start if one is missing (fail-fast).

| Variable | Required | Meaning |
|-----------|--------------|-------------|
| `DATABASE_URL` | Yes | Full Postgres DSN: the sole source of the connection identity (host, port, db, user, password, sslmode). It must be a parseable URL with the `postgres` or `postgresql` scheme, otherwise the error `DATABASE_URL is not a valid postgres:// URL`. |
| `KIGUMI_JWT_SECRET` | Yes | HS256 signing secret for access and refresh tokens. |
| `KIGUMI_JWT_SECRET_OLD` | No | Previous JWT secret, still **accepted on verify** during a rotation window (rotation by `kid`). |
| `KIGUMI_SMTP_PASSWORD` | No (*) | SMTP password. (*) Becomes required if `[mail].smtp_host` is configured. |
| `KIGUMI_ADMIN_TOKEN` | No | Bearer token intended to protect destructive database operations (dump/restore/gc). Optional at boot; when present it is only loaded into `Secrets` and shown redacted by `print` (endpoint-side enforcement is not wired up yet). |
| `KIGUMI_OIDC_CLIENT_SECRET` | No (*) | OIDC client secret. (*) Becomes required when the `[oidc]` section is configured. |

A variable is considered "unset" both if it is absent and if it is empty (`req`/`opt` filter out empty strings).

Example (see `.env.example`):

```bash
# REQUIRED — single source of the database connection identity (full Postgres DSN)
DATABASE_URL=postgres://kigumi:CHANGE_ME@127.0.0.1:5432/kigumi
# REQUIRED — HS256 signing secret for access/refresh tokens
KIGUMI_JWT_SECRET=CHANGE_ME_long_random_value
# OPTIONAL — previous JWT secret, accepted on verify during a rotation window
# KIGUMI_JWT_SECRET_OLD=
# OPTIONAL — required only if [mail].smtp_host is configured
# KIGUMI_SMTP_PASSWORD=
# OPTIONAL — bearer token gating destructive db ops (dump/restore/gc)
# KIGUMI_ADMIN_TOKEN=
```

When the effective configuration is printed (`kigumi config print` or `kigumi-config print`), every secret is redacted at the field level: the `DATABASE_URL` password is masked (`redact_db_url`) while host/port/db/user remain visible, and the other secrets appear as `set (****)` or `unset`.

### Additional operational secrets

Besides the secrets managed by `Secrets`, some CLI commands read these environment variables directly:

| Variable | Used by | Meaning |
|-----------|----------|-------------|
| `KIGUMI_ADMIN_PASSWORD` | `serve` (bootstrap admin) | Password used to bootstrap the `admin` user if it does not yet exist. Without it, `serve` warns (`warning: no admin user; set KIGUMI_ADMIN_PASSWORD to bootstrap one`) and does not create the admin (no password is ever hardcoded). |
| `KIGUMI_NEW_PASSWORD` | `user create`, `user set-password` | Alternative to the `--password` flag for creating/resetting a user's password. |
| `KIGUMI_CONFIG` | all commands | Path of the `kigumi.toml` file if not passed with `--config`; in its absence `./kigumi.toml` is used. |

## Runtime settings in the database

Some settings do **not** live in the boot configuration: they live in the database, are mutable without a restart, and the DB is their sole authority. They are stored in the `kigumi_setting` table (columns `key`, `value`, `vtype`), the typed equivalent of a runtime configuration parameter.

Two distinct mechanisms populate this table:

- `seed_setting(key, value, vtype)` inserts a default **only if the key is absent** (`ON CONFLICT (key) DO NOTHING`): install-time defaults never overwrite an operator's change.
- `set_setting(key, value, vtype)` performs an upsert (`ON CONFLICT (key) DO UPDATE`) and always **overwrites** the existing value.

During migration, `serve`/`migrate` seed the runtime defaults without trampling any changes the operator has already made:

```rust
db.seed_setting("base_url", "", "string").await?;
db.seed_setting("mode", "production", "string").await?;
```

Other typical runtime keys are `neutralized` and `banner`. The `vtype` field (`string` \| `bool` \| `int` \| `json`) is a hint for typed readers (default `string`). These keys are managed from the CLI with `kigumi config set|get` (see below).

## `kigumi` CLI reference

The `kigumi` executable is the single command to operate an instance. All commands accept the global option `--config <path>` (as an alternative to `$KIGUMI_CONFIG`).

| Command | What it does |
|---------|---------|
| `kigumi serve` | Migrates the catalog + the auth schema, bootstraps an admin from `KIGUMI_ADMIN_PASSWORD`, then serves the protected API. It also starts the cron scheduler in the background (tick every 60 s, atomic `SKIP LOCKED` claim). |
| `kigumi migrate` | Migrates the models of the installed modules + the framework schemas, then exits. On a fresh DB it installs `base` (and its dependency closure); seeds the `base_url`/`mode` runtime defaults. |
| `kigumi module list` | Lists the available (linked) modules, indicating for each the `installed` or `available` status and its summary. |
| `kigumi module install <name>` | Installs a module and its dependency closure (deps first), then migrates their tables (idempotent). |
| `kigumi module uninstall <name>` | Uninstalls a module: it stops being migrated/served, but its tables and data are **kept**. Refuses `base` and refuses if an installed module still depends on it. |
| `kigumi user create <login> [--password <p>] [--groups <csv>]` | Creates or replaces a user (upsert). Password via `--password` or `$KIGUMI_NEW_PASSWORD`. `--groups` defaults to `user`. |
| `kigumi user set-password <login> [--password <p>]` | Resets a user's password while keeping their groups. |
| `kigumi user grant <login> <group>` | Adds a group to a user. |
| `kigumi user company <login> [--active <id>] [--allowed <csv>]` | Assigns the user's multi-company scope: `--active` is the default company, `--allowed` a CSV of accessible ids (the active company is always included). Empty = unrestricted. |
| `kigumi acl grant <model> <group> [--read] [--write] [--create] [--delete]` | Grants (or updates) a runtime ACL for a group on a model. At least one operation flag is required. |
| `kigumi acl revoke <model> <group>` | Removes a runtime ACL override for a group on a model (the static baseline stays unchanged). |
| `kigumi acl list` | Lists the effective ACLs: the compiled baseline + the runtime overrides from the DB. |
| `kigumi rule add <model> [--groups <csv>] [--ops <csv>] --domain <json>` | Adds a runtime record rule. `--groups` is a CSV (empty = global), `--ops` a CSV of `r`/`w`/`c`/`d` (default `r`), `--domain` the portable JSON AST (e.g. `{"field":"state","op":"!=","value":"done"}`). |
| `kigumi rule remove <id>` | Removes a runtime record rule by id (the static baseline is not touched). |
| `kigumi rule list` | Lists the runtime record rules present in the DB. |
| `kigumi config check` | Validates the effective configuration. |
| `kigumi config print` | Prints the effective configuration (secrets redacted) **plus** the runtime settings from the DB. |
| `kigumi config set <key> <value> [--vtype <t>]` | Sets a runtime setting in the DB (the authority for runtime keys). `--vtype` defaults to `string`. |
| `kigumi config get <key>` | Reads the value of a runtime setting. |
| `kigumi version` | Prints the framework version and the linked modules with their version. |

The ACLs and record rules managed by the CLI are **additive** on top of the compiled baseline: for ACLs the DB overrides can only widen access (union), for record rules they add restrictions/alternatives through the same engine — in both cases the static baseline remains in force. For details on groups, ACLs, and record rules see [sicurezza.md](sicurezza.md).

> Only modules whose crate is linked into the binary are available to `module install`/`list`. For how to declare and package a module see [moduli.md](moduli.md) and [moduli-custom.md](moduli-custom.md).
