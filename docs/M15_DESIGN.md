# M15 — Report + Wizard + Sale/Purchase completion + Variant pricing

Design from the M15 multi-agent design pass (4 area analyses + synthesis), with owner decisions taken.

## Unifying insight

Three of the four areas need work only because of ONE framework gap: **there is no aggregate over
Many2many** (stored aggregates run over One2many via `sum_decimal`; `compute_on_read` is same-record;
`parents_of` walks only One2many inverse FKs). The plan deliberately **avoids widening that gap**:

- Variant `price_extra` is **materialized** as a stored `Decimal` at the two write boundaries (the M2M
  analogue of `recompute_columns_on`), never aggregated on read.
- Line tax uses a **single `Many2one tax_id`** (not Odoo's M2M `tax_ids`), so per-line tax is a
  same-record stored compute (reading the related `tax_rate` via the supported single-hop
  `related_subquery`) and order totals are existing One2many aggregates.
- Reports read whatever stored/computed fields exist; they never recompute pricing.

## Owner decisions (taken)

- **PRICING-MODEL** → `price_extra` on the PTAV row (manager partial-write; the 3 structural FKs locked
  **both** by D6 field-groups **and** an `@api.constrains` rejecting non-sudo structural changes),
  materialized to a stored `product.product.price_extra`; `lst_price` = `list_price + price_extra`
  (same-record on-read compute). PLUS a **flat pricelist** subset (fixed + percentage; base
  list/standard; `applied_on` global/category/product/variant; `min_quantity`; date window;
  **same-currency only**, enforced by constrains — no FX).
- **D15 (tax)** → land a **minimal `account.tax`** now (name, type_tax_use, amount_type, amount,
  company_id, active) in the sales crate, adopted later by the full account module via `#[extend]`
  (same name, no migration). **Single `tax_id` per line**, round-per-line, `price_include` deferred.
  Invoicing is a **status-flag + `create_invoice` seam** (flips `invoice_status` + posts chatter; no
  `account.move`).
- **D11 (report)** → **HTML-first**; PDF optional behind `Optional<Arc<dyn Rasterizer>>` in config,
  **typst** as the pure-Rust primary engine (single-binary preserved), chromiumoxide available behind
  the same trait. HTML and PDF are two render targets sharing data, not one markup string.
- **WIZARD** → **build the full generic wizard/TransientModel subsystem now** (owner choice, ≠ the
  design's "defer" recommendation): `register_wizard!` + a transient model surface (Odoo-faithful:
  `default_get` seeding + a GC vacuum cron). The action create-DSL generalization is folded in.

## Ordering (sub-milestones)

1. **M15.1 — Variant pricing core** (FIRST): P1 PTAV `price_extra` + dual-locked partial write · P2
   materialize variant `price_extra` in `generate_variants` · P3 `lst_price` same-record on-read compute
   · P4 PTAV-write refresh hook in `update_secured`.
2. **M15.2 — Pricelist** model + `resolve_price` engine (selectivity + category-ancestor walk, capped) +
   sale.order `pricelist_id` + `apply_pricelist` action (writes `price_unit`, base = `lst_price`).
3. **M15.3 — account.tax + line tax/discount + order amount split + purchase.order + invoicing seam.**
4. **M15.4 — Wizard subsystem** (full): `register_wizard!` + transient model + `default_get` + GC cron;
   `sale.order.discount` as the first wizard. Concrete design:
   - **Transient registry** — `register_transient!("model")` → `TransientRegistration{model}` in core
     (`is_transient`/`transient_models()`, mirroring `is_mailed`). A transient model is a normal model
     (own table, served, secured) whose rows are ephemeral.
   - **GC timestamp via DB default** — `to_ddl` emits no column `DEFAULT`, so migration adds one: after
     creating a transient model's table the CLI runs `ALTER TABLE <t> ALTER COLUMN create_date SET
     DEFAULT now()`. Postgres then stamps `create_date` on **every** insert path (open endpoint, generic
     POST, anything) — robust, zero hot-path. Transient models declare a nullable `create_date Datetime`.
   - **GC cron** `gc_transient_records` (hourly): for each `transient_models()`, resolve its table and
     `DELETE … WHERE create_date < now() - interval '1 hour'`; tolerate an unmigrated table (42P01).
   - **Open + `default_get`** — `register_wizard!(model, default_get)` where `default_get: fn(&WizardContext)
     -> Vec<(&'static str, Value)>` (pure, no DB in v1; DB-backed defaults are a later extension).
     `WizardContext { active_model, active_id, active_ids }`. Endpoint `POST /api/:name/open` body
     `{active_model?, active_id?, active_ids?}` → `default_get(ctx)` seeds → `insert_secured` under the
     caller → returns the created record for the FE to contract-render.
   - **Apply** — per-wizard service method + endpoint (the Odoo-faithful "button method"; the framework
     does NOT generalize apply, exactly as `generate_variants`/`apply_pricelist` are dedicated). First
     wizard `sale.order.discount` (fields `order_id` req, `discount` Decimal, `create_date`): `Db::
     apply_sale_order_discount(id)` gates on `sale.order` WRITE, reads the transient, writes `discount`
     onto every line of `order_id` under the caller ctx (line computes + order amounts cascade); endpoint
     `POST /api/:name/:id/apply_discount` (pinned to `sale.order.discount`). ACL: `sales.user`
     read/write/create, no delete (GC reclaims).
   - **Slices**: (1) transient registry + migration default + GC cron; (2) `register_wizard!` + open
     endpoint + `default_get` + `sale.order.discount` model; (3) `apply_sale_order_discount` + endpoint.
5. **M15.5 — Report engine** (LAST): registration primitive (render fn, like `register_action!`) +
   secured HTML endpoint (`GET /api/:name/:id/report/:report`, secured entirely by `find_one_secured`)
   + sale.order quotation template + Rasterizer trait + typst PDF (501 when None) + content-addressed
   `ir.attachment` cache + contract `reports` array.
   - **Slices**: (1) `register_report!` + HTML endpoint + quotation template (HTML-escaped) + contract
     `reports` array; (2) `Rasterizer` trait + `router_with_data_rasterized` + `?format=pdf` (501 when
     None, rasterize + `application/pdf` when Some).
   - **Shipped**: slices 1 + 2 — HTML reports end-to-end and the PDF rasterization seam.
   - **Deferred (explicit, not silent)**: a concrete `Rasterizer` impl (typst/headless-Chromium) — the
     trait is the pluggable seam, production answers PDF with 501 until one is attached; and the
     content-addressed `ir.attachment` PDF cache — re-renders per request for now, since the dedup only
     pays for itself behind a slow real rasterizer (noted with a `ponytail:` comment at the call site).

## Top risks (carried into implementation)

1. **`price_extra` materialization staleness** — only `generate_variants` + the PTAV-write hook refresh
   the stored variant `price_extra`; any other write path leaves it stale and silently mis-prices.
   Mitigations: route all PTAV writes through the secured path; a manual `refresh_prices` action; an
   invariant test `variant.price_extra == SUM(linked ptav.price_extra)`.
2. **PTAV partial-write escalation** — opening WRITE on PTAV needs the **dual** lock (D6 field-groups
   AND constrains) on the 3 structural FKs; a manager must not split/corrupt a combo. Non-negotiable test.
3. **Action/wizard creates must NOT elevate** — user-intent creates go through `insert_secured_in_tx`
   under the CALLER's ctx (ACL/rules/company/D6 per record). Create-only DSL (no nested update/delete).
4. **`resolve_price` base = `lst_price`** (not the delegated `list_price`), else variant surcharges
   vanish. Cap+dedupe the category `parent_id` ancestor walk (cycle guard).
5. **Tax recompute on `tax_id` change** — changing a line's `tax_id` must recompute its stored
   `price_tax` AND `order.amount_tax`. Single-currency enforced only by the pricelist constrains; keep a
   `resolve_price(..., target_currency)` seam for later FX.
6. **Report fidelity** — `find_one_secured` returns Many2one as a raw id, so a template needs resolved
   display fields (partner name, currency symbol); filename → `header_safe`; rasterize on `spawn_blocking`.
