# Custom modules

This page is the complete guide to writing a custom module for Kigumi. A module is a Rust crate that declares models, ACLs, record rules, views, computes, constraints, actions, services, jobs, routes, reports, and wizards through compile-time macros and registries: everything auto-registers into the catalog via `inventory`, and the binary that links it collects and serves it without any manual wiring. You start from the crate and end up with a generated REST API and an integration test. For the big picture see [architettura.md](architettura.md) and [moduli.md](moduli.md); for installation and configuration [installazione.md](installazione.md) and [configurazione.md](configurazione.md); for security [sicurezza.md](sicurezza.md); for routes [api.md](api.md).

> **Two ways to author a module.** The recommended path for an application of your own is an
> out-of-tree workspace scaffolded by `kigumi new <name>`: it generates a module crate exactly like
> the ones described here plus a ~45-line server binary on `kigumi-runtime` (migrate, admin
> bootstrap, workers, serve — see [installazione.md](installazione.md)). Everything on this page
> applies unchanged to both that workspace and an in-tree module under `modules/`.

---

## 1. Crate setup

A module lives in `modules/NAME/` and is a normal Rust crate. Its only mandatory dependency is the `kigumi` facade, plus every module it depends on (to reuse their models as relation targets). A real example, `modules/stock/Cargo.toml`:

```toml
[package]
name = "kigumi-mod-stock"
description = "Kigumi stock module: inventory — locations, quants, pickings and moves"
# MODULE version, independent of the framework (see docs/VERSIONING.md).
version = "2.0.0"
edition.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
kigumi = { workspace = true }
# Exact-quantity arithmetic in the quant/move math.
rust_decimal = "1"
# Depends on base (company), sales (product.product), and mail (pickings carry a chatter thread).
kigumi-mod-base = { path = "../base", version = "2.0.0" }
kigumi-mod-sales = { path = "../sales", version = "2.0.0" }
kigumi-mod-mail = { path = "../mail", version = "2.0.0" }

[dev-dependencies]
kigumi-db = { workspace = true }
kigumi-mod-sales = { path = "../sales", version = "2.0.0" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
serde_json = "1"
```

Important notes:

- The package `version` is the **module version**, independent of the framework version (SemVer per module). The framework version is shared by all the core crates (`0.2.0` in the workspace).
- Cargo dependencies on other modules (`kigumi-mod-base`, ...) must mirror the module manifest's `depends`. Keeping the two lists aligned is intentional: a dependency declared in the manifest but not linked as a Cargo crate would not be present in `inventory`.
- `rust_decimal` is only needed if the module does exact arithmetic (money, quantities); `serde_json` only if it has reports or code that reads the JSON record.

### Linking the crate into the binary

The module auto-registers only if its crate is **linked** into the final binary. This is done in two steps in `apps/kigumi-cli`.

First you add the dependency in `apps/kigumi-cli/Cargo.toml`:

```toml
# Linked so their models/ACLs/rules self-register into the catalog (inventory).
kigumi-mod-base = { path = "../../modules/base" }
kigumi-mod-mail = { path = "../../modules/mail" }
kigumi-mod-sales = { path = "../../modules/sales" }
kigumi-mod-account = { path = "../../modules/account" }
kigumi-mod-stock = { path = "../../modules/stock" }
```

Then you reference a symbol from the crate inside `link_modules()` in `apps/kigumi-cli/src/main.rs`, so the linker does not discard the crate (its `inventory` registrations would otherwise be absent):

```rust
/// Forces the module crates to link so their `inventory` registrations are present in this binary.
fn link_modules() {
    let _ = (
        &kigumi_mod_base::MANIFEST,
        &kigumi_mod_mail::MANIFEST,
        &kigumi_mod_sales::MANIFEST,
        &kigumi_mod_account::MANIFEST,
        &kigumi_mod_stock::MANIFEST,
    );
}
```

`run()` calls `link_modules()` as its first line, before any command. Without the reference to `MANIFEST`, the module's models, ACLs, and rules would not appear in the catalog.

### The prelude

Every module's `lib.rs` starts with a single import:

```rust
use kigumi::prelude::*;
```

The prelude (`crates/kigumi/src/lib.rs`) re-exports everything you need: the metamodel types (`FieldDef`, `FieldKind`, `Model`, `ModelDescriptor`, `ResolvedModel`), the manifest (`ModuleManifest`, `ModuleDep`), security (`Acl`, `Ctx`, `Operation`, `RecordRule`, `RuleDomain`), the domain (`Domain`, `Value`, `Operator`, `Condition`, ...), compute (`ComputeInput`, `ComputeFn`, `Children`), constraints (`ConstraintFn`), actions (`ActionInput`, `ActionOutcome`, `ActionFn`), reports (`ReportFn`), wizards (`WizardContext`, `WizardDefaultGet`), views (`FormView`, `FieldGroup`, `FieldSlot`, `NotebookPage`), the `FRAMEWORK_VERSION` constant, the `extend`/`model` macros, and from `kigumi_schema` `to_ddl`, `to_ui_contract`, `openapi`, `FieldRule`, `UiRule`. The `register_*!` macros are crate-level macros (`kigumi::register_acls!`, ...), invoked with the `kigumi::` prefix.

---

## 2. Declaring a model

A model is a `struct` annotated with `#[model(name = "...", table = "...")]`. The `#[model]` macro (`crates/kigumi-macros/src/lib.rs`) **replaces** the struct with a marker type (`pub struct StockLocation;`), generates `impl Model` with a static `ModelDescriptor`, and auto-registers the model into the catalog via `inventory::submit!`. The field "types" (`Text`, `Many2one`, ...) are DSL keywords mapped onto `FieldKind`, not real Rust types.

```rust
#[model(name = "stock.location", table = "stock_location")]
pub struct StockLocation {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Type", required, default = "internal", selection = "internal:Internal,supplier:Vendor,customer:Customer,inventory:Inventory Loss,transit:Transit")]
    usage: Selection,

    #[field(label = "Parent Location", target = "stock.location")]
    parent_id: Many2one,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    #[field(label = "Active", default = "true")]
    active: Bool,
}
```

Arguments of `#[model(...)]`:

| Argument | Required | Meaning |
|-----------|--------------|-------------|
| `name` | yes | The model's logical name (e.g. `"stock.location"`). |
| `table` | no | SQL table. If omitted, it derives from `name` by replacing `.` with `_`. |
| `inherits` + `via` | no (together) | Delegation inheritance: the model exposes the parent's stored scalar fields through the `via` FK. Both must be declared, or neither. Emits an `InheritsRegistration`. |

### Field types (FieldKind)

The struct field's "type" selects the `FieldKind` variant (`crates/kigumi-core/src/metamodel.rs`). The aliases the macro recognizes are exactly these; a different alias is a compile error.

| DSL alias | FieldKind | Notes / required attributes |
|-----------|-----------|----------------------------|
| `Text` | `Text` | Plain text. |
| `Html` | `Html` | Rich text (`text`); sanitized on every write (allowlist), `html` widget. |
| `Image` | `Image` | `bigint` FK to `ir.attachment` (the bytes live in the blob store). Read/written as the attachment id. |
| `Integer` | `Integer` | Integer. |
| `Float` | `Float` | Inexact floating point (`double precision`): quantities, weights, factors, rates. |
| `Decimal` | `Decimal { currency_field }` | Exact decimal (`NUMERIC`). `currency = "field"` makes it monetary (`monetary` widget). |
| `Bool` | `Bool` | Boolean. |
| `Date` | `Date` | Date without time (`date`). |
| `Datetime` | `Datetime` | Timestamp with time zone (`timestamptz`). |
| `Selection` | `Selection(&[(k, label)])` | Requires `selection = "k:Label,..."`. |
| `Many2one` | `Many2one { target }` | N→1 relation, generates an FK column. Requires `target = "model.name"`. |
| `One2many` | `One2many { target, inverse }` | 1→N relation, no column (it lives on the inverse). Requires `target` and `inverse`. |
| `Many2many` | `Many2many { target, relation, column, target_column }` | N↔N relation via a junction table. No column on the model. Requires all four. |

### Full reference for the `#[field(...)]` attributes

All attributes are parsed in `build_field` (and the auxiliary submission functions) in `crates/kigumi-macros/src/lib.rs`. They fill in the `FieldDef` (`metamodel.rs`) or emit a side registration.

| Attribute | Form | Effect |
|-----------|-------|---------|
| `label` | `label = "..."` | UI label. Default: the field's name. |
| `required` | flag | `FieldDef.required = true` (NOT NULL). |
| `default` | `default = "..."` | Default value as a string, parsed per type, applied on create when the field is unset. |
| `selection` | `selection = "k:Label,..."` | Key:label pairs for a `Selection` field. |
| `target` | `target = "model.name"` | Target model of `Many2one` / `One2many` / `Many2many`. |
| `inverse` | `inverse = "field"` | Inverse field of a `One2many` (the `Many2one` on the child). |
| `relation` / `column` / `target_column` | strings | Junction table and the two columns of a `Many2many` (`column` → this model, `target_column` → the target). |
| `related` | `related = "path"` | `related` field: a non-stored, read-only mirror of the value reached by following a relational path (e.g. `order_id.partner_id`). Emits a `RelatedRegistration`; generates no column (`stored = false`). |
| `compute` | `compute = "fn_name"` | Name of the compute function registered for the field. |
| `depends` | `depends = "a,b,line_ids.x"` | Compute dependencies (CSV). Checked by `validate_depends`: a non-existent dependent field is an error. An on-read (non-stored) compute cannot depend on a dotted relational path. |
| `store` | flag | Stores a computed field (column materialized on write). Without `store`, a field with `compute` is on-read (no column, recomputed on every read). |
| `tracked` | flag | Tracks the field's changes in the chatter (requires a mailed model). Emits a `TrackedFieldRegistration`. |
| `groups` | `groups = "a,b"` | Field-level security (D6): both reading AND writing the field require membership in at least one of the groups. Emits a `FieldGroupRegistration`. The superuser bypasses it. |
| `currency` | `currency = "field"` | For `Decimal` only: the linked currency field (`monetary` widget). |
| `unique` | flag | Generates a single-column `UNIQUE` constraint in the DDL. |
| `check` | `check = "SQL expr"` | Raw SQL `CHECK` expression (trusted, compile-time) → a column `CHECK` constraint. |

Storage rule (`stored`) computed by the macro:

- `One2many`, `Many2many`, and fields with `related` are never stored (no column).
- A field with `compute` is stored only if it also has `store`.
- All others are stored.

Real examples (from `modules/sales/src/lib.rs` and `modules/base/src/lib.rs`):

```rust
// Monetary decimal, exact aggregate over the children, stored:
#[field(label = "Total", compute = "compute_amount", depends = "line_ids.price_total", currency = "currency_id", store)]
amount_total: Decimal,

// Related field (read-only mirror): the order's customer, from order_id.partner_id.
#[field(label = "Customer", target = "res.partner", related = "order_id.partner_id")]
order_partner_id: Many2one,

// Field-level security: cost is manager-only (read and write).
#[field(label = "Cost", default = "0", groups = "sales.manager")]
purchase_price: Decimal,

// unique + check on res.currency:
#[field(label = "Code", required, unique)]
code: Text,
#[field(label = "Decimal Places", default = "2", check = "decimal_places >= 0")]
decimal_places: Integer,
```

### Extending a model with `#[extend]`

`#[extend("model.name")]` adds fields to a model defined elsewhere, without touching its base. The extension auto-registers as a `FieldExtension` and is merged by `resolve_registered` with conflict checking (a field that already exists is an error). From the `sales` module:

```rust
/// `sale_margin` extension: adds `margin` via `#[extend]`, WITHOUT touching the base.
#[extend("sale.order")]
pub struct SaleMargin {
    #[field(label = "Margin", compute = "compute_margin", depends = "line_ids.margin", currency = "currency_id", store)]
    margin: Decimal,
}
```

Fields accept the exact same set of `#[field(...)]` attributes as `#[model]`. This is the mechanism by which, for example, the account module can "adopt" `account.tax` (same name, no migration) by adding fields.

---

## 3. The registries

All registries are crate-level macros invoked at the **top level** of the module. Each one emits an `inventory::submit!` with a specific struct.

### `register_module!` — the manifest

Every module declares a static `ModuleManifest` and registers it. From `modules/stock/src/lib.rs`:

```rust
pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "stock",
    version: "1.0.0",
    framework: ">=0.2, <0.3",
    depends: &[
        ModuleDep { name: "base", req: "^1.0" },
        ModuleDep { name: "sales", req: "^1.0" },
        ModuleDep { name: "mail", req: "^1.0" },
    ],
    summary: "Inventory — locations, quants, pickings and moves",
};
kigumi::register_module!(MANIFEST);
```

`ModuleManifest` (`crates/kigumi-core/src/manifest.rs`) has the fields `name`, `version` (the module's SemVer), `framework` (the framework compatibility range, e.g. `">=0.2, <0.3"`), `depends` (a slice of `ModuleDep { name, req }` with checked SemVer ranges), and `summary`. The resolver (`resolve_module_set`) validates framework compatibility, dependency ranges, the absence of duplicates, self-dependencies, and cycles, and returns the modules in topological order.

### `register_acls!` — the `Acl` struct

Model-level ACLs (`crates/kigumi-core/src/security.rs`). A `&'static [Acl]` registered; the server collects them via `registered_acls()`.

```rust
pub struct Acl {
    pub model: &'static str,
    pub group: &'static str,
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub delete: bool,
}
```

Access is granted if **any one** of the user's groups grants it (union semantics). The superuser is always allowed. From `modules/stock/src/lib.rs`:

```rust
pub static ACLS: &[Acl] = &[
    Acl { model: "stock.location", group: "stock.user", read: true, write: false, create: false, delete: false },
    Acl { model: "stock.location", group: "stock.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "stock.picking", group: "stock.user", read: true, write: true, create: true, delete: false },
    Acl { model: "stock.move", group: "stock.user", read: true, write: true, create: true, delete: true },
    // ...
];
kigumi::register_acls!(ACLS);
```

### `register_rules!` — `RecordRule`, `RuleDomain`, and the `Domain` DSL

Record rules are row-level rules. The domain is not a runtime-evaluated string, but typed data compiled into parameterized SQL.

```rust
pub struct RecordRule {
    pub model: &'static str,
    pub groups: &'static [&'static str],   // empty = global (applies to everyone)
    pub ops: &'static [Operation],         // Read / Write / Create / Delete
    pub domain: RuleDomain,
}

pub enum RuleDomain {
    Static(fn() -> Domain),   // static module rule (a thunk: Domain is not const-constructible)
    Owned(Domain),            // rule loaded from the DB at runtime
}
```

Combination semantics (`record_rule_domain`): global rules (without a group) are all required → AND; the rules of the groups applicable to the user are alternatives → OR; the two sets are then AND-ed together. The superuser is subject to no restriction.

The `Domain` DSL (`crates/kigumi-core/src/domain.rs`) is built fluently:

```rust
Domain::field("state").ne("done")                       // state <> 'done'
Domain::field("amount_total").lt(10_000_i64)            // amount_total < 10000
Domain::field("order_id.state").ne("done")             // dotted path → subquery
Domain::field("partner_id").is_not_null()
Domain::field("state").in_(["draft", "sale"])

// combinators:
a.and(b)   a.or(b)   a.not()
```

Operators available on the `FieldBuilder`: `eq`, `ne`, `lt`, `le`, `gt`, `ge`, `like`, `ilike`, `is_null`, `is_not_null`, `in_`, `not_in`. A dotted path through a `Many2one`/`One2many` becomes a subquery (NULL-safe), so rules can traverse relations. Invalid domains (non-existent field, incompatible type, operator unsuited to the type, non-relational path) are rejected when the domain is compiled, not in production.

A real example from `stock` (the moves of a "done" transfer are frozen):

```rust
fn move_picking_not_done() -> Domain {
    Domain::field("picking_id.state").ne("done")
}

pub static RECORD_RULES: &[RecordRule] = &[
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Write], domain: RuleDomain::Static(move_picking_not_done) },
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Create], domain: RuleDomain::Static(move_picking_not_done) },
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Delete], domain: RuleDomain::Static(move_picking_not_done) },
];
kigumi::register_rules!(RECORD_RULES);
```

### `register_action!` — action functions and `ActionOutcome`

An action is a named state transition on a model, executable via `POST /api/<model>/<id>/action/<name>`. The signature:

```rust
pub type ActionFn = fn(&ActionInput) -> Result<ActionOutcome, String>;
```

`ActionInput` (`crates/kigumi-core/src/action.rs`) is the read-only view of the current record, with typed accessors: `str(field)`, `int(field)`, `decimal(field)`, `bool(field)`, `get(field)`. The guard ("only if draft") lives in the body and returns `Err(message)` to reject. `ActionOutcome` collects the field updates (`set`) plus an optional `assign_sequence(field, code)` directive, resolved by the persistence layer (for gap-free numbering). From `modules/sales/src/lib.rs`:

```rust
fn confirm_order(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "draft" => Ok(ActionOutcome::new()
            .set("state", Value::Str("sale".to_string()))
            .set("invoice_status", Value::Str("to_invoice".to_string()))
            .assign_sequence("name", "SO")),
        s => Err(format!("can only confirm a draft order (state is '{s}')")),
    }
}
kigumi::register_action!("sale.order", "confirm", confirm_order, &["sales.user"]);
```

The last argument is the slice of groups that may execute the action (on top of the model's Write ACL + record rules); `&[]` does not restrict further.

### `register_sequence!` — document numbering

`assign_sequence` needs its code to exist. A module declares its sequences next to the action that consumes them; migrate ensures them (an existing sequence keeps its counter — upgrades never reset numbering), and a cross-module code collision fails migrate with both module names:

```rust
kigumi::register_sequence!("sales", "SO", "SO/", "", 5);   // module, code, prefix, suffix, padding → SO/00001
```

### `register_seed!` — reference data

Idempotent reference-data seeding, run at **every** migrate while the module is installed, in module dependency order (account's chart can rely on base's company already existing). The body must never overwrite an operator change — guard every insert with a count/exists check: the database is the authority.

```rust
pub async fn seed_base_data(db: &Db) -> Result<(), DbError> { /* guarded inserts */ }
kigumi::register_seed!("base", seed::seed_base_data);
```

### `register_migration!` — the upgrade contract

A module ships its data migrations next to its models. When migrate finds the module installed at a ledger version older than the linked crate, it applies the registered steps with `ledger < to_version <= linked` in semver order, **bumping the ledger after each step** — a failed upgrade resumes exactly where it stopped, so bodies must be idempotent (at-least-once, like jobs). A fresh install replays nothing (the declarative schema is already current-shape); downgrades are refused; a step for an unknown module, a duplicate `to_version`, or a step beyond the linked crate version fails migrate loudly. Uninstalling keeps the ledger row flagged, so a later re-install replays the migrations the kept data actually missed.

```rust
// 1.0.0 → 1.1.0: orders gain `reference`; existing rows get a legacy one.
pub async fn backfill_references(db: &Db) -> Result<(), DbError> { /* idempotent backfill */ }
kigumi::register_migration!("myshop", "1.1.0", backfill_references);
```

Bump `version` in the module's `ModuleManifest` in the same change; `migrate` prints one `upgraded module <name> to <version>` line per applied step.

### `register_report!`

A report is a pure `fn(&serde_json::Value) -> String` that renders a record (with its One2many children already inlined) into an HTML document, exposed at `GET /api/<model>/<id>/report/<name>` and protected by read access to the record.

```rust
pub type ReportFn = fn(&serde_json::Value) -> String;
```

```rust
kigumi::register_report!("sale.order", "quotation", "Quotation", render_quotation);
```

The arguments are `model`, `name` (URL segment), `title` (human label / file name), and the render function. The stored content is untrusted: it must always be escaped (the real `render_quotation` uses an `esc` helper to avoid persistent XSS).

### `register_wizard!` and `register_transient!` — transient models and `default_get`

A wizard is a **transient** model (a scratchpad with ephemeral rows, reclaimed by an hourly cron by age) tied to a `default_get` function. The transient model must declare a nullable `create_date: Datetime` field (the migration gives it a `DEFAULT now()`). From `modules/sales/src/lib.rs`:

```rust
#[model(name = "sale.order.discount", table = "sale_order_discount")]
pub struct SaleOrderDiscount {
    #[field(label = "Order", required, target = "sale.order")]
    order_id: Many2one,
    #[field(label = "Discount %", default = "0")]
    discount: Decimal,
    // GC timestamp: migration gives this a DEFAULT now(); the transient cron reclaims aged rows.
    #[field(label = "Created")]
    create_date: Datetime,
}
kigumi::register_transient!("sale.order.discount");
kigumi::register_wizard!("sale.order.discount", default_get_discount);

/// default_get: seed `order_id` from the open context's active record.
fn default_get_discount(ctx: &WizardContext) -> Vec<(&'static str, Value)> {
    match ctx.active_id {
        Some(id) => vec![("order_id", Value::Int(id))],
        None => vec![],
    }
}
```

`WizardDefaultGet` has the signature `fn(&WizardContext) -> Vec<(&'static str, Value)>`; `WizardContext` (`crates/kigumi-core/src/wizard.rs`) carries `active_model`, `active_id`, `active_ids`. It is pure in v1 (no DB access). The wizard opens via `POST /api/<model>/open`, which computes the defaults, creates the scratchpad row under the caller (normal create ACL), and returns it. The "apply" logic is a dedicated per-wizard service method plus an endpoint (e.g. `apply_discount` → `POST /api/sale.order.discount/<id>/apply_discount`), **not** part of the wizard registration.

### `register_mailed!`

One line, no mixin: the model acquires a thread of messages, followers, and activities through the polymorphic `(res_model, res_id)` link, and the framework cleans up the thread when the record is deleted. It is the precondition for `#[field(tracked)]` to record changes in the chatter.

```rust
kigumi::register_mailed!("stock.picking");
```

### `register_view!` — `FormView`, `FieldGroup`, `FieldSlot`, `NotebookPage`

A form view (`crates/kigumi-core/src/view.rs`) is static data emitted in the UI contract, so the frontend renders a real view instead of dumping the fields in declaration order. The structs:

```rust
pub struct FieldSlot   { pub name: &'static str, pub full: bool }            // full = spans both columns
pub struct FieldGroup  { pub title: Option<&'static str>, pub fields: &'static [FieldSlot] }
pub struct NotebookPage{ pub title: &'static str, pub fields: &'static [&'static str] }
pub struct FormView    { pub model: &'static str, pub groups: &'static [FieldGroup], pub pages: &'static [NotebookPage] }
```

The macro takes `model`, the slice of `FieldGroup`, and the slice of `NotebookPage`, and emits a `FormView`. From `modules/base/src/lib.rs`:

```rust
kigumi::register_view!(
    "res.partner",
    &[
        FieldGroup {
            title: None,
            fields: &[
                FieldSlot { name: "name", full: true },
                FieldSlot { name: "is_company", full: false },
                FieldSlot { name: "parent_id", full: false },
                FieldSlot { name: "active", full: false },
            ],
        },
        FieldGroup {
            title: Some("Contact"),
            fields: &[FieldSlot { name: "email", full: false }, FieldSlot { name: "phone", full: false }],
        },
    ],
    &[]   // no notebook page
);
```

For a view with a notebook (a One2many relation in a tab), from `sales`:

```rust
&[NotebookPage { title: "Order lines", fields: &["line_ids"] }]
```

---

## 4. Compute functions

A compute is a pure `fn(&ComputeInput) -> Value` registered by name with `register_compute!`. The engine (`crates/kigumi-core/src/compute.rs`) fills in every computed field, whether stored (on write, `compute_stored`) or on-read (on every read, `compute_on_read`), whose function is registered.

`ComputeInput` is the read-only view of the record (its fields + the One2many children). Scalar accessors: `int`, `float`, `str`, `bool`, `decimal`, `get`. Aggregation accessors over the children: `children(o2m)`, `count(o2m)`, `sum_float(o2m, child_field)`, `sum_decimal(o2m, child_field)` (exact sum, no f64 rounding). `Value` is the value enum (`Str`, `Int`, `Float`, `Decimal`, `Bool`, `Null`, `List`).

Same-record compute (both inputs on the record) and aggregate compute (over the children), from `sales`:

```rust
/// A line's subtotal = discounted net (qty × unit price × (1 - discount%)).
fn compute_line_subtotal(i: &ComputeInput) -> Value {
    Value::Decimal(line_net(i))
}
kigumi::register_compute!("compute_line_subtotal", compute_line_subtotal);

/// amount_total of an order = exact sum of its lines' taxed totals.
fn compute_amount(i: &ComputeInput) -> Value {
    Value::Decimal(i.sum_decimal("line_ids", "price_total"))
}
kigumi::register_compute!("compute_amount", compute_amount);
```

Key rules:

- A stored compute (a field with `compute` + `store`) is evaluated on write, can aggregate over the children, and can declare `depends` with dotted relational paths.
- An on-read compute (a field with `compute` without `store`) is evaluated on every read, is same-record (the children are not loaded), and **cannot** have dotted `depends` (`validate_depends` rejects it).
- `depends` are checked: a dependency on a non-existent field is a resolution error.

---

## 5. In-transaction constraints (`constrains`)

A cross-record constraint (`crates/kigumi-core/src/constraints.rs`) runs **inside the write transaction**, after the record and its One2many children have been written and re-read, and rejects the write (typed error, rollback) if the invariant is violated. Unlike a SQL `CHECK` (single-row), it reads the header together with the children through the same `ComputeInput` as the compute engine, so it expresses invariants that span a header and its lines.

The signature and the limitation:

```rust
pub type ConstraintFn = fn(&ComputeInput) -> Result<(), String>;
```

**Important limitation: a `ConstraintFn` has no DB access.** It reads only the values already present in the `ComputeInput` (the record and the written children); it cannot run queries. Invariants that require reading other records (e.g. the company of an account referenced via an FK) are not expressible here and must be closed off with a record rule or a company-aware FK validation.

You register it with `register_constraint!(model, &[trigger_fields], func)`. The trigger fields are the written fields (and the One2many field names) that drive the invariant; an empty list runs the constraint on every write. On create it always runs. The canonical example (a balanced accounting entry) from `modules/account/src/lib.rs`:

```rust
fn check_balanced(m: &ComputeInput) -> Result<(), String> {
    let debit: Decimal = m.sum_decimal("line_ids", "debit");
    let credit: Decimal = m.sum_decimal("line_ids", "credit");
    if debit != credit {
        return Err(format!("unbalanced journal entry: total debit {debit} != total credit {credit}"));
    }
    Ok(())
}
kigumi::register_constraint!("account.move", &["line_ids"], check_balanced);
```

In v1, constraints run on the top-level model that is written: a constraint on a child written through the parent's nested One2many commands, or on a parent in delegation inheritance (`inherits`/`via`), is not evaluated.

---

## 6. Cross-record operations: services, routes, and jobs

Actions, computes, and constraints cover single-record transitions and header+lines invariants. For everything beyond them, three seams — all registered from the module, all dispatched generically by the server, no server code to touch.

### `register_service!` — cross-record work, one transaction

A service is a business method that touches **multiple records atomically** (creating linked documents, moving stock, posting entries), runnable via `POST /api/<model>/<id>/service/<name>`. The body receives a `ServiceCtx` that owns ONE transaction: commit on `Ok`, rollback on `Err` — including everything enqueued through it.

```rust
pub async fn complete_order(cx: &mut ServiceCtx<'_, '_>, input: ServiceInput) -> Result<ServiceOutput, DbError> {
    let order_model = cx.resolve("workshop.order")?;
    let ctx = cx.caller().clone();

    // Secured read under the caller's ACLs/rules; state guard in plain code.
    let order = cx.find_one_secured(&order_model, &ctx, input.record_id).await?
        .ok_or_else(|| DbError::BadInput("order not found or not permitted".to_string()))?;
    if order.get("state").and_then(|v| v.as_str()) != Some("in_progress") {
        return Err(DbError::BadInput("can only complete an order in progress".to_string()));
    }
    let patch = serde_json::json!({ "state": "done" });
    cx.update_secured(&order_model, &ctx, input.record_id, patch.as_object().unwrap()).await?;
    // Transactional enqueue: the job exists iff the state change commits.
    cx.enqueue_job("workshop_close_note", serde_json::json!({ "order_id": input.record_id })).await?;
    Ok(ServiceOutput::json(serde_json::json!({ "done": true })))
}
kigumi::register_service!("workshop.order", "complete", complete_order, true, &["workshop.user"]);
```

The fourth argument is the write gate (`true` = the caller must hold Write on the model); the last is the extra group restriction. Where a system effect must exceed the caller's rights (a salesperson creating a stock picking), gate explicitly first, then elevate: `let elevated = ctx.sudo();` — the grep-able idiom for every elevation.

### `register_route!` — bespoke module HTTP

For endpoints that are not shaped like a model — an inbound-webhook receiver, a custom search — a module registers a route on the generic dispatch `GET|POST /api/x/<name>`, keyed by `(name, method)`:

```rust
pub async fn parts_webhook(db: &Db, input: RouteInput) -> Result<RouteOutput, DbError> {
    let secret = std::env::var("WORKSHOP_WEBHOOK_SECRET").unwrap_or_default();
    let signature = input.headers.get("x-parts-signature").cloned().unwrap_or_default();
    if secret.is_empty() || !input.verify_hmac_sha256(secret.as_bytes(), &signature) {
        return Err(DbError::AccessDenied { model: "workshop.order.line".to_string(), operation: "create" });
    }
    // Sender verified: elevate explicitly and do the write.
    let su = input.ctx.clone().sudo();
    /* ... insert via db.insert_secured(&model, &su, &[], &[], values) ... */
    Ok(RouteOutput::Json(serde_json::json!({ "ok": true })))
}
kigumi::register_route!("parts-webhook", Post, false, &[], parts_webhook);
```

`auth: false` runs the body under the GUEST context (uid −1, no groups): the default-deny ACL blocks every secured call until the body verifies the sender itself — use `RouteInput::verify_hmac_sha256` (constant-time) or your provider's exact scheme, never a hand-rolled hash compared with `==`. `RouteInput` carries `ctx`, `query`, `body` (parsed JSON object), `raw_body` (for signatures), and lowercased `headers`. `RouteOutput::Text` exists for challenge handshakes. Bodies are capped at 2 MB.

### `register_job!` — background work with retries

The ad-hoc counterpart of cron ("run X now, async, with retries"). Jobs live in the `kigumi_job` Postgres table — no broker — claimed with `SKIP LOCKED` (multiple workers are safe), retried with exponential backoff up to `max_attempts`, then dead-lettered. Bodies MUST be idempotent (at-least-once execution):

```rust
pub async fn close_note_job(db: &Db, payload: serde_json::Value) -> Result<(), DbError> { /* ... */ }
kigumi::register_job!("workshop_close_note", 5, close_note_job);
```

Enqueue with `Db::enqueue_job(name, payload)`, or — from a service — `ServiceCtx::enqueue_job`, which rides the service transaction: the job exists iff the business write committed. An unregistered name fails fast at enqueue; a job kind not registered in this binary is left claimable for a capable worker (mixed fleets during rolling deploys).

--

## 7. End-to-end example: a small `library` module

Let's put it all together with a new, minimal module: a book catalog.

### 7.1 `modules/library/Cargo.toml`

```toml
[package]
name = "kigumi-mod-library"
description = "Kigumi library module: a tiny book catalog"
version = "2.0.0"
edition.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
kigumi = { workspace = true }
# Depends on base to use res.partner (the author) as a relation target.
kigumi-mod-base = { path = "../base", version = "2.0.0" }

[dev-dependencies]
kigumi-db = { workspace = true }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
serde_json = "1"
```

### 7.2 `modules/library/src/lib.rs`

```rust
//! Application module `library`: a tiny book catalog.
use kigumi::prelude::*;

pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "library",
    version: "1.0.0",
    framework: ">=0.2, <0.3",
    depends: &[ModuleDep { name: "base", req: "^1.0" }],
    summary: "A tiny book catalog",
};
kigumi::register_module!(MANIFEST);

#[model(name = "library.book", table = "library_book")]
pub struct LibraryBook {
    #[field(label = "Title", required)]
    name: Text,

    #[field(label = "ISBN", unique)]
    isbn: Text,

    #[field(label = "Author", target = "res.partner")]
    author_id: Many2one,

    #[field(label = "Status", required, default = "available", selection = "available:Available,borrowed:Borrowed")]
    state: Selection,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// Access control: members read the catalog, librarians maintain it.
pub static ACLS: &[Acl] = &[
    Acl { model: "library.book", group: "library.member", read: true, write: false, create: false, delete: false },
    Acl { model: "library.book", group: "library.librarian", read: true, write: true, create: true, delete: true },
];
kigumi::register_acls!(ACLS);

/// Members never see borrowed books in the catalog list.
fn only_available() -> Domain {
    Domain::field("state").eq("available")
}
pub static RECORD_RULES: &[RecordRule] = &[
    RecordRule {
        model: "library.book",
        groups: &["library.member"],
        ops: &[Operation::Read],
        domain: RuleDomain::Static(only_available),
    },
];
kigumi::register_rules!(RECORD_RULES);

/// `borrow`: an available book becomes borrowed.
fn borrow_book(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "available" => Ok(ActionOutcome::new().set("state", Value::Str("borrowed".to_string()))),
        s => Err(format!("only an available book can be borrowed (state is '{s}')")),
    }
}
kigumi::register_action!("library.book", "borrow", borrow_book, &["library.librarian"]);

/// Form layout.
kigumi::register_view!(
    "library.book",
    &[FieldGroup {
        title: None,
        fields: &[
            FieldSlot { name: "name", full: true },
            FieldSlot { name: "isbn", full: false },
            FieldSlot { name: "author_id", full: false },
            FieldSlot { name: "state", full: false },
            FieldSlot { name: "active", full: false },
        ],
    }],
    &[]
);
```

### 7.3 Linking the module

In `apps/kigumi-cli/Cargo.toml`:

```toml
kigumi-mod-library = { path = "../../modules/library" }
```

In `apps/kigumi-cli/src/main.rs`, inside `link_modules()`:

```rust
let _ = (
    &kigumi_mod_base::MANIFEST,
    // ... the others ...
    &kigumi_mod_library::MANIFEST,
);
```

### 7.4 Installing the module

On a fresh database the migration installs only `base` (+ closure); the other modules are opt-in. After compiling the `kigumi` binary:

```sh
# Migrate the framework + base schemas (initial installation)
kigumi migrate

# Install library and its dependency closure (deps first), then migrate the tables
kigumi module install library

# Verify
kigumi module list
```

`module install` calls `module_closure(name)` (the name + its transitive dependencies, deps first), marks the modules as installed, and then re-runs `migrate_installed` (idempotent) to create the tables. You then start the server (which serves only the models of the installed modules):

```sh
kigumi serve
```

### 7.5 Curl against the generated API

All data routes require a bearer. First you log in (`POST /auth/login` returns `access_token` / `refresh_token` / `token_type: "Bearer"` / `expires_in`). The server listens by default on `127.0.0.1:8099` (`server.bind` in `kigumi.toml`):

```sh
TOKEN=$(curl -s http://127.0.0.1:8099/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"login":"admin","password":"'"$KIGUMI_ADMIN_PASSWORD"'"}' \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["access_token"])')

# Create a book (POST /api/:name → { "id": <n> } with 201)
curl -s http://127.0.0.1:8099/api/library.book \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"The Rust Programming Language","isbn":"9781718500457","state":"available"}'

# List (GET /api/:name) → envelope { data, total, limit, offset }
curl -s http://127.0.0.1:8099/api/library.book -H "Authorization: Bearer $TOKEN"

# Run the borrow action (POST /api/:name/:id/action/:action)
curl -s -X POST http://127.0.0.1:8099/api/library.book/1/action/borrow \
  -H "Authorization: Bearer $TOKEN"

# Fetch the view's UI contract (GET /api/:name/view)
curl -s http://127.0.0.1:8099/api/library.book/view -H "Authorization: Bearer $TOKEN"
```

The generated CRUD routes are `GET/POST /api/:name`, `GET/PATCH/DELETE /api/:name/:id`, the action `POST /api/:name/:id/action/:action`, the report `GET /api/:name/:id/report/:report`, and the wizard open `POST /api/:name/open`. See [api.md](api.md) for the complete list.

---

## 8. Integration test for a module

The pattern used in `modules/stock/tests/` is: a `#[tokio::test]` test that **skips** if `DATABASE_URL` is not set, links the modules, recreates the schema from `migration_plan()`, and operates on `Db` with a superuser `Ctx`. The test dependencies live in `[dev-dependencies]` (see `kigumi-db`, `tokio`, `serde_json` in the module's `Cargo.toml`).

A skeleton (from `modules/stock/tests/validate.rs`, trimmed):

```rust
use kigumi::prelude::*;
use kigumi_db::Db;
use serde_json::json;

/// Forces the modules to link so their inventory registrations are in the test binary.
fn link() {
    let _ = (
        &kigumi_mod_stock::MANIFEST,
        &kigumi_mod_sales::MANIFEST,
        &kigumi_mod_base::MANIFEST,
        &kigumi_mod_mail::MANIFEST,
    );
}

#[tokio::test]
async fn validate_moves_stock_and_is_single_shot() {
    link();
    // Skips without DATABASE_URL (so the suite passes even without a DB).
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let db = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();

    // Recreate the schema from the migration plan (ordered by FK), idempotent.
    let plan = migration_plan().unwrap();
    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
    for t in &plan { db.create_table(&t.model).await.unwrap(); }
    for t in &plan { db.create_m2m_relations(&t.model).await.unwrap(); }
    db.ensure_stock_indexes().await.unwrap();
    db.ensure_sequence_schema().await.unwrap();

    // Resolve the models from the catalog and insert data with insert_secured.
    let picking = resolve_registered("stock.picking").unwrap();
    // ... let receipt = db.insert_secured(&picking, &su, &[], &[], v.as_object().unwrap()).await.unwrap();

    // Run the service method and make the assertions.
    let n1 = db.validate_picking(&su, &[], &[], receipt).await.unwrap();
    assert!(n1.starts_with("IN/"));

    // Cleanup.
    for t in plan.iter().rev() { db.drop_table(&t.model).await.unwrap(); }
}
```

Essential points of the pattern:

- `link()` references the `MANIFEST` of every module involved (including the dependencies), otherwise their models would not be registered in the test binary.
- The test returns without failing if `DATABASE_URL` is not present, so `cargo test` stays green even on a machine without Postgres.
- `migration_plan()` yields the targets ordered by FK (`MigrationTarget { module, version, model }`); they are dropped in reverse order and created in forward order, then the Many2many relations in a second pass.
- You work with a superuser `Ctx` (`Ctx::new(0, vec![]).sudo()`) to skip ACL/record rules in the setup, and you use the `Db`'s `*_secured` methods (`insert_secured`, `find_one_secured`, `find_secured`, `count_secured`).

For non-DB checks (descriptor shape, presence of attributes) a `#[cfg(test)]` unit test in the module's `lib.rs` is enough — one that calls `resolve_registered("...")` and inspects the `FieldDef`s, as in the examples in `modules/sales/src/lib.rs`.
