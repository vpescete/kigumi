# Kigumi framework — agent guide (contributors)

This is the FRAMEWORK repository (building an app on Kigumi? `kigumi new` scaffolds a workspace
with its own agent guide). Kigumi is a headless, schema-driven business-app framework in Rust:
one declarative model generates schema, REST API, UI contract, security — and the MCP surface.

## Layout

- `crates/` — the framework: `kigumi-core` (metamodel, security, registries), `kigumi-macros`
  (`#[model]`/`#[extend]`), `kigumi-schema` (DDL/OpenAPI/UI-contract projections), `kigumi-db`
  (secured persistence, services, jobs, module lifecycle), `kigumi-server` (axum, SSE),
  `kigumi-auth`, `kigumi-config`, `kigumi-storage`, `kigumi-test` (fingerprinted DB reset),
  `kigumi-runtime` (adopter wiring), `kigumi-mcp` (MCP projection), `kigumi` (facade + macros).
- `modules/` — the stdlib (base, mail) and the optional ERP layer (sales, account, stock).
- `apps/kigumi-cli` — the operational binary (`kigumi serve|migrate|new|mcp|module|user|...`).
- `docs/guida/{en,it}` — the guides (the site renders them; keep both languages in sync).

## Commands

```sh
cargo build --workspace
DATABASE_URL=postgres://localhost/kigumi_test KIGUMI_TEST_ALLOW_RESET=1 cargo test -p <crate> --test <name>
```

DB tests use `kigumi-test`: fingerprinted reset (TRUNCATE fast path), advisory-locked, safe to
run binaries in sequence. `KIGUMI_TEST_ALLOW_RESET=1` is required on first contact with a DB.

## Rules that are enforced here

- ALL code is English: comments, identifiers, strings. (Conversation and docs/guida/it are not code.)
- Core names ZERO business models: ERP knowledge lives in `modules/*` behind the seams
  (`register_service!`, `register_write_trigger!`, ...). A grep-guard test fails the build otherwise.
- New module-facing capability = a new `register_*!` seam following the existing idiom
  (inventory registration + generic dispatch), never a hardcoded hook in core or CLI.
- Every elevation is explicit: `ctx.sudo()` after its permission gate — greppable.
- Commits are standalone (no chained shell commands in hooks' way) with imperative subjects.
- Public-surface changes (server routes, MCP tools, auth) get an adversarial review before push.

## Gotchas

- `register_acls!` takes a slice: `register_acls!(&ACLS)`.
- Auth endpoints live at `/auth/*`, not `/api/auth/*`.
- Sequences: module-declared via `register_sequence!`; migrate ensures them, counters survive.
- Uninstall FLAGS the module ledger row (data kept); re-install replays pending data migrations.
- `Domain` is not serde: parse filters with `Domain::from_json` and validate against the model.
