---
name: kigumi
description: Recipes for building Kigumi application modules — models, actions with numbering, computed fields, validation, services, background jobs, webhook routes, seeds and data migrations. Use when adding or changing features in a Kigumi app.
---

# Kigumi module recipes

Everything below goes in the application module's `src/lib.rs` (or a submodule). After model or
seam changes: `cargo build` (composition is verified by the compiler), then re-run migrate.
Namespace every global name (computes, jobs, sequence codes) with the module name.

## Add a model

```rust
#[model(name = "myshop.order", table = "myshop_order")]
pub struct Order {
    #[field(label = "Number")]
    name: Text,
    #[field(label = "Customer", target = "res.partner", required)]
    partner_id: Many2one,
    #[field(label = "State", default = "draft", selection = "draft:Draft,open:Open,done:Done")]
    state: Selection,
    #[field(label = "Lines", target = "myshop.order.line", inverse = "order_id")]
    line_ids: One2many,
}
kigumi::register_mailed!("myshop.order"); // optional: chatter thread

static ACLS: [Acl; 2] = [
    Acl { model: "myshop.order", group: "myshop.user", read: true, write: true, create: true, delete: false },
    Acl { model: "myshop.order", group: "admin", read: true, write: true, create: true, delete: true },
];
kigumi::register_acls!(&ACLS);
```

Without an ACL the model is default-deny for everyone but the superuser. Re-run migrate: the
table is created (and new fields on existing models are added additively).

## State transition + document numbering

```rust
kigumi::register_sequence!("myshop", "ORD", "ORD/", "", 5); // once per code, next to the action

fn open_order(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "draft" => Ok(ActionOutcome::new()
            .set("state", Value::Str("open".to_string()))
            .assign_sequence("name", "ORD")),
        s => Err(format!("can only open a draft order (state is '{s}')")),
    }
}
kigumi::register_action!("myshop.order", "open", open_order, &["myshop.user"]);
```

Run via `POST /api/myshop.order/:id/action/open`. The guard lives in the body; `Err` becomes the
caller's 400 message.

## Computed fields (stored, cascading)

```rust
// On the line: #[field(label = "Subtotal", compute = "myshop_line_subtotal", depends = "qty,price", store)]
fn line_subtotal(i: &ComputeInput) -> Value {
    Value::Decimal(i.decimal("qty") * i.decimal("price"))
}
kigumi::register_compute!("myshop_line_subtotal", line_subtotal);

// On the order: #[field(label = "Total", compute = "myshop_order_total", depends = "line_ids.subtotal", store)]
fn order_total(i: &ComputeInput) -> Value {
    Value::Decimal(i.sum_decimal("line_ids", "subtotal"))
}
kigumi::register_compute!("myshop_order_total", order_total);
```

## Validation with per-field errors

```rust
fn check_amounts(i: &ComputeInput) -> Result<(), String> {
    if i.decimal("qty") < rust_decimal::Decimal::ZERO {
        return Err("quantity cannot be negative".to_string());
    }
    Ok(())
}
kigumi::register_constraint!("myshop.order.line", &["qty"], check_amounts);
```

Violations return `{"error":{"code":"invalid","fields":{"qty":["..."]}}}` (HTTP 400).

## Cross-record service (one transaction) + background job

```rust
pub async fn complete(cx: &mut ServiceCtx<'_, '_>, input: ServiceInput) -> Result<ServiceOutput, DbError> {
    let order_model = cx.resolve("myshop.order")?;
    let ctx = cx.caller().clone();
    let order = cx.find_one_secured(&order_model, &ctx, input.record_id).await?
        .ok_or_else(|| DbError::BadInput("order not found or not permitted".to_string()))?;
    if order.get("state").and_then(|v| v.as_str()) != Some("open") {
        return Err(DbError::BadInput("can only complete an open order".to_string()));
    }
    let patch = serde_json::json!({ "state": "done" });
    cx.update_secured(&order_model, &ctx, input.record_id, patch.as_object().unwrap()).await?;
    cx.enqueue_job("myshop_notify", serde_json::json!({ "order_id": input.record_id })).await?;
    Ok(ServiceOutput::json(serde_json::json!({ "done": true })))
}
kigumi::register_service!("myshop.order", "complete", complete, true, &["myshop.user"]);

pub async fn notify_job(db: &Db, payload: serde_json::Value) -> Result<(), DbError> {
    // IDEMPOTENT (at-least-once). Use a su ctx for system effects: Ctx::new(0, vec![]).sudo()
    Ok(())
}
kigumi::register_job!("myshop_notify", 5, notify_job);
```

The service owns ONE transaction (commit on Ok, rollback on Err — including the enqueued job).
Where a system effect must exceed the caller's rights: gate first, then `let elevated = ctx.sudo();`.

## Webhook route (unauthenticated, HMAC-verified)

```rust
pub async fn supplier_hook(db: &Db, input: RouteInput) -> Result<RouteOutput, DbError> {
    let secret = std::env::var("MYSHOP_WEBHOOK_SECRET").unwrap_or_default();
    let sig = input.headers.get("x-signature").cloned().unwrap_or_default();
    if secret.is_empty() || !input.verify_hmac_sha256(secret.as_bytes(), &sig) {
        return Err(DbError::AccessDenied { model: "myshop.order".to_string(), operation: "create" });
    }
    let su = input.ctx.clone().sudo(); // sender verified: explicit elevation
    /* db.insert_secured(&model, &su, &[], &[], values) ... */
    Ok(RouteOutput::Json(serde_json::json!({ "ok": true })))
}
kigumi::register_route!("supplier-hook", Post, false, &[], supplier_hook);
```

Served at `POST /api/x/supplier-hook`. `auth: false` = guest ctx (default-deny until you verify
and elevate). Never compare signatures with a hand-rolled hash and `==`.

## Ship a data migration (upgrade contract)

1. Change the model (new field, new semantics). 2. Bump `version` in `MANIFEST`. 3. Register the step:

```rust
pub async fn backfill(db: &Db) -> Result<(), DbError> { /* IDEMPOTENT backfill */ Ok(()) }
kigumi::register_migration!("myshop", "1.1.0", backfill);
```

Migrate applies pending steps in semver order, records progress per step, resumes after a
failure, refuses downgrades. A fresh install replays nothing.

## Seed reference data

```rust
pub async fn seed(db: &Db) -> Result<(), DbError> {
    // Runs at EVERY migrate while installed: guard with exists-checks, never overwrite operators.
    Ok(())
}
kigumi::register_seed!("myshop", seed);
```

## Verify from outside

```sh
TOKEN=$(curl -s -X POST localhost:8600/auth/login -H 'content-type: application/json' \
  -d '{"login":"admin","password":"..."}' | jq -r .access_token)   # /auth/*, NOT /api/auth/*
curl -s localhost:8600/api/myshop.order/view -H "Authorization: Bearer $TOKEN"   # UI contract
curl -sN localhost:8600/api/events/stream -H "Authorization: Bearer $TOKEN"      # live SSE
```

`cargo run -p app -- mcp <login>` serves the app over MCP (stdio) for AI agents, with the
user's ACLs enforced on every tool.
