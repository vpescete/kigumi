//! Sales cross-record services — module-owned multi-record operations registered on the framework's
//! `register_service!` seam (the run_action twin). These used to live as hardcoded methods on `Db` in the
//! framework crate; relocating them here is what makes the ERP an OPTIONAL layer: meshble-db no longer
//! names `sale.order`. The model-name literals belong here, in the module that owns them.

use meshble::prelude::*; // ServiceCtx, ServiceInput, ServiceOutput, DbError, Domain, Operation
use rust_decimal::Decimal;
use serde_json::json;

/// Applies the discount wizard's percentage to every line of its order (Odoo's `sale.order.discount`
/// apply). Invoked on the wizard id via `POST /api/sale.order.discount/:id/service/apply_discount`.
/// Behavior-preserving relocation of the former `Db::apply_sale_order_discount`: gates on the order's
/// Write access (the wizard the route named is only a transient scratchpad), validates the percent at the
/// boundary, and writes each line's `discount` through the SECURED path so the line/order totals cascade.
pub async fn apply_discount(cx: &mut ServiceCtx<'_>, input: ServiceInput) -> Result<ServiceOutput, DbError> {
    let wizard_model = cx.resolve("sale.order.discount")?;
    let order_model = cx.resolve("sale.order")?;
    let line_model = cx.resolve("sale.order.line")?;
    let ctx = cx.caller().clone();

    // The real authorization is "may write the order" — the route only named the (user-owned) wizard.
    if !cx.check_access(Operation::Write, order_model.name) {
        return Err(DbError::AccessDenied { model: order_model.name.to_string(), operation: "apply_discount" });
    }

    let wizard = cx
        .find_one_secured(&wizard_model, &ctx, input.record_id)
        .await?
        .ok_or_else(|| DbError::BadInput("discount wizard not found or not permitted".to_string()))?;
    let order_id = wizard
        .get("order_id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| DbError::BadInput("the discount wizard has no order".to_string()))?;
    let discount: Decimal = wizard
        .get("discount")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or_default();
    // Validate at the boundary: a percent must be in [0, 100] (the line net factor is 1 - d/100).
    if discount < Decimal::ZERO || discount > Decimal::from(100) {
        return Err(DbError::BadInput("discount must be a percentage between 0 and 100".to_string()));
    }

    // The order must be visible/permitted to the caller.
    cx.find_one_secured(&order_model, &ctx, order_id)
        .await?
        .ok_or_else(|| DbError::BadInput("order not found or not permitted".to_string()))?;

    let lines = cx
        .find_secured(&line_model, &ctx, Some(&Domain::field("order_id").eq(order_id)))
        .await?;
    let mut applied = 0u64;
    for line in &lines {
        let Some(lid) = line.get("id").and_then(|v| v.as_i64()) else { continue };
        let payload = json!({ "discount": discount.to_string() });
        cx.update_secured(&line_model, &ctx, lid, payload.as_object().unwrap()).await?;
        applied += 1;
    }
    Ok(ServiceOutput::json(json!({ "applied": applied })))
}
