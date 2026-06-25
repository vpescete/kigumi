//! Sales cross-record services — module-owned multi-record operations registered on the framework's
//! `register_service!` seam (the run_action twin). These used to live as hardcoded methods on `Db` in the
//! framework crate; relocating them here is what makes the ERP an OPTIONAL layer: meshble-db no longer
//! names `sale.order`. The model-name literals belong here, in the module that owns them.

use meshble::prelude::*; // ServiceCtx, ServiceInput, ServiceOutput, DbError, Domain, Operation
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::Row;

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

/// Re-prices every line of a `sale.order` from its pricelist (Odoo's pricelist apply). Invoked on the
/// order id via `POST /api/sale.order/:id/service/apply_pricelist`. run_service has already gated Write +
/// visibility on the order. The pricelist currency must equal the order currency (no FX in v1).
/// Behavior-preserving relocation of the former `Db::apply_pricelist`.
pub async fn apply_pricelist(cx: &mut ServiceCtx<'_>, input: ServiceInput) -> Result<ServiceOutput, DbError> {
    let order_model = cx.resolve("sale.order")?;
    let line_model = cx.resolve("sale.order.line")?;
    let pricelist_model = cx.resolve("product.pricelist")?;
    let ctx = cx.caller().clone();
    let order_id = input.record_id;

    let order = cx
        .find_one_secured(&order_model, &ctx, order_id)
        .await?
        .ok_or_else(|| DbError::BadInput("order not found or not permitted".to_string()))?;
    let pricelist_id = order
        .get("pricelist_id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| DbError::BadInput("the order has no pricelist".to_string()))?;
    let order_currency = order.get("currency_id").and_then(|v| v.as_i64());

    let pl = cx
        .find_one_secured(&pricelist_model, &ctx, pricelist_id)
        .await?
        .ok_or_else(|| DbError::BadInput("pricelist not found or not permitted".to_string()))?;
    if pl.get("currency_id").and_then(|v| v.as_i64()) != order_currency {
        return Err(DbError::BadInput("pricelist currency does not match the order currency".to_string()));
    }

    let today = cx.today().await?;
    let lines = cx
        .find_secured(&line_model, &ctx, Some(&Domain::field("order_id").eq(order_id)))
        .await?;
    let mut priced = 0u64;
    for line in &lines {
        let (Some(lid), Some(product_id)) =
            (line.get("id").and_then(|v| v.as_i64()), line.get("product_id").and_then(|v| v.as_i64()))
        else {
            continue;
        };
        let qty: Decimal = line
            .get("product_uom_qty")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or_default();
        let price = resolve_price(cx.pool(), pricelist_id, product_id, qty, &today).await?;
        let payload = json!({ "price_unit": price.to_string() });
        cx.update_secured(&line_model, &ctx, lid, payload.as_object().unwrap()).await?;
        priced += 1;
    }
    Ok(ServiceOutput::json(json!({ "priced": priced })))
}

/// The values an order line should default when its product is set (name, effective unit price, qty 1,
/// uom). A read-only service on `product.product` — `POST /api/product.product/:id/service/line_defaults`,
/// returning `{values}` the client merges into the line. Behavior-preserving relocation of the former
/// `Db::product_onchange_values`; replaces the old model-level `/_onchange` endpoint.
pub async fn line_defaults(cx: &mut ServiceCtx<'_>, input: ServiceInput) -> Result<ServiceOutput, DbError> {
    let product_model = cx.resolve("product.product")?;
    let ctx = cx.caller().clone();
    let p = cx
        .find_one_secured(&product_model, &ctx, input.record_id)
        .await?
        .ok_or_else(|| DbError::BadInput("product not found or not permitted".to_string()))?;
    let dec = |k: &str| -> Decimal { p.get(k).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or_default() };
    let price = dec("list_price") + dec("price_extra");
    let mut values = serde_json::Map::new();
    if let Some(name) = p.get("name").filter(|v| !v.is_null()) {
        values.insert("name".to_string(), name.clone());
    }
    values.insert("price_unit".to_string(), json!(price.to_string()));
    values.insert("product_uom_qty".to_string(), json!("1"));
    if let Some(uom) = p.get("uom_id").filter(|v| !v.is_null()) {
        values.insert("uom_id".to_string(), uom.clone());
    }
    Ok(ServiceOutput::json(json!({ "values": serde_json::Value::Object(values) })))
}

/// Resolves the unit price for `variant_id` from `pricelist_id` at `date` and `quantity` — the
/// most-specific applicable rule (variant > product > category > global), else the variant's own sales
/// price. Module-owned bespoke READ SQL (recursive category ancestry + applied_on specificity ordering)
/// the domain-based finds cannot express, so it runs on the pool ServiceCtx hands out. Relocated verbatim
/// from the former `Db::resolve_price`.
pub async fn resolve_price(
    pool: &sqlx::PgPool,
    pricelist_id: i64,
    variant_id: i64,
    quantity: Decimal,
    date: &str,
) -> Result<Decimal, DbError> {
    // The variant's pricing inputs: own price_extra + the delegated template list_price / cost / category.
    let row = sqlx::query(
        "SELECT pp.product_tmpl_id AS tmpl, pp.price_extra AS extra, \
                pt.list_price AS list, pt.standard_price AS cost, pt.categ_id AS categ \
         FROM product_product pp JOIN product_template pt ON pt.id = pp.product_tmpl_id \
         WHERE pp.id = $1",
    )
    .bind(variant_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| DbError::BadInput(format!("variant {variant_id} not found")))?;
    let tmpl: i64 = row.try_get("tmpl")?;
    let extra: Decimal = row.try_get::<Option<Decimal>, _>("extra")?.unwrap_or_default();
    let list: Decimal = row.try_get::<Option<Decimal>, _>("list")?.unwrap_or_default();
    let cost: Decimal = row.try_get::<Option<Decimal>, _>("cost")?.unwrap_or_default();
    let categ: Option<i64> = row.try_get("categ")?;
    let lst_price = list + extra;

    // The product's category ancestry (category + parents), capped to terminate a deep/cyclic tree.
    let categ_chain: Vec<i64> = match categ {
        Some(c) => sqlx::query_scalar(
            "WITH RECURSIVE anc(id, parent_id, depth) AS (\
                 SELECT id, parent_id, 0 FROM product_category WHERE id = $1 \
                 UNION ALL \
                 SELECT c.id, c.parent_id, anc.depth + 1 FROM product_category c \
                   JOIN anc ON c.id = anc.parent_id WHERE anc.depth < 16) \
             SELECT id FROM anc",
        )
        .bind(c)
        .fetch_all(pool)
        .await?,
        None => Vec::new(),
    };

    // The most-specific applicable rule. applied_on sorts '0_product_variant' < '1_product' <
    // '2_product_category' < '3_global', so ORDER BY applied_on ASC takes the narrowest scope; then the
    // highest qualifying quantity tier.
    let item = sqlx::query(
        "SELECT compute_price, fixed_price, percent_price, base FROM product_pricelist_item \
         WHERE pricelist_id = $1 AND min_quantity <= $2 \
           AND (date_start IS NULL OR date_start <= $3::date) \
           AND (date_end IS NULL OR date_end >= $3::date) \
           AND (applied_on = '3_global' \
                OR (applied_on = '2_product_category' AND categ_id = ANY($4)) \
                OR (applied_on = '1_product' AND product_tmpl_id = $5) \
                OR (applied_on = '0_product_variant' AND product_id = $6)) \
         ORDER BY applied_on ASC, min_quantity DESC LIMIT 1",
    )
    .bind(pricelist_id)
    .bind(quantity)
    .bind(date)
    .bind(&categ_chain)
    .bind(tmpl)
    .bind(variant_id)
    .fetch_optional(pool)
    .await?;

    let item = match item {
        Some(i) => i,
        None => return Ok(lst_price), // no rule → the variant's own sales price
    };
    let compute: String = item.try_get("compute_price")?;
    let base: String = item.try_get("base")?;
    let base_price = if base == "standard_price" { cost } else { lst_price };
    let price = if compute == "fixed" {
        item.try_get::<Option<Decimal>, _>("fixed_price")?.unwrap_or_default()
    } else {
        let pct: Decimal = item.try_get::<Option<Decimal>, _>("percent_price")?.unwrap_or_default();
        let p = base_price * (Decimal::ONE - pct / Decimal::from(100));
        if p < Decimal::ZERO { Decimal::ZERO } else { p }
    };
    Ok(price)
}
