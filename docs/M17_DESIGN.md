# M17 — Stock (inventory)

The last XL business module: it closes the Odoo-like chain (sales -> inventory -> purchase ->
accounting). New crate `modules/stock` (depends on base + sales for `product.product`). Reuses cron,
sequences, actions, the service-method + dedicated-endpoint pattern, and the secured CRUD machinery.

## The core mechanism (the crux, M17.2)

Stock is a ledger of movements. A `stock.move` in state `done` atomically changes `stock.quant` (the
materialized on-hand per product+location): decrement the source location, increment the destination.

```
validate_picking(picking):  -- one tx
  for each move (product p, qty q, src, dst):
    upsert stock_quant (p, src) quantity -= q      -- ON CONFLICT (product_id, location_id)
    upsert stock_quant (p, dst) quantity += q
    move.state = 'done'
  picking.state = 'done'
  recompute product.qty_available for each moved product  -- materialized = SUM quant at internal locs
```

This is the stock analogue of the balanced-entry constraint: the invariant is "a done move conserves
quantity (what leaves src arrives at dst)". v1 allows **negative stock** (Odoo's default) — no reservation
or availability blocking yet, so a delivery from empty stock drives the internal quant negative and the
supplier/customer virtual locations absorb the counter-flow. `qty_available` is **materialized** on
product.product (the compute engine can't aggregate over a separate table — same gap M15 hit), refreshed
at the move-done write boundary, like the variant `price_extra` materialization.

## Models

| Model | Table | Key fields | Notes |
|---|---|---|---|
| `stock.location` | `stock_location` | `name` (req), `usage` Selection (internal/supplier/customer/inventory/transit), `parent_id` M2o self, `company_id` | Hierarchical. The supplier/customer/inventory locations are VIRTUAL (infinite source/sink) — only `internal` counts as real on-hand. |
| `stock.warehouse` | `stock_warehouse` | `name` (req), `code` (req), `location_id` M2o stock.location (the main stock), `company_id` | v1: one main stock location per warehouse (no input/output/pack sub-locations). |
| `stock.quant` | `stock_quant` | `product_id` M2o product.product (req), `location_id` M2o stock.location (req), `quantity` Decimal | UNIQUE (product_id, location_id) via a migration index (`ensure_stock_indexes`). The on-hand source of truth. |
| `stock.picking` | `stock_picking` | `name` (def "/"), `picking_type` Selection (receipt/delivery/internal), `partner_id` M2o, `location_id`/`location_dest_id` M2o stock.location (req), `state` Selection (draft/done/cancel), `move_ids` O2m, `company_id` | A transfer document grouping moves. Mailed (chatter). Numbered from a per-type sequence (IN/OUT/INT) on validate. |
| `stock.move` | `stock_move` | `picking_id` M2o (req, inverse), `product_id` M2o (req), `product_uom_qty` Decimal, `location_id`/`location_dest_id` M2o (req), `state` Selection (draft/done) | One product movement. Defaults its locations from the picking. |

product.product (+field): `qty_available` Decimal (default 0, `groups="base.system"` — engine-maintained,
materialized; users read, only the move-done boundary writes it).

## Slices (each: implement -> adversarial review on the risky ones -> tests + live smoke -> commit + push)

1. **M17.1 — Locations + warehouse + quants (structural).** The four models above (location, warehouse,
   quant) + ACLs (`stock.user`/`stock.manager`) + a migrate seed of a default warehouse and the standard
   locations (Stock [internal], Vendors [supplier], Customers [customer], Inventory adjustment) for the
   default company + the `ensure_stock_indexes` composite-unique on quant. Tests: migrate, create a
   quant, the unique index rejects a duplicate (product, location).
2. **M17.2 — stock.picking + stock.move + the validate mechanism (the crux).** The two models + `Db::
   validate_picking(id)`: draft -> done, the atomic quant upserts (src -=, dst +=), per-type sequence
   numbering, and the `qty_available` materialization on each moved product. Posted-move-style
   immutability: a done picking's moves are frozen (record rule `picking_id.state != done`). Endpoint
   `POST /api/stock.picking/:id/validate`. **Adversarial review** (atomicity, negative stock, double-
   validate, materialization staleness, multi-company). Tests: a receipt validates and raises on-hand;
   a delivery lowers it; re-validate rejected.
3. **M17.3 — sale/purchase integration.** Service methods + endpoints: `create_delivery` on sale.order
   (a draft delivery picking Stock -> Customers, one move per goods line), `create_receipt` on
   purchase.order (Vendors -> Stock). Gated on the order WRITE, picking created elevated, like
   `create_invoice`. Buttons in the FE service-action registry. Tests + live smoke: confirm a sale ->
   create delivery -> validate -> on-hand drops; same for a purchase receipt raising it.

## Risks
- Quant atomicity / double-validate -> the validate runs in one tx and is guarded on state == draft; a
  done picking is frozen by a record rule (covers direct + nested move writes).
- `qty_available` staleness -> only the move-done boundary writes it; a manual recompute action is a
  fast-follow; never expose it as user-writable (base.system lock).
- Negative stock -> allowed in v1 (documented); reservation/availability is a later milestone.
- Company consistency -> a move's locations + product must share the picking's company (an @api.constrains
  on stock.picking, mirroring account.move's check_line_companies).
- product.product lives in `sales`, so `stock` depends on sales; the sale/purchase integration is a
  db-layer service method (resolve_registered by name), so no circular Cargo dependency (like invoicing).
