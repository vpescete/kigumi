# Architecture

Meshble is a headless, schema-driven ERP framework written in Rust: a model is
defined once as **inspectable static data**, and from that single source of
truth the framework derives the Postgres schema, the UI contract consumable by any
frontend, and the OpenAPI schema for integrators. This page describes the layout of the
crates and their responsibilities, the metamodel and the compile-time registries, the
generation pipeline, the lifecycle of an HTTP request, and the versioning model.
For the overview and quickstart see [README.md](./README.md); for the security
details see [sicurezza.md](./sicurezza.md) and for the REST APIs [api.md](./api.md).

## Workspace layout

The Cargo workspace groups three families of members:

```toml
[workspace]
resolver = "2"
members = ["crates/*", "modules/*", "apps/*"]
```

- `crates/*` — the **framework**: the crates that implement the metamodel, persistence,
  security, server. They all share the same workspace SemVer version.
- `modules/*` — the application **modules** (`base`, `mail`, `sales`, `account`, `stock`).
  Each has its own version, independent of the framework.
- `apps/*` — the executable **applications** that link the framework and modules (`meshble-cli`,
  `renderer-demo`).

All framework crates share the version declared in `[workspace.package]`:

```toml
[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT OR Apache-2.0"
repository = "https://github.com/vpescete/msh_framework"
```

### Responsibilities of the framework crates

| Crate | Responsibility |
|---|---|
| `meshble-core` | The inspectable metamodel (`FieldKind`, `FieldDef`, `ModelDescriptor`, `ResolvedModel`), the compile-time registries via `inventory`, the security engine (ACL + record rule + `Ctx`), the typed domain AST, the module versioning model. No dependency on database or HTTP. |
| `meshble-macros` | The `#[model]` and `#[extend]` proc-macros: they generate the static `ModelDescriptor` + `impl Model` from an annotated struct, and emit the `inventory` registrations for field-level security (`#[field(groups = "...")]`), related field (`#[field(related = "...")]`), tracked field (`#[field(tracked)]`) and `inherits` delegation (`#[model(inherits = "...", via = "...")]`). |
| `meshble-schema` | The projections from `ResolvedModel`: `to_ddl` (Postgres DDL), `to_ui_contract` (JSON UI contract) and `openapi` (OpenAPI 3.1 schema). Same source of truth, multiple outputs. |
| `meshble-db` | The Postgres persistence layer (`sqlx`). It exposes the `*_secured` methods that apply the security engine at the database boundary, the versioned migration engine, the supporting stores (auth, runtime ACL/record rule, installed modules, sequences, settings, cron). |
| `meshble-auth` | Authentication: password hashing (argon2) and HS256-signed JWT tokens (typed access/refresh). Verifies a bearer access token into a trusted `Ctx`. |
| `meshble-server` | The HTTP router (`axum`): exposes the metadata (OpenAPI, model list, UI contracts) and the secure CRUD endpoints. For every data request it verifies the token into a `Ctx` and delegates persistence to `meshble-db`. |
| `meshble-config` | Typed instance configuration: non-secret settings from `defaults < meshble.toml < env` (fail-fast validation) and the secrets, read only from the environment and verified at startup. |
| `meshble-storage` | Content-addressed blob storage: binary attachments live behind the `BlobStore` trait, indexed by the sha256 of the content (identical bytes deduplicate into a single object). v1 provides `FsBlobStore`. |
| `meshble` (facade) | The facade. Application modules depend **only** on this crate: `use meshble::prelude::*;` exposes the metamodel, the macros, the schema projections and all the `register_*!` macros. |

The facade also re-exports `inventory`, so the macros can emit absolute
`::meshble::inventory::submit!` paths without every module having to add the
dependency:

```rust
// crates/meshble/src/lib.rs
pub use meshble_core::inventory;
```

## The metamodel

The heart of the framework is the metamodel in `crates/meshble-core/src/metamodel.rs`. A
model is not a class synthesized at runtime, but **inspectable static data**.

### `FieldKind`

The logical type of a field. From here the framework derives the SQL type, the UI widget and the
API type:

```rust
pub enum FieldKind {
    Text,
    Html,
    Image,
    Integer,
    Float,
    Decimal { currency_field: Option<&'static str> },
    Bool,
    Date,
    Datetime,
    Selection(&'static [(&'static str, &'static str)]),
    Many2one { target: &'static str },
    One2many { target: &'static str, inverse: &'static str },
    Many2many {
        target: &'static str,
        relation: &'static str,
        column: &'static str,
        target_column: &'static str,
    },
}
```

Notes relevant to generation:

- `Many2one` generates an FK column; `One2many` does **not** generate a column (it lives on the inverse);
  `Many2many` does not generate a column on the model (membership lives in the
  `relation` junction table).
- `Image` is a `bigint` FK column toward the attachments table: the bytes live in the
  content-addressed blob store, indexed by the sha256, and the field carries the attachment id.
- `Decimal` carries an optional `currency_field` that makes it a "monetary" field linked to
  a currency; for non-exact amounts (quantities, weights, factors, rates) use `Float`.

### `FieldDef`

The definition of a single field:

```rust
pub struct FieldDef {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    pub stored: bool,
    pub compute: Option<&'static str>,
    pub depends: &'static [&'static str],
    pub default: Option<&'static str>,
    pub unique: bool,
    pub check: Option<&'static str>,
}
```

Two methods drive generation: `has_column()` is true only if the field is `stored` and
is not a `One2many`/`Many2many` relation; `is_computed()` is true if `compute` is
present.

### `ResolvedModel`

A `ModelDescriptor` describes a model as defined by **one** module (the "base"). The
`ResolvedModel` is instead the **resolved** descriptor: base plus all extensions merged and
validated.

```rust
pub struct ResolvedModel {
    pub name: &'static str,
    pub table: &'static str,
    pub fields: Vec<FieldDef>,
}
```

The `resolve(base, extensions)` function merges the base with the module extensions; a
field name conflict is an **error**, not a silent override. The `validate_depends`
function checks that every `depends` points to an existing field (first segment
of the path), so a broken dependency is a build error and not a runtime bug; it also
rejects a relational `depends` (containing a dot) on a non-stored computed field, which
would be evaluated same-record and silently read empty.

### Inspectability

Because every model is static data, the catalog is queryable at runtime without class
introspection: `resolve_registered(model)` returns the `ResolvedModel`,
`resolve_all_registered()` the entire set, `registered_model_names()` the names (ordered and
deterministic). The projections of `meshble-schema` operate directly on these
descriptors. For the complete design of the metamodel see
[`METAMODEL_DESIGN.md`](../../METAMODEL_DESIGN.md).

## The compile-time registries (`inventory`)

Models and extensions **self-register** through the `inventory` crate: the
resolver merges them without manual wiring. Every `register_*!` macro (or the
`#[model]`/`#[field]` annotation) emits an `inventory::submit!` of a type registered in
`meshble-core` (defined in `registry.rs` and in the related modules `action.rs`, `report.rs`,
`wizard.rs`, `view.rs`, `security.rs`, and re-exported by the facade). The `register_*!`
macros live in the facade, in `crates/meshble/src/lib.rs`. Example:

```rust
// crates/meshble/src/lib.rs
macro_rules! register_module {
    ($manifest:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ModuleRegistration { manifest: || $manifest, crate_path: ::core::module_path!() }
        }
    };
}
```

Every registered type has its own `inventory::collect!`, and the core provides the
collection functions that iterate over all submissions linked into the binary.

| Registry | Type | Emitted by | Collected by |
|---|---|---|---|
| Base models | `ModelRegistration` | `#[model]` | `registered_model_names`, `resolve_registered` |
| Field extensions | `FieldExtension` | `#[extend]` | `resolve_registered` (merged into the base) |
| Module manifests | `ModuleRegistration` | `register_module!` | `resolve_modules` |
| ACL | `AclRegistration` | `register_acls!` | `registered_acls` |
| Record rule | `RecordRuleRegistration` | `register_rules!` | `registered_rules` |
| Action | `ActionRegistration` | `register_action!` | `actions_for`, `action_for` |
| Report | `ReportRegistration` | `register_report!` | `reports_for`, `report_for` |
| Wizard | `WizardRegistration` | `register_wizard!` | `wizard_for` |
| View (form) | `FormView` | `register_view!` | `view_for` |
| Models with chatter | `MailedRegistration` | `register_mailed!` | `mailed_models`, `is_mailed` |
| External tables | `ExternalTable` | `register_external!` | `external_tables` |
| Transient models | `TransientRegistration` | `register_transient!` | `transient_models`, `is_transient` |
| Tracked fields | `TrackedFieldRegistration` | `#[field(tracked)]` / `register_tracked!` | `tracked_fields` |
| `inherits` delegation | `InheritsRegistration` | `#[model(inherits = …, via = …)]` / `register_inherits!` | `inherits_of`, `delegated_fields` |
| Related field | `RelatedRegistration` | `#[field(related = "...")]` / `register_related!` | `related_path` |
| Field-level security | `FieldGroupRegistration` | `#[field(groups = "...")]` / `register_field_groups!` | `field_required_groups` |
| Compute | `ComputeRegistration` | `register_compute!` | `compute_fn`, `computed_fields` |
| Cross-record constraint | `ConstraintRegistration` | `register_constraint!` | `check_constraints` |
| Cron | `CronRegistration` (in `meshble-db`) | manual `inventory::submit!` | `registered_crons` |

`registered_group_names()` derives the catalog's known groups by merging those referenced by
any registered ACL or record rule (ordered, deterministic): it is the source for the
seed of the read-only `res.groups` list.

### Catalog resolution and migration order

`resolve_registered(model)` starts from the registered base, collects and orders the extensions
per module (deterministic), merges them with `resolve` (conflict check) and validates the
`inherits` delegation and the `depends`. `migration_plan()` produces the migration plan
**topologically ordered** by FK dependencies: a model's FK targets — `Many2one` and
`Image` (FK toward the attachments table) — are created before the referencing
table; a self-reference is ignored and a genuine FK cycle is an error. External tables
(`register_external!`) are resolved and served like any model but **excluded**
from migration: the metamodel neither creates nor alters their table.

## The generation pipeline

The projections live in `crates/meshble-schema/src/lib.rs` and
`crates/meshble-schema/src/openapi.rs`. From **one** `ResolvedModel` three outputs are produced.

### 1. Postgres DDL — `to_ddl`

`to_ddl(m)` generates the `CREATE TABLE`. The `id bigserial` PK is always present; only the fields
with a column (`has_column()`) produce a row; a `Many2one` adds
`REFERENCES <target>(id)`, an `Image` adds `REFERENCES meshble_attachment(id)`; `required`,
`unique` and `check` add the respective constraints. The table of a dotted name derives
by replacing `.` with `_`.

```rust
pub fn to_ddl(m: &ResolvedModel) -> String {
    let mut lines = vec!["  id bigserial PRIMARY KEY".to_string()];
    for f in m.fields.iter().filter(|f| f.has_column()) {
        // ... pg_type(&f.kind), REFERENCES, NOT NULL, UNIQUE, CHECK ...
    }
    format!("CREATE TABLE {} (\n{}\n);", m.table, lines.join(",\n"))
}
```

The type mapping (`pg_type`): `Text`/`Html`/`Selection` → `text`, `Integer` →
`bigint`, `Float` → `double precision`, `Decimal` → `numeric`, `Bool` → `boolean`,
`Date` → `date`, `Datetime` → `timestamptz`, `Many2one`/`Image` → `bigint`;
`One2many`/`Many2many` have no column.

### 2. JSON UI contract — `to_ui_contract`

`to_ui_contract(m, rules)` produces the UI contract: JSON consumable by **any**
frontend. For each field it emits the name, label, suggested widget, `required` and `readonly`
(computed and related fields are read-only); for `Selection` the options; for the
relations `relation`/`inverse`. The dynamic `invisible_when`/`readonly_when` rules are
emitted as portable JSON domain ASTs — **the same** domains the server compiles into
SQL, never an evaluated string. A rule that references an unknown field is an error
(not a broken UI discovered in production). The contract also includes the list view
columns, the available actions (with the allowed groups), the printable reports, the
`mailed` flag and the declared form view; the fields delegated via `inherits` are exposed
transparently as editable fields.

### 3. OpenAPI 3.1 schema — `openapi`

`openapi(models)` builds an OpenAPI 3.1 document (`openapi(&[&ResolvedModel])`) that
describes the models as a documented REST API; from this you generate typed SDKs
(TS/Python/Go) with standard tooling, without hand-written clients. For each model it emits
the field schema and the paths `/api/<table>` (list) and `/api/<table>/{id}` (get-one):

```rust
let base = format!("/api/{}", m.table);
paths.insert(base.clone(), json!({ "get": list_op(m) }));
paths.insert(format!("{base}/{{id}}"), json!({ "get": get_op(m) }));
```

The field types follow the same source: `Decimal` is serialized as a string (format
`decimal`) to preserve precision; a `One2many` is an array referencing the schema
of the child model; a `Many2many` an array of `int64` ids.

## The lifecycle of a request

A data request traverses a precise chain, with a **single point** where ACL,
record rule and multi-company are applied. The router is in
`crates/meshble-server/src/lib.rs`.

### The router

`router_with_data` builds the complete router: metadata routes plus the secure CRUD
endpoints. The signature makes the dependency on the JWT secret explicit:

```rust
pub fn router_with_data(
    models: Vec<ResolvedModel>,
    db: Db,
    acls: &'static [Acl],
    rules: &'static [RecordRule],
    auth_secret: impl Into<String>,
    blobs: Arc<dyn BlobStore>,
) -> Router
```

The main CRUD routes registered:

```rust
.route("/api/:name", get(list_handler).post(create_handler))
.route("/api/:name/:id", get(get_one_handler).patch(update_handler).delete(delete_handler))
.route("/api/:name/:id/action/:action", post(action_handler))
```

plus the authentication routes (`/auth/login`, `/auth/refresh`, `/auth/logout`,
`/auth/me`), health routes (`/health`, `/ready`), and those for attachments, chatter, activities,
followers, reports and the pinned business services (e.g. `generate_variants`,
`apply_pricelist`, `apply_discount`, `post`, `create_invoice`, `validate`). The
metadata-only router `router(models)` exposes `/openapi.json`, `/api/models` and `/api/:name/view`
without a database.

### Step 1 — HTTP → JWT auth → trusted `Ctx`

Every data handler begins by verifying the bearer token into a `Ctx`:

```rust
fn authenticate(backend: &DataBackend, headers: &HeaderMap) -> Result<Ctx, Response> {
    let header = headers.get("authorization").and_then(|v| v.to_str().ok());
    backend
        .auth
        .verify_bearer(header)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "unauthorized").into_response())
}
```

`verify_bearer` (in `meshble-auth`) extracts `Bearer <token>`, verifies it as an **access**
HS256 token (rejecting refresh tokens, with `alg=HS256` pinned against alg-confusion) and
turns it into a `Ctx`. The claims carry `groups` and the multi-company scope
(`company`/`companies`): a non-empty set produces a company-scoped `Ctx` via
`Ctx::in_companies(active, allowed)`. This is real authentication: a client cannot
claim a group without a token signed by the server's secret.

The `Ctx` (in `crates/meshble-core/src/security.rs`) carries `uid`, `groups`, the active
company (`company_id`) and the set of allowed companies (`allowed_company_ids`), and a
**private** superuser flag (`su`): external code cannot forge an elevated context with a
struct literal, because the only escalation path is the greppable `Ctx::sudo()` method.

### Step 2 — Secure CRUD in `meshble-db`

The handler delegates to one of `Db`'s `*_secured` methods
(`crates/meshble-db/src/lib.rs`), which apply the security engine at the **database
boundary**:

| Operation | Entry point |
|---|---|
| Paginated list | `list_secured` |
| Count | `count_secured` |
| Rows as JSON | `find_secured` |
| Visible ids | `find_ids_secured` |
| Get-one | `find_one_secured` |
| Create | `insert_secured` |
| Update | `update_secured` |
| Delete | `delete_secured` |

On read, the single enforcement point is `secured_read_domain`: it verifies the Read ACL,
checks that the filter supplied by the caller does not reference non-readable fields (D6,
including the relational paths walked hop-by-hop), then composes in `AND` the record rule
domain and the caller's filter and adds in `AND` the multi-company restriction:

```rust
let rule = record_rule_domain(Operation::Read, model.name, ctx, rules);
let base = match (filter, rule) { /* AND of filter and rule */ };
Ok(match company_filter(model, ctx) {
    Some(cf) => base.and(cf),
    None => base,
})
```

The resulting domain is compiled into a **parameterized** `WHERE`: the values are
always bound (`$1, $2, …`), never interpolated into the SQL text — closing the
injection surface. On write, `insert_secured`/`update_secured`/`delete_secured` verify
the ACL of the respective operation, apply field-level security
(`check_writable_fields`), and enforce the record rule and the company scope in the
same transaction (a Create/Update/Delete that would violate the rule is rejected or
rolled back).

The three policies coexist in a single place:

- **ACL** (`check_access`): model-level grant per group, with union semantics
  (a single group granting the operation suffices); superuser always allowed.
- **Record rule** (`record_rule_domain`): global rules (without group) all required
  (AND), applicable group rules in the alternative (OR), the two composed in AND. A
  rule is a typed `Domain` compiled into parameterized SQL, not an evaluated string.
- **Multi-company** (`company_filter`): a non-superuser caller is **always**
  company-scoped (default-deny) on models that have a `Many2one` `company_id` field. With
  an allowed set it sees those companies plus the shared rows (`company_id IS NULL`);
  with an empty set it sees only the shared rows. Only `sudo` is unrestricted.

The db errors (`DbError`) are mapped to coherent HTTP responses: `AccessDenied` → 403,
`BadInput` → 400, `Conflict` (unique/FK violation) → 409; an internal error becomes an opaque 500
that does not leak schema or SQL.

## Modules, apps and web

Three distinct planes, with clean boundaries:

- **modules** (`modules/*`) — Rust crates that define models, extensions, ACL, record
  rules, actions, reports, wizards and views. They depend **only** on the `meshble` facade and
  self-register into the `inventory` registries. A module declares its own `ModuleManifest`
  and registers it:

  ```rust
  // modules/base/src/lib.rs
  pub static MANIFEST: ModuleManifest = ModuleManifest {
      name: "base",
      version: "1.0.0",
      framework: ">=0.1, <0.2",
      depends: &[],
      summary: "Foundational models: currency, partner, company",
  };
  meshble::register_module!(MANIFEST);
  ```

  To write your own module see [moduli-custom.md](./moduli-custom.md); for the included
  modules see [moduli.md](./moduli.md).

- **apps** (`apps/*`) — the executables. `meshble-cli` (binary `meshble`) links the framework
  and the desired modules: linking a module is what brings its `inventory`
  registrations into the binary. The CLI exposes `meshble serve` (migrates catalog + auth, does
  the admin bootstrap from env, then serves the secure API), `meshble migrate` (migrates all the
  linked modules + the auth schema, then exits) and the subcommands `meshble config`,
  `meshble user`, `meshble acl`, `meshble rule`, `meshble module`, `meshble version`. The
  modules made available are only those whose crate is linked into the binary.

  ```toml
  # apps/meshble-cli/Cargo.toml — the linked modules self-register into the catalog
  meshble-mod-base = { path = "../../modules/base" }
  meshble-mod-mail = { path = "../../modules/mail" }
  meshble-mod-sales = { path = "../../modules/sales" }
  meshble-mod-account = { path = "../../modules/account" }
  meshble-mod-stock = { path = "../../modules/stock" }
  ```

- **web** (`web/`) — the frontend (Vite/TypeScript), separate from the Rust workspace. It is a
  consumer of the UI contract and the OpenAPI schema generated by the server: it does not know the
  schema a priori, it reads it as data. Because Meshble is headless, the web is one of the many
  possible clients (on par with a generated SDK).

The separation of catalog (compile time) vs installed set (runtime, per database) is
deliberate: all the available modules are linked, resolved and type-checked crates together;
which modules are *active* for an instance is runtime data (managed by
`meshble module install` / `meshble module uninstall`), not a recompilation.

## The versioning model

The framework uses **pure SemVer** (Cargo-native). `FRAMEWORK_VERSION` is the workspace
version, exposed by the core:

```rust
// crates/meshble-core/src/lib.rs
pub const FRAMEWORK_VERSION: &str = env!("CARGO_PKG_VERSION");
```

Every module has its **own** SemVer version, independent of the framework, and declares it in the
manifest (`crates/meshble-core/src/manifest.rs`):

```rust
pub struct ModuleManifest {
    pub name: &'static str,
    pub version: &'static str,        // module SemVer, e.g. "1.0.0"
    pub framework: &'static str,      // compatibility range with the framework, e.g. ">=0.1, <0.2"
    pub depends: &'static [ModuleDep],// dependencies on other modules, with SemVer range
    pub summary: &'static str,
}
```

A dependency between modules is a `ModuleDep` with a SemVer range:

```rust
pub struct ModuleDep {
    pub name: &'static str,
    pub req: &'static str,            // SemVer range, e.g. "^1.0"
}
```

Two mechanisms make versioning **verifiable**:

- **Compatibility range with the framework** — `check_compat` verifies that the framework
  version falls within the `framework` range declared by the module. A module out of range is an
  error, not a runtime crash.
- **Per-module versions with ranges on dependencies** — every `ModuleDep` carries a SemVer
  range (`req`, e.g. `"^1.0"`). `resolve_module_set` (a pure function on the explicit set)
  verifies compat with the framework, the existence of every dependency with a version that
  satisfies the range, the absence of duplicates, self-dependencies and cycles, returning the modules in
  **topological order**. `resolve_modules` is the thin wrapper that feeds this
  function with the `inventory` catalog.

The errors are dedicated: `Incompatible`, `MissingDependency`, `DependencyConflict`,
`DuplicateModule`, `SelfDependency`, `DependencyCycle` (with only the real members of the cycle).

### Pre-release policy

A pre-release build (e.g. `0.1.5-rc.1`) is treated like its release line (`0.1.5`)
when comparing ranges, via `release_of`. Without this policy the Cargo/SemVer
rules would reject every in-range pre-release, making every install fail during
the RC/dev builds of the framework. The boundary stays correct: `0.2.0-rc.1` → `0.2.0`, still
outside `<0.2`.

### Versioned migrations

The migration engine (`crates/meshble-db/src/migration.rs`) is declarative and
versioned: the state lives in `meshble_module` (current version) and `meshble_migration`
(one row per applied version). Every install/upgrade is **atomic** (single
transaction), **serialized** (`pg_advisory_xact_lock` per module) and **idempotent**
(re-running at the same version is a no-op). The schema is generated from the `ResolvedModel`
(via `to_ddl`), not written by hand. For the complete model see
[`VERSIONING.md`](../../VERSIONING.md).

---

See also: [installazione.md](./installazione.md) for the setup,
[configurazione.md](./configurazione.md) for `meshble.toml` and the environment variables,
[api.md](./api.md) for the REST endpoints and [sicurezza.md](./sicurezza.md) for ACL, record
rule and multi-company in detail.
