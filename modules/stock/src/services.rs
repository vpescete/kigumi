//! Stock cross-record services — the reservation / validation engine, relocated from meshble-db onto the
//! framework's `register_service!` seam so the ERP becomes an optional layer (the core no longer names
//! stock.picking). Unlike the other relocated services these are genuinely TRANSACTIONAL: they run FOR
//! UPDATE quant/picking locking + the quant math as raw SQL on the SERVICE transaction (`cx.tx()`), which
//! run_service opens and commits. This is the sole consumer of the ServiceCtx v2 tx() surface.

use meshble::prelude::*; // ServiceCtx, ServiceInput, ServiceOutput, DbError
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::Row;

/// The uom.uom conversion factor for `uom_id` (units of the reference unit per 1 of this unit), rounded to
/// 6 dp; absent/unreadable/non-positive => 1 (pass-through). Read on the SERVICE tx so it stays consistent
/// with the FOR UPDATE reads in the same transaction. Relocated from `Db::uom_factor`.
async fn uom_factor(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, uom_id: Option<i64>) -> Result<Decimal, DbError> {
    let Some(uid) = uom_id else { return Ok(Decimal::ONE) };
    let f: Option<f64> = sqlx::query_scalar("SELECT factor FROM uom_uom WHERE id = $1")
        .bind(uid)
        .fetch_optional(&mut **tx)
        .await?
        .flatten();
    match f.and_then(Decimal::from_f64_retain) {
        Some(d) if d > Decimal::ZERO => Ok(d.round_dp(6)),
        _ => Ok(Decimal::ONE),
    }
}

/// Reserves a draft transfer: for each internal-source move, lock the (product, location, lot) quant FOR
/// UPDATE and grant up to the free (on-hand − reserved) quantity, in the product reference unit. Invoked on
/// the picking id via `POST /api/stock.picking/:id/service/reserve`. Relocated from `Db::reserve_picking`;
/// run_service gated Write + visibility on the picking. Returns the number of moves reserved.
pub async fn reserve(cx: &mut ServiceCtx<'_, '_>, input: ServiceInput) -> Result<ServiceOutput, DbError> {
    let picking_model = cx.resolve("stock.picking")?;
    let ctx = cx.caller().clone();
    let picking_id = input.record_id;
    let picking = cx
        .find_one_secured(&picking_model, &ctx, picking_id)
        .await?
        .ok_or_else(|| DbError::BadInput("transfer not found or not permitted".to_string()))?;
    let state = picking.get("state").and_then(|v| v.as_str()).unwrap_or("");
    if state != "draft" {
        return Err(DbError::BadInput(format!("only a draft transfer can be reserved (state is '{state}')")));
    }

    let mut reserved_moves = 0i64;
    {
        let tx = cx.tx();
        let moves = sqlx::query("SELECT id, product_id, product_uom_qty, product_uom_id, lot_id, reserved_qty, location_id FROM stock_move WHERE picking_id = $1 AND state = 'draft'")
            .bind(picking_id)
            .fetch_all(&mut **tx)
            .await?;
        for m in &moves {
            let move_id: i64 = m.try_get("id")?;
            let product_id: i64 = m.try_get("product_id")?;
            let ordered: Decimal = m.try_get("product_uom_qty")?;
            let already: Decimal = m.try_get("reserved_qty")?;
            let lot_id: Option<i64> = m.try_get("lot_id")?;
            let src: i64 = m.try_get("location_id")?;
            let factor = uom_factor(tx, m.try_get::<Option<i64>, _>("product_uom_id")?).await?;
            let ordered_ref = (ordered * factor).round_dp(6);
            // Only internal sources hold reservable stock (a supplier/customer source has none).
            let src_usage: Option<String> = sqlx::query_scalar("SELECT usage FROM stock_location WHERE id = $1")
                .bind(src)
                .fetch_optional(&mut **tx)
                .await?;
            if src_usage.as_deref() != Some("internal") {
                continue;
            }
            let want = ordered_ref - already;
            if want <= Decimal::ZERO {
                continue;
            }
            // Lock the quant row: a concurrent reserve of the same quant blocks here.
            let row = sqlx::query("SELECT quantity, COALESCE(reserved_quantity, 0) AS reserved_quantity FROM stock_quant WHERE product_id = $1 AND location_id = $2 AND COALESCE(lot_id, 0) = COALESCE($3, 0) FOR UPDATE")
                .bind(product_id)
                .bind(src)
                .bind(lot_id)
                .fetch_optional(&mut **tx)
                .await?;
            let Some(row) = row else { continue };
            let on_hand: Decimal = row.try_get("quantity")?;
            let reserved: Decimal = row.try_get("reserved_quantity")?;
            let free = on_hand - reserved;
            let grant = if want < free { want } else { free };
            if grant <= Decimal::ZERO {
                continue;
            }
            sqlx::query("UPDATE stock_quant SET reserved_quantity = reserved_quantity + $4 WHERE product_id = $1 AND location_id = $2 AND COALESCE(lot_id, 0) = COALESCE($3, 0)")
                .bind(product_id)
                .bind(src)
                .bind(lot_id)
                .bind(grant)
                .execute(&mut **tx)
                .await?;
            sqlx::query("UPDATE stock_move SET reserved_qty = reserved_qty + $2 WHERE id = $1")
                .bind(move_id)
                .bind(grant)
                .execute(&mut **tx)
                .await?;
            reserved_moves += 1;
        }
    }
    Ok(ServiceOutput::json(json!({ "reserved": reserved_moves })))
}

/// Validates a draft transfer (`POST /api/stock.picking/:id/service/validate`): a FOR UPDATE compare-and-set
/// on the picking state, then per move moves the (reference-unit) quantity between the source and
/// destination quants (serial guard, over-delivery clamp against available), flips the moves + picking to
/// done, re-materializes on-hand, and emits stock.picking.done — all in one transaction. Any unfulfilled
/// remainder becomes a DRAFT backorder created POST-COMMIT (so a backorder failure leaves the validation
/// durable). Relocated from `Db::validate_picking`. Returns the assigned transfer number.
pub async fn validate(cx: &mut ServiceCtx<'_, '_>, input: ServiceInput) -> Result<ServiceOutput, DbError> {
    let picking_model = cx.resolve("stock.picking")?;
    let ctx = cx.caller().clone();
    let picking_id = input.record_id;
    let picking = cx
        .find_one_secured(&picking_model, &ctx, picking_id)
        .await?
        .ok_or_else(|| DbError::BadInput("transfer not found or not permitted".to_string()))?;
    let state = picking.get("state").and_then(|v| v.as_str()).unwrap_or("");
    if state != "draft" {
        return Err(DbError::BadInput(format!("only a draft transfer can be validated (state is '{state}')")));
    }
    let seq = match picking.get("picking_type").and_then(|v| v.as_str()).unwrap_or("internal") {
        "receipt" => "IN",
        "delivery" => "OUT",
        _ => "INT",
    };
    cx.ensure_sequence(seq, &format!("{seq}/"), "", 5).await?;
    let number = cx.next_value(seq).await?;

    // (product, remainder in move unit, src, dst, move uom, lot) for each partially-processed move → a
    // backorder after commit.
    let mut backorders: Vec<(i64, Decimal, i64, i64, Option<i64>, Option<i64>)> = Vec::new();
    let mut products: Vec<i64> = Vec::new();
    {
        let tx = cx.tx();
        // Compare-and-set: lock the row and re-assert draft, so concurrent validations can't double-apply.
        let live: Option<String> = sqlx::query_scalar("SELECT state FROM stock_picking WHERE id = $1 FOR UPDATE")
            .bind(picking_id)
            .fetch_optional(&mut **tx)
            .await?;
        if live.as_deref() != Some("draft") {
            return Err(DbError::Conflict("the transfer was already validated".to_string()));
        }
        let moves = sqlx::query("SELECT id, product_id, product_uom_qty, product_uom_id, lot_id, quantity_done, reserved_qty, location_id, location_dest_id FROM stock_move WHERE picking_id = $1 AND state = 'draft'")
            .bind(picking_id)
            .fetch_all(&mut **tx)
            .await?;
        if moves.is_empty() {
            return Err(DbError::BadInput("cannot validate a transfer with no moves".to_string()));
        }
        for m in &moves {
            let move_id: i64 = m.try_get("id")?;
            let product_id: i64 = m.try_get("product_id")?;
            let ordered: Decimal = m.try_get("product_uom_qty")?;
            let done_field: Decimal = m.try_get("quantity_done")?;
            let move_reserved: Decimal = m.try_get("reserved_qty")?;
            let uom_id: Option<i64> = m.try_get("product_uom_id")?;
            let lot_id: Option<i64> = m.try_get("lot_id")?;
            let src: i64 = m.try_get("location_id")?;
            let dst: i64 = m.try_get("location_dest_id")?;
            // quantity_done == 0 means "the full ordered quantity"; a positive value validates that much and
            // backorders the rest. All quant math is in the product REFERENCE unit.
            let factor = uom_factor(tx, uom_id).await?;
            let done = if done_field > Decimal::ZERO { done_field } else { ordered };
            let mut done_ref = (done * factor).round_dp(6);
            // Serial-tracked product: exactly one unit, must carry its lot.
            let tracking: Option<String> = sqlx::query_scalar::<_, Option<String>>(
                "SELECT t.tracking FROM product_product p JOIN product_template t ON p.product_tmpl_id = t.id WHERE p.id = $1",
            )
            .bind(product_id)
            .fetch_optional(&mut **tx)
            .await?
            .flatten();
            if tracking.as_deref() == Some("serial") {
                if lot_id.is_none() {
                    return Err(DbError::BadInput("a serial-tracked move requires a serial number (lot_id)".to_string()));
                }
                if done_ref != Decimal::ONE {
                    return Err(DbError::BadInput("a serial number is exactly one unit; the move quantity must be 1".to_string()));
                }
            }
            // Over-delivery guard: an INTERNAL source can take only what is available (on-hand − other
            // reservations + this move's own reservation). Stock never goes negative.
            let src_usage: Option<String> = sqlx::query_scalar("SELECT usage FROM stock_location WHERE id = $1")
                .bind(src)
                .fetch_optional(&mut **tx)
                .await?;
            if src_usage.as_deref() == Some("internal") {
                let row = sqlx::query("SELECT quantity, COALESCE(reserved_quantity, 0) AS reserved_quantity FROM stock_quant WHERE product_id = $1 AND location_id = $2 AND COALESCE(lot_id, 0) = COALESCE($3, 0)")
                    .bind(product_id)
                    .bind(src)
                    .bind(lot_id)
                    .fetch_optional(&mut **tx)
                    .await?;
                let (on_hand, reserved) = match row {
                    Some(r) => (r.try_get::<Decimal, _>("quantity")?, r.try_get::<Decimal, _>("reserved_quantity")?),
                    None => (Decimal::ZERO, Decimal::ZERO),
                };
                let available = on_hand - reserved + move_reserved;
                if done_ref > available {
                    done_ref = available;
                }
                if done_ref < Decimal::ZERO {
                    done_ref = Decimal::ZERO;
                }
            }
            let done = (done_ref / factor).round_dp(6);
            // Source loses done_ref + frees this move's reservation in full; destination gains done_ref.
            if done_ref > Decimal::ZERO {
                sqlx::query(
                    "INSERT INTO stock_quant (product_id, location_id, quantity, reserved_quantity, lot_id) VALUES ($1, $2, $3, 0, $5) \
                     ON CONFLICT (product_id, location_id, COALESCE(lot_id, 0)) DO UPDATE SET \
                       quantity = stock_quant.quantity + $3, \
                       reserved_quantity = GREATEST(0, stock_quant.reserved_quantity - $4)",
                )
                .bind(product_id)
                .bind(src)
                .bind(-done_ref)
                .bind(move_reserved)
                .bind(lot_id)
                .execute(&mut **tx)
                .await?;
                sqlx::query(
                    "INSERT INTO stock_quant (product_id, location_id, quantity, reserved_quantity, lot_id) VALUES ($1, $2, $3, 0, $4) \
                     ON CONFLICT (product_id, location_id, COALESCE(lot_id, 0)) DO UPDATE SET quantity = stock_quant.quantity + $3",
                )
                .bind(product_id)
                .bind(dst)
                .bind(done_ref)
                .bind(lot_id)
                .execute(&mut **tx)
                .await?;
            }
            sqlx::query("UPDATE stock_move SET state = 'done', quantity_done = $2 WHERE id = $1")
                .bind(move_id)
                .bind(done)
                .execute(&mut **tx)
                .await?;
            let remainder = ordered - done;
            if remainder > Decimal::ZERO {
                backorders.push((product_id, remainder, src, dst, uom_id, lot_id));
            }
            products.push(product_id);
        }
        products.sort_unstable();
        products.dedup();

        sqlx::query("UPDATE stock_picking SET state = 'done', name = $2 WHERE id = $1")
            .bind(picking_id)
            .bind(&number)
            .execute(&mut **tx)
            .await?;

        // Re-materialize on-hand for the moved products from the (just-updated) internal quants.
        if !products.is_empty() {
            sqlx::query(
                "UPDATE product_product p SET qty_available = COALESCE( \
                   (SELECT SUM(q.quantity) FROM stock_quant q JOIN stock_location l ON l.id = q.location_id \
                    WHERE q.product_id = p.id AND l.usage = 'internal'), 0) \
                 WHERE p.id = ANY($1)",
            )
            .bind(&products)
            .execute(&mut **tx)
            .await?;
        }
    }

    // Domain event, atomic with the transfer (same service tx): the picking is validated/done.
    let company_id = picking.get("company_id").and_then(|v| v.as_i64());
    cx.emit_event("stock.picking.done", "stock.picking", picking_id, company_id, json!({ "name": number })).await?;

    // Spill any unfulfilled remainder into a new DRAFT backorder — created POST-COMMIT (defer_insert), so a
    // backorder failure leaves the original transfer validated (documented non-atomicity).
    if !backorders.is_empty() {
        let elevated = cx.elevated();
        let ptype = picking.get("picking_type").and_then(|v| v.as_str()).unwrap_or("internal");
        let ploc = picking.get("location_id").and_then(|v| v.as_i64());
        let pdest = picking.get("location_dest_id").and_then(|v| v.as_i64());
        let bo_moves: Vec<serde_json::Value> = backorders
            .iter()
            .map(|(product_id, remainder, src, dst, uom_id, lot_id)| {
                json!({
                    "product_id": product_id, "product_uom_qty": remainder.to_string(),
                    "product_uom_id": uom_id, "lot_id": lot_id, "location_id": src, "location_dest_id": dst
                })
            })
            .collect();
        let bo_payload = json!({
            "picking_type": ptype, "location_id": ploc, "location_dest_id": pdest,
            "backorder_id": picking_id, "move_ids": bo_moves
        });
        cx.defer_insert(picking_model, elevated, bo_payload.as_object().unwrap().clone());
    }

    Ok(ServiceOutput::json(json!({ "validated": number })))
}
