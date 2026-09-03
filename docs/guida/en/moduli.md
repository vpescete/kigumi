# Modules

Kigumi is a schema-driven, headless ERP framework: every domain feature is packaged into a **module**. A module is a Rust crate that, through a `ModuleManifest` and a handful of `register_*!` macros, declares its models, ACLs, record rules, actions, and service methods in the compile-time registry. This page describes the module system — the manifest, dependency declaration, dependency closure, install/uninstall semantics, the install registry, and the framework compatibility check — and then catalogs the bundled modules (`base`, `mail`, `sales`, `account`, `stock`) along with the models they ship and their main features. To write your own module, see [moduli-custom.md](./moduli-custom.md).

## The `ModuleManifest`

Every module declares a static `ModuleManifest` — declarative data validated at build/install time. The struct is defined in `crates/kigumi-core/src/manifest.rs`:

```rust
pub struct ModuleManifest {
    pub name: &'static str,
    /// SemVer of the module, e.g. "1.0.0".
    pub version: &'static str,
    /// Compatibility range with the framework, e.g. ">=0.2, <0.3".
    pub framework: &'static str,
    /// Dependencies on other modules, with version ranges.
    pub depends: &'static [ModuleDep],
    pub summary: &'static str,
}
```

| Field | Type | Meaning |
|-------|------|-------------|
| `name` | `&'static str` | Technical name of the module (e.g. `"sales"`). Must be unique in the catalog. |
| `version` | `&'static str` | SemVer version of the module, independent of the framework's. |
| `framework` | `&'static str` | SemVer compatibility range with the framework (e.g. `">=0.2, <0.3"`). |
| `depends` | `&'static [ModuleDep]` | Dependencies on other modules, each with its own version range. |
| `summary` | `&'static str` | Short description, shown by `kigumi module list`. |

Each dependency is a `ModuleDep`, i.e. the name of the required module plus a SemVer version constraint:

```rust
pub struct ModuleDep {
    pub name: &'static str,
    pub req: &'static str,
}
```

Unlike a plain list of names, each dependency carries a **verifiable version range** (e.g. `^1.0`): resolution checks that the depended-on module is present *and* that its version satisfies the range, not merely that it exists.

### `register_module!`

The manifest alone is not visible to the catalog: the module must register it. This is done with a single line, at the module's top level, right after defining the `MANIFEST`:

```rust
pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "base",
    version: "1.0.0",
    framework: ">=0.2, <0.3",
    depends: &[],
    summary: "Foundational models: currency, partner, company",
};
kigumi::register_module!(MANIFEST);
```

The macro (defined in `crates/kigumi/src/lib.rs`) emits a `ModuleRegistration` into the compile-time registry via `inventory`, also preserving the crate's `module_path!()`. That path is what lets `module_of(model)` trace back from a model to the module that owns it — the basis of per-installed-module gating in migration and at serve time.

For the `inventory` registrations to be present in the binary, the module crate must actually be linked. The `kigumi` binary forces this in `apps/kigumi-cli/src/main.rs`:

```rust
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

A module is **available** when its crate is linked into the binary (compile time); it becomes **installed** only when it has a row in the install registry (see below).

## The framework compatibility check

Before any resolution, every manifest is compared against the framework version by the `check_compat` function (in `crates/kigumi-core/src/manifest.rs`). The framework version is the `FRAMEWORK_VERSION` constant, derived from `kigumi-core`'s `CARGO_PKG_VERSION` (defined in `crates/kigumi-core/src/lib.rs`) — currently `0.2.0`. All bundled modules declare `framework = ">=0.2, <0.3"`, so they are compatible with this line.

```rust
pub fn check_compat(
    manifest: &ModuleManifest,
    framework_version: &str,
) -> Result<(), ResolutionError> {
    let fw = Version::parse(framework_version)?;
    let _ = Version::parse(manifest.version)?;
    let req = VersionReq::parse(manifest.framework)?;
    if !req.matches(&release_of(&fw)) {
        return Err(ResolutionError::Incompatible {
            module: manifest.name.to_string(),
            needs: manifest.framework.to_string(),
            found: framework_version.to_string(),
        });
    }
    Ok(())
}
```

A pre-release build (e.g. `0.1.5-rc.1`) is treated as its release line (`0.1.5`) via `release_of`, so RC/dev builds of the same cycle stay in range. A pre-release of the *next* line (`0.2.0-rc.1`), on the other hand, stays out of range and fails.

### Resolution and topological ordering

`resolve_module_set` takes a slice of manifests plus the framework version and returns the modules in **dependency (topological) order** — dependencies before dependents. Along the way it verifies:

- each module's framework compatibility (`check_compat`);
- that every dependency exists in the catalog, otherwise `MissingDependency`;
- that the dependency's version satisfies the required range, otherwise `DependencyConflict`;
- the absence of duplicate names (`DuplicateModule`) and of self-dependencies (`SelfDependency`);
- the absence of cycles — a Kahn-style topological ordering; in case of a cycle, `DependencyCycle` reports **only** the modules actually on the cycle (the downstream tail is removed).

The ordering is deterministic: when availability is tied (several modules ready at the same time), names are processed in alphabetical order, because resolution indexes them in a `BTreeMap`. That is why `account` precedes `sales` in the final order (both become ready after `mail`).

The possible error conditions are enumerated by `ResolutionError`:

| Variant | When |
|----------|--------|
| `BadVersion` / `BadRequirement` | Unparsable SemVer version or range. |
| `Incompatible` | The module is not compatible with the framework version. |
| `MissingDependency` | A declared dependency is not present in the catalog. |
| `DependencyConflict` | The dependency exists but its version does not satisfy the range. |
| `DuplicateModule` | Two modules declare the same `name`. |
| `SelfDependency` | A module lists itself among its dependencies. |
| `DependencyCycle` | The dependency graph contains a cycle. |

The `resolve_modules` wrapper (in `crates/kigumi-core/src/registry.rs`) feeds `resolve_module_set` with all the manifests registered in the catalog and with `FRAMEWORK_VERSION`.

## The dependency closure

When you install a module you don't install just that one: you install its **transitive closure**. The `module_closure(name)` function (in `crates/kigumi-core/src/registry.rs`) returns the module plus all its transitive dependencies, in dependency order (dependencies first):

```rust
pub fn module_closure(name: &str) -> Result<Vec<&'static str>, String> {
    let mods = resolve_modules()?; // validated + topo-sorted
    // ... collect name + transitive dependencies ...
    // Return in the validated dependency order (dependencies before dependents).
}
```

For example, `module_closure("sales")` returns `["base", "mail", "sales"]`, whereas `module_closure("base")` returns `["base"]`. An unknown name produces an error. Because the result is reordered according to the validated module order, the closure never contains a dependent before its dependencies.

## Install and uninstall

Modules are managed from the CLI (`apps/kigumi-cli/src/main.rs`), `module` subcommand:

```text
kigumi module list              # lists the linked modules and whether each is installed
kigumi module install <name>    # installs a module + its closure, then migrates the tables
kigumi module uninstall <name>  # uninstalls a module (tables and data KEPT)
```

`kigumi module list` prints one line per module with name, version, status (`installed` / `available`), and summary:

```text
  base       1.0.0    [installed]  Foundational models: currency, partner, company
  mail       1.0.0    [available]  Headless chatter: messages, tracking, followers, activities
  ...
```

### Install

`kigumi module install <name>` computes the closure with `module_closure(&name)`, marks as installed the modules not yet present, and then calls `migrate_installed`, which (idempotently) creates the tables for the newly installed modules:

```rust
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
```

### Uninstall — data is kept

`kigumi module uninstall <name>` has two guards and non-destructive semantics:

1. **`base` cannot be uninstalled** (it is the foundational module): `cannot uninstall 'base' (the foundational module)`.
2. **Downstream guard**: if an installed module still depends on the requested one, the uninstall is refused until you first uninstall the dependents.
3. **Data is preserved**: the uninstall merely deletes the row from the install registry (`mark_module_uninstalled`). The module stops being migrated and served, **but its tables and data remain intact**; re-installing it recovers everything.

```rust
ModuleCmd::Uninstall { name } => {
    if name == "base" {
        return Err("cannot uninstall 'base' (the foundational module)".into());
    }
    // ... guardia a valle sui dipendenti ...
    db.mark_module_uninstalled(&name).await?;
    println!("uninstalled '{name}' (its tables and data are kept; re-install to restore)");
}
```

This is deliberately non-destructive and reversible: the uninstall is a *disable*, not a *drop*.

## The per-module install registry

The "installed" state lives in a dedicated table, `installed_module`, managed by `crates/kigumi-db/src/module_store.rs`:

```sql
CREATE TABLE IF NOT EXISTS installed_module
  (name text PRIMARY KEY,
   installed_version text NOT NULL,
   installed_at timestamptz NOT NULL DEFAULT now())
```

A module is **available** when its crate is linked (compile time); it is **installed** when it has a row here. The registry operations are `installed_modules()`, `is_module_installed(name)`, `mark_module_installed(name, version)`, and `mark_module_uninstalled(name)`.

There is a second table, `kigumi_module` (in `crates/kigumi-db/src/migration.rs`), which is the **per-model migration ledger**: it tracks the version up to which each module's tables have been migrated, used by `install_or_upgrade` (with a `pg_advisory_xact_lock` to serialize concurrent install/upgrade of the same module). The `has_prior_migration` method distinguishes a truly new DB from one upgraded before module selection existed.

### Installed-driven migration

On a new database nothing is installed, so `migrate` installs `base` first (and its closure); the rest is opt-in. On a DB that already had migrations *before* the introduction of per-module selection, **all** the already-present modules are kept, so that the upgrade doesn't silently hide previously available models:

```rust
if db.installed_modules().await?.is_empty() {
    let mods = resolve_modules()?;
    let want: Vec<&str> = if db.has_prior_migration().await? {
        mods.iter().map(|m| m.name).collect()
    } else {
        module_closure("base")?
    };
    // ... mark_module_installed for each module in `want` ...
}
```

`migrate_installed` then migrates only the models of the installed modules, **in FK dependency order**, creates the Many2many join tables in a second pass (once both ends exist), and finally seeds the base data for the installed modules (`base` → currency + default company + sequences; `account` → chart of accounts + journals; `stock` → warehouse + default locations). At `Serve` time, the router exposes **only** the models of the installed modules: a model whose owning module is not installed is omitted from the served catalog.

## Bundled module catalog

Five modules are bundled and linked into the `kigumi` binary. All declare `version = "2.0.0"` and `framework = ">=0.2, <0.3"`.

| Module | Crate | Depends on (verified by the MANIFEST) |
|--------|-------|--------------------------------------|
| `base` | `kigumi-mod-base` | *(none)* |
| `mail` | `kigumi-mod-mail` | `base ^1.0` |
| `sales` | `kigumi-mod-sales` | `base ^1.0`, `mail ^1.0` |
| `account` | `kigumi-mod-account` | `base ^1.0`, `mail ^1.0` |
| `stock` | `kigumi-mod-stock` | `base ^1.0`, `sales ^1.0`, `mail ^1.0` |

### `base`

The root of the graph: no dependencies, always installed first. It ships the foundational models the other modules build on.

`depends: &[]` (`modules/base/src/lib.rs`).

Models:

| Model | Table | Notes |
|---------|---------|------|
| `res.currency` | `res_currency` | Currency for monetary fields, shared across companies. |
| `res.partner` | `res_partner` | Address book: companies and people (customers, vendors, contacts); self-referential `parent_id` hierarchy. |
| `res.company` | `res_company` | Data isolation unit in multi-company; has a currency (`currency_id`, required) and a partner (`partner_id`) linked. |
| `res.groups` | `res_groups` | (Read-only) list of the groups referenced by ACLs/rules; used by the UI for pickers and filters. |
| `res.users` | `kigumi_user` | Read-only projection of the authentication subsystem; an **external** table (never migrated from the metamodel), via `register_external!("res.users")`. |
| `ir.attachment` | `kigumi_attachment` | File attached to any record via the polymorphic link `(res_model, res_id)`; the bytes live in the content-addressed blob store indexed by `checksum`. |

Main features:

- **Sequences** for document numbering: `base` seeds the `SO` and `PO` sequences (e.g. `SO/00001`, `PO/00001`) used by the sale/purchase confirmation actions.
- **Runtime settings** (typed key/value) managed via `kigumi config set/get/print`; the install-time seeding sets `base_url` (empty) and `mode` (`production`) without ever overwriting an operator change.
- **Multi-company**: `res.company` is the isolation unit; transactional models carry their own `company_id` (e.g. `sale.order`), while partners are shared.
- **Base seeding**: on a fresh instance a currency (`Euro`/`EUR`) and a company (`Main Company`) are created; `res.groups` is populated from the groups referenced by the registered ACLs/rules.
- ACL: the `user` group reads the reference data and the group list (and can create/edit partners); `res.users` and `ir.attachment` (generic CRUD) are `admin`-only — users reach the files through the dedicated `/api/:name/:id/attachments` endpoints (plus `/api/attachment/:aid/content` for download and `/api/attachment/:aid` for deletion), gated on access to the host record.

### `mail`

Headless chatter subsystem. A model opts in with **one line** (`kigumi::register_mailed!("sale.order")`), no mixin: it gains a message thread addressed by the polymorphic link `(res_model, res_id)`, and the framework cleans up that thread when the record is deleted.

`depends: &[ModuleDep { name: "base", req: "^1.0" }]` (`modules/mail/src/lib.rs`). It depends on `base` because `res.users` is the message author / activity assignee.

Models:

| Model | Table | Notes |
|---------|---------|------|
| `mail.message` | `mail_message` | Thread message (comment or system note); append-only, ordered by `id`; `parent_id` for nested replies. |
| `mail.tracking` | `mail_tracking` | Audit row for a field change: a typed `old_value` / `new_value` pair, carried by a `notification` message. |
| `mail.activity` | `mail_activity` | To-do scheduled on a record (`date_deadline` + `user_id` assignee); the `state` (overdue/today/planned) is **derived** from `date_deadline` on read, never stored. |
| `mail.follower` | `mail_follower` | Subscription to a record's thread; uniqueness of `(res_model, res_id, user_id)` via a composite index (`ensure_mail_indexes`). |

Main features:

- **Chatter** via the dedicated `/api/:name/:id/messages` (GET) and `/api/:name/:id/message` (POST) endpoints, gated on read access to the host record: a user posts/reads the threads only of the records they can already see.
- **Tracking** of the fields marked `tracked` in models (e.g. `state` on `sale.order`).
- **Followers** and **activities** on the same polymorphic link (endpoints `/api/:name/:id/followers`, `/follow`, `/unfollow`, `/activities`, `/activity`, `/activities/:aid/done`).
- **Opt-in via `register_mailed!`**: besides the models of other modules, `mail` retrofits `res.partner` (`register_mailed!("res.partner")`), so `base` doesn't need to depend on `mail` (the arrow always goes `mail → base`).
- ACL: the thread models are `admin`-only on the generic CRUD routes (moderation/debug); normal access goes through the chatter endpoints, which act in an elevated manner after the check on the host.

### `sales`

Sales and purchase management, with a product catalog, variants, pricelists, and taxes.

`depends: &[ModuleDep { name: "base", req: "^1.0" }, ModuleDep { name: "mail", req: "^1.0" }]` (`modules/sales/src/lib.rs`). `sale.order` and `product.template` opt into the chatter (`register_mailed!`), hence the dependency on `mail`.

Models:

| Model | Table | Notes |
|---------|---------|------|
| `product.category` | `product_category` | Hierarchical product category. |
| `uom.uom` | `uom_uom` | Unit of measure with a ratio (`factor`) relative to the category reference. |
| `product.template` | `product_template` | Shared product definition (fields common to all variants); opts into the chatter. |
| `product.product` | `product_product` | Sellable variant; `inherits = "product.template"` via `product_tmpl_id`; carries an internal reference (`default_code`), barcode, tags, extra price (`price_extra`), and on-hand quantity (`qty_available`). |
| `product.tag` | `product_tag` | Variant label/tag (comodel of the Many2many `tag_ids`). |
| `product.attribute` | `product_attribute` | Configurable dimension (e.g. "Color"). |
| `product.attribute.value` | `product_attribute_value` | Possible value of an attribute (e.g. "Red"). |
| `product.template.attribute.line` | `product_template_attribute_line` | Attribute line on a template: which values are selected. |
| `product.template.attribute.value` | `product_template_attribute_value` | Per-template cell of a chosen value; the structural FKs are engine-locked (`groups = "base.system"`), only `price_extra` is editable by a manager. |
| `product.pricelist` | `product_pricelist` | Pricelist in a currency. |
| `product.pricelist.item` | `product_pricelist_item` | Pricelist rule with a scope (`applied_on`: variant > product > category > global) and `compute_price` (fixed or % discount). |
| `account.tax` | `account_tax` | Tax (minimal subset, percentage per line); an extension of it by the `account` module via `#[extend]` is foreseen. |
| `sale.order` | `sale_order` | Sales order; `state` and `invoice_status` tracked; `amount_untaxed`/`amount_tax`/`amount_total` aggregates computed from the lines; the `sale_margin` extension adds `margin` via `#[extend]`. |
| `sale.order.line` | `sale_order_line` | Order line: product, quantity, price, discount, tax; `price_subtotal`/`price_tax`/`price_total`/`margin` as stored computes. |
| `purchase.order` | `purchase_order` | Buy-side mirror of `sale.order`. |
| `purchase.order.line` | `purchase_order_line` | Purchase line (same shape as the sales line, reuses the same computes). |
| `sale.order.discount` | `sale_order_discount` | Transient wizard (`register_transient!`) to apply a % discount to all the lines of an order. |

Actions and service methods:

- **State actions**: `confirm` and `done` on `sale.order` (`confirm` assigns the SO number from the sequence and sets `invoice_status = to_invoice`); `confirm` and `done` on `purchase.order` (`confirm` assigns the PO number). Exposed as `POST /api/:name/:id/action/:action`.
- **`generate_variants`** — materializes the attribute combinations of a `product.template` into `product.product` variants. `POST /api/:name/:id/generate_variants` (valid only on `product.template`).
- **`apply_pricelist`** — re-prices the lines of a `sale.order` from its pricelist (same currency). `POST /api/:name/:id/apply_pricelist` (valid only on `sale.order`).
- **Discount wizard** — `POST /api/:name/open` opens the transient wizard (seeding via `default_get`), and `POST /api/:name/:id/apply_discount` writes the discount onto the lines of the target order (valid only on `sale.order.discount`).
- **`create_invoice`** — generates a posted customer invoice (`account.move`) from a confirmed `sale.order`; requires the `account` module to be installed (otherwise the error `install the account module to invoice`) and flips `invoice_status` to `invoiced`. `POST /api/:name/:id/create_invoice` (valid only on `sale.order`).
- **`create_delivery`** — creates a delivery transfer from the lines of a `sale.order`; requires the `stock` module (otherwise `install the stock module to create transfers`). `POST /api/:name/:id/create_delivery` (valid only on `sale.order`).
- **`create_receipt`** — creates a receipt transfer from the lines of a `purchase.order`; requires the `stock` module. `POST /api/:name/:id/create_receipt` (valid only on `purchase.order`).
- **Report** `quotation` on `sale.order` (HTML, with escaping of the stored content). `GET /api/:name/:id/report/:report`.
- ACL: the `sales.user` (order/line operations) and `sales.manager` (catalog maintenance) groups; purchase orders are pragmatically handled by the same groups. The record rules limit visibility to orders that are not "done" (and, for `sales.user`, to small orders only, below 10,000).

### `account`

Headless double-entry bookkeeping: a general ledger.

`depends: &[ModuleDep { name: "base", req: "^1.0" }, ModuleDep { name: "mail", req: "^1.0" }]` (`modules/account/src/lib.rs`). `account.move` opts into the chatter (audit trail), hence the dependency on `mail`.

Models:

| Model | Table | Notes |
|---------|---------|------|
| `account.account` | `account_account` | Chart-of-accounts account; `account_type` drives the behavior (receivable/payable/income/expense/tax…). |
| `account.journal` | `account_journal` | Journal; `code`/`sequence_code` drive the numbering of posted entries. |
| `account.move` | `account_move` | Entry/invoice: groups the debit/credit lines; mailed; numbered `/` until posted; `amount_total` is a stored aggregate. |
| `account.move.line` | `account_move_line` | Entry line: a posting to a GL account; debit XOR credit (two `Decimal` columns); `balance = debit − credit` derived on read. |

Main features:

- **Double-entry posting**: `POST /api/:name/:id/post` posts a draft entry (re-check of balancing + per-journal numbering + `state → posted`); valid only on `account.move`.
- **Balanced-entry constraint** (`check_balanced`, `register_constraint!`): the total debit of an entry must equal the total credit — a cross-record constraint that a single-row SQL CHECK cannot express. An empty entry (Σ = 0) is balanced. A second constraint (`check_line_companies`) prevents mixing companies in the same entry.
- **Posted immutability**: record rules freeze the lines of a posted `account.move` (no write/create/delete) — this is what guarantees the invariant "posted ⇒ balanced". The `button_draft` and `button_cancel` actions handle the state reversals.
- **Sales invoicing**: it is the other end of `sales`' `create_invoice` — `Db::create_sale_invoice` generates and posts a customer `account.move` from a confirmed order.
- **Seeding**: when `account` is installed, `migrate_installed` seeds a minimal chart of accounts + journals (Customer Invoices/Vendor Bills/Bank/Miscellaneous) for the default company.
- ACL: `account.user` (accountant) and `account.manager` (configuration: account creation, journal maintenance, entry deletion).

### `stock`

Headless inventory ledger: locations, warehouses, on-hand stock, transfers, and moves.

`depends: &[ModuleDep { name: "base", req: "^1.0" }, ModuleDep { name: "sales", req: "^1.0" }, ModuleDep { name: "mail", req: "^1.0" }]` (`modules/stock/src/lib.rs`). It depends on `base` (company), `sales` (`product.product`), and `mail` (transfers carry a chatter thread).

Models:

| Model | Table | Notes |
|---------|---------|------|
| `stock.location` | `stock_location` | Location; `usage` drives the behavior — only `internal` counts as real on-hand stock, supplier/customer/inventory/transit are virtual. |
| `stock.warehouse` | `stock_warehouse` | Warehouse: an internal location with a short code. |
| `stock.quant` | `stock_quant` | On-hand stock of a product at a location (materialized); unique on `(product_id, location_id)` via `ensure_stock_indexes`. |
| `stock.picking` | `stock_picking` | Transfer (receipt/delivery/internal): a document grouping the moves from a source to a destination; mailed; `state` tracked. |
| `stock.move` | `stock_move` | Move of a product within a transfer; done when the transfer is validated. |

Main features:

- **`validate` mechanism**: `POST /api/:name/:id/validate` (`Db::validate_picking`) validates a draft transfer — assigns the number from the per-type sequence (`receipt`→`IN`, `delivery`→`OUT`, otherwise `INT`), brings the moves to `done` with a compare-and-set (`FOR UPDATE` + re-assertion of `draft`) to prevent concurrent double validations, and updates the on-hand stock (`stock.quant`). Valid only on `stock.picking`.
- **Integration with orders**: the `create_delivery` (from `sale.order`) and `create_receipt` (from `purchase.order`) methods of the `sales` module generate the corresponding `stock.picking`; the materialized on-hand quantity `product.product.qty_available` is updated by the move-done mechanism.
- **Done immutability**: record rules freeze the moves of a `done` transfer (no write/create/delete) — the stock analog of a posted accounting entry.
- **Seeding**: when `stock` is installed, `migrate_installed` seeds a default warehouse + the standard locations (Stock / Vendors / Customers / Inventory adjustment) for the default company.
- ACL: `stock.user` (transfer/move operations) and `stock.manager` (configuration: locations, warehouses, direct editing of on-hand stock).

## Dependency graph and install order

The dependencies declared in the manifests form this acyclic graph. In edge form (`module → dependencies`):

- `base` → *(none)*
- `mail` → `base`
- `sales` → `base`, `mail`
- `account` → `base`, `mail`
- `stock` → `base`, `sales`, `mail`

Represented as a graph (the arrows go from the dependent to its dependencies; `base` is the root at the bottom):

```text
   stock
   ├──► sales ──► mail ──► base
   ├──► mail ──────────────► base
   └──► base

   account
   ├──► mail ──► base
   └──► base
```

`base` depends on nothing; `mail` depends only on `base`; `sales` and `account` both depend on `base` and `mail`; `stock` depends on `base`, `sales`, and `mail`.

`resolve_module_set` topologically sorts this graph (Kahn, with an alphabetical tiebreak). The resulting install order (dependencies before dependents), confirmed by `kigumi version`, is:

1. `base`
2. `mail`
3. `account`
4. `sales`
5. `stock`

`account` precedes `sales` because both become "ready" as soon as `mail` is installed, and when availability is tied the tiebreak is alphabetical (`account` < `sales`).

Concretely, `module_closure` produces the expected closures (always in the validated order, dependencies first):

- `module_closure("base")` → `["base"]`
- `module_closure("sales")` → `["base", "mail", "sales"]`
- `module_closure("stock")` → `["base", "mail", "sales", "stock"]`

`base` is always installed first on a new database; the other modules are opt-in via `kigumi module install <name>`.

## See also

- [moduli-custom.md](./moduli-custom.md) — how to write your own module (manifest, models, ACL, actions).
- [architettura.md](./architettura.md) — the compile-time registry and the metamodel.
- [sicurezza.md](./sicurezza.md) — ACLs, record rules, and field-level security.
- [api.md](./api.md) — the UI contract and the REST endpoints.
- [installazione.md](./installazione.md) and [configurazione.md](./configurazione.md) — instance bootstrap and configuration.
