# Kigumi 組

**Modules that interlock. No glue.**

Kigumi (木組み — Japanese interlocking joinery) is a **headless, schema-driven business-app
framework in Rust**. One declarative model is the single source of truth: the Postgres schema,
the REST API (OpenAPI 3.1), the JSON UI contract, the security policy — and the MCP surface for
AI agents — are all generated from it. Modules compose at **compile time** through typed seams:
if it builds, it fits.

Business apps rarely die of a bad framework. They die of the same truth written five times — a
migration, an ORM class, a serializer, a form, a permission check — kept aligned by hand.
Kigumi's whole pitch: one definition, or five copies drifting apart.

**Site & docs**: https://vpescete.github.io/kigumi-site/ · **[Changelog](https://vpescete.github.io/kigumi-site/changelog/en/)** · **Status**: pre-1.0, built in the open.

## Quickstart

```sh
cargo install kigumi-cli
kigumi new myshop            # scaffolds a module + a server binary (asks about ERP modules)
cd myshop
createdb myshop
export DATABASE_URL=postgres://localhost/myshop
export KIGUMI_JWT_SECRET=change-me
KIGUMI_ADMIN_PASSWORD=change-me cargo run -p app -- migrate
cargo run -p app -- serve    # REST + security + chatter + live SSE on http://127.0.0.1:8600
cargo run -p app -- mcp admin  # the same app over MCP, ACLs enforced on every tool
```

The generated workspace is born agent-ready: `AGENTS.md`, a Claude Code skill and a
module-author agent ship with it.

## Releases

Current line: **framework 0.2.0** · ERP modules **2.0.0** · `kigumi-cli` **0.2.0**. Every released
version — with its breaking changes named rather than buried — is on the
**[changelog](https://vpescete.github.io/kigumi-site/changelog/en/)**.

Coming from 0.1? Four things changed under you:

- **The catalog is no longer anonymous.** `/openapi.json`, `/api/models` and `/api/:name/view`
  follow the same ACLs as the data they describe. Mind the shape: a request with no token is not
  rejected, it *succeeds as the guest* and returns only what a `public` Read ACL exposes — so a
  client that keys "please log in" off a `401` must react to an empty catalog instead.
- **`ServeOptions` is `#[non_exhaustive]`.** Build it with `ServeOptions::new(jwt_secret)` and
  assign the fields you need; a struct literal no longer compiles, which is the point — the next
  field the framework adds is a recompile, not a breakage.
- **OpenAPI paths moved to the model name** (`/api/sale.order`, not `/api/sale_order`). The old
  spec described paths that 404. Regenerate any SDK built from it.
- **Modules require framework `>=0.2, <0.3`**, which is why they went to 2.0.0 rather than 1.1.0:
  an out-of-range module refuses to boot, so a minor bump would have let Cargo pick a version that
  cannot run.

## One definition, everything derived

```rust
#[model(name = "myshop.order", table = "myshop_order")]
pub struct Order {
    #[field(label = "Customer", required, target = "res.partner")]
    partner_id: Many2one,
    #[field(label = "State", default = "draft", selection = "draft:Draft,open:Open,done:Done")]
    state: Selection,
    #[field(label = "Total", compute = "myshop_order_total", depends = "line_ids.subtotal", store)]
    total: Decimal,
    #[field(label = "Lines", target = "myshop.order.line", inverse = "order_id")]
    line_ids: One2many,
}
```

Everything else a module needs is one macro each, declared next to the model and verified by the
build: `register_acls!` (access), `register_action!` (state transitions + numbering),
`register_compute!`, `register_constraint!` (structured per-field 400s), `register_service!`
(cross-record work, one transaction), `register_job!` (retries on Postgres, no broker),
`register_route!` (webhooks with constant-time HMAC), `register_seed!`,
`register_migration!` (the upgrade contract: per-step, resumable, downgrades refused),
`register_mailed!` (chatter).

## Workspace

| Path | What |
|---|---|
| `crates/kigumi-core` | introspectable metamodel, domain AST, security (ACLs + record rules + sudo), registries |
| `crates/kigumi-macros` | `#[model]` / `#[extend]` proc-macros |
| `crates/kigumi-schema` | projections: Postgres DDL, JSON UI contract, OpenAPI 3.1 |
| `crates/kigumi-db` | security-enforced persistence (sqlx), services, jobs, module lifecycle |
| `crates/kigumi-server` | headless axum server: CRUD, actions, services, module routes, SSE |
| `crates/kigumi-auth` / `-config` / `-storage` | JWT auth · typed config · content-addressed blobs |
| `crates/kigumi-runtime` | adopter wiring: migrate, admin bootstrap, workers, serve — four calls |
| `crates/kigumi-mcp` | the AI surface: MCP server derived from the catalog, tools under the user's ACLs |
| `crates/kigumi-test` | test kit: fingerprinted database reset (seconds, not minutes) |
| `modules/` | the stdlib (base, mail) and the **optional** ERP layer (sales, account, stock) |
| `apps/kigumi-cli` | the operational binary: `kigumi serve · migrate · new · mcp · module · user` |

The ERP is a layer you can lift off: `cargo build --no-default-features` leaves the bare frame —
a headless metamodel engine with security, services and events, ready for your own vertical.

## Development

```sh
cargo build --workspace
DATABASE_URL=postgres://localhost/kigumi_test KIGUMI_TEST_ALLOW_RESET=1 cargo test --workspace
```

Contributors' agents: see [AGENTS.md](AGENTS.md). Guides (EN/IT) live in `docs/guida/` and are
rendered at the site.

## License

MIT or Apache-2.0, at your option.
