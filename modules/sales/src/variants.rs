//! The product-variant generation engine — a module-owned service on `product.template`, registered on
//! the `register_service!` seam and dispatched by `POST /api/product.template/:id/service/generate_variants`.
//! Behavior-preserving relocation of the former `Db::generate_variants`: kigumi-db no longer names any
//! product model, so the variant engine ships with the sales module that owns those tables.
//!
//! It builds the cartesian product of a template's attribute lines, reconciles it against the template's
//! existing variants (keeping/reactivating matches, archiving stale ones, creating the rest), and
//! materializes each variant's `price_extra`. The whole reconciliation runs on ONE transaction under a
//! per-template advisory lock (via `ServiceCtx::tx` / `insert_in_tx`), so concurrent generations of the same
//! template serialize and a batch of variants plus its join rows commits atomically.

use kigumi::prelude::*; // ServiceCtx, ServiceInput, ServiceOutput, DbError, Domain, Ctx, ResolvedModel
use serde_json::json;
use sqlx::Row;
use std::collections::{BTreeSet, HashMap, HashSet};

// The product-variant model graph the generator operates on. The ERP model-name literals belong here, in
// the module that owns the tables — kigumi-db resolves none of them.
const VG_VARIANT: &str = "product.product";
const VG_LINE: &str = "product.template.attribute.line";
const VG_PTAV: &str = "product.template.attribute.value";
const VG_ATTRIBUTE: &str = "product.attribute";
/// The junction (product.product.product_template_attribute_value_ids) linking a variant to its cells.
const VG_VARIANT_PTAV_REL: &str = "variant_ptav_rel";
/// Hard cap on variants produced by one call — a runaway cartesian product (5 attributes x 10 values
/// = 100k rows) must not explode the table in a single request.
const MAX_VARIANTS: usize = 1000;

/// Generates/reconciles the variants of the template named by the route `:id`. Gate (Write + visibility on
/// product.template) is enforced by run_service before the body runs. Returns the created / archived / kept
/// `product.product` ids: `{ "created": [..], "archived": [..], "kept": [..] }`.
pub async fn generate(cx: &mut ServiceCtx<'_, '_>, input: ServiceInput) -> Result<ServiceOutput, DbError> {
    let template_id = input.record_id;
    let variant = cx.resolve(VG_VARIANT)?;
    let line_model = cx.resolve(VG_LINE)?;
    let ptav = cx.resolve(VG_PTAV)?;
    let attribute = cx.resolve(VG_ATTRIBUTE)?;

    // Past the gate, the engine's own reads/writes run elevated (the join rows are not user-writable).
    let su = cx.elevated();

    // Read the template's attribute lines and their selected values (M2M projected as an id array).
    let lines = cx
        .find_secured(&line_model, &su, Some(&Domain::field("product_tmpl_id").eq(template_id)))
        .await?;

    struct Line {
        id: i64,
        attribute_id: i64,
        value_ids: Vec<i64>,
    }
    let mut parsed: Vec<Line> = Vec::new();
    for l in &lines {
        let id = l["id"].as_i64().ok_or_else(|| DbError::BadInput("attribute line missing id".into()))?;
        let attribute_id = l["attribute_id"].as_i64().unwrap_or(0);
        let mut value_ids: Vec<i64> = l["value_ids"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
            .unwrap_or_default();
        value_ids.sort_unstable(); // deterministic combo order, independent of array_agg
        value_ids.dedup();
        if value_ids.is_empty() {
            continue; // a line with no selected values contributes nothing
        }
        parsed.push(Line { id, attribute_id, value_ids });
    }

    // Exclude `no_variant` attributes (informational only — they never multiply variants). Read by id
    // directly: `id` is the implicit PK, not a domain-addressable field.
    let attr_ids: Vec<i64> = parsed.iter().map(|l| l.attribute_id).collect();
    if !attr_ids.is_empty() {
        let sql = format!(
            "SELECT id FROM {} WHERE create_variant = 'no_variant' AND id = ANY($1)",
            attribute.table
        );
        let no_variant: HashSet<i64> = sqlx::query_scalar::<_, i64>(&sql)
            .bind(&attr_ids)
            .fetch_all(cx.pool())
            .await?
            .into_iter()
            .collect();
        parsed.retain(|l| !no_variant.contains(&l.attribute_id));
    }

    // Bound the product before building it (saturating, so a huge product can't overflow usize).
    let mut total: usize = 1;
    for l in &parsed {
        total = total.saturating_mul(l.value_ids.len());
        if total > MAX_VARIANTS {
            return Err(DbError::BadInput(format!("variant count exceeds the cap of {MAX_VARIANTS}")));
        }
    }

    // Cartesian product → each combo is one (line_id, value_id) per line. Zero lines yields a single empty
    // combo (a template with no variant attributes still has one variant — Odoo parity).
    let mut combos: Vec<Vec<(i64, i64)>> = vec![Vec::new()];
    for l in &parsed {
        let mut next = Vec::with_capacity(combos.len() * l.value_ids.len());
        for combo in &combos {
            for &v in &l.value_ids {
                let mut c = combo.clone();
                c.push((l.id, v));
                next.push(c);
            }
        }
        combos = next;
    }

    // Each desired combo is keyed by its sorted set of attribute-VALUE ids — an order-independent identity
    // that survives regeneration (so an existing variant is recognised, not duplicated).
    let mut desired_keys: HashSet<Vec<i64>> = HashSet::new();
    let desired: Vec<(Vec<i64>, &Vec<(i64, i64)>)> = combos
        .iter()
        .map(|c| {
            let mut k: Vec<i64> = c.iter().map(|&(_, v)| v).collect();
            k.sort_unstable();
            k.dedup(); // a true SET, symmetric with the existing-variant key (a degenerate config could
            // select one value on two lines; dedup keeps the keys comparable / idempotent)
            (k, c)
        })
        .collect();

    // Snapshot the template's existing variants (active or archived) and the combo each represents, so
    // reconciliation keeps/reactivates matches and archives only the truly-stale ones. Runs on the service
    // tx under a per-template advisory lock: without it, two callers could each miss an existing join row in
    // their cell lookup and both insert it, and their reconciliations would race. The lock releases at
    // commit and gives this reconciliation a consistent snapshot of the template's current variants.
    let mut existing: HashMap<Vec<i64>, Vec<(i64, bool)>> = HashMap::new();
    {
        let tx = cx.tx();
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
            .bind(format!("variants:product_template:{template_id}"))
            .execute(&mut **tx)
            .await?;

        let vrows = sqlx::query(&format!(
            "SELECT id, active FROM {} WHERE product_tmpl_id = $1",
            variant.table
        ))
        .bind(template_id)
        .fetch_all(&mut **tx)
        .await?;
        let mut active_of: HashMap<i64, bool> = HashMap::new();
        for r in &vrows {
            // NULL-safe: `active` is nullable at the DB level (a default, not NOT NULL), so a row planted
            // with active=null must not panic the decode — treat NULL as active.
            active_of.insert(r.get::<i64, _>("id"), r.get::<Option<bool>, _>("active").unwrap_or(true));
        }
        // Each variant's combo = the set of attribute-value ids behind its PTAV links.
        let mut vset: HashMap<i64, BTreeSet<i64>> = HashMap::new();
        let prows = sqlx::query(&format!(
            "SELECT r.{rel_col} AS vid, p.product_attribute_value_id AS val \
             FROM {rel} r JOIN {ptav} p ON p.id = r.{rel_target} \
             WHERE p.product_tmpl_id = $1",
            rel = VG_VARIANT_PTAV_REL,
            rel_col = "product_id",
            rel_target = "ptav_id",
            ptav = ptav.table,
        ))
        .bind(template_id)
        .fetch_all(&mut **tx)
        .await?;
        for r in &prows {
            vset.entry(r.get::<i64, _>("vid")).or_default().insert(r.get::<i64, _>("val"));
        }
        for (&id, &active) in &active_of {
            let key: Vec<i64> = vset.get(&id).map(|s| s.iter().copied().collect()).unwrap_or_default();
            existing.entry(key).or_default().push((id, active));
        }
        // Deterministic survivor among any duplicate variants for one combo: keep the active row with the
        // lowest id (reactivate only when none is active). Without this, the bucket order is HashMap-random
        // and a {active, archived} pair could flip which sibling id is canonical on each regeneration —
        // churning the id that anchors a combo's stock / order history.
        for v in existing.values_mut() {
            v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        }
    }

    let mut cell_ptav: HashMap<(i64, i64), i64> = HashMap::new();
    let mut created: Vec<i64> = Vec::new();
    let mut archived: Vec<i64> = Vec::new();
    let mut kept: Vec<i64> = Vec::new();

    // Desired combos: keep/reactivate an existing variant, or create one. Any duplicate variants for the
    // same desired combo (e.g. from a pre-reconciliation create-only run) are archived so the template
    // converges to exactly one active variant per combination.
    for (key, combo) in &desired {
        desired_keys.insert(key.clone());
        match existing.get(key).filter(|v| !v.is_empty()) {
            Some(variants) => {
                let (first_id, first_active) = variants[0];
                if !first_active {
                    set_variant_active(cx.tx(), &variant, first_id, true).await?;
                }
                // Regeneration is a full refresh: re-materialize the kept variant's price_extra so a PTAV
                // price change since the last run is picked up.
                set_variant_price_extra(cx.tx(), &variant, &ptav, first_id).await?;
                kept.push(first_id);
                for &(dup_id, dup_active) in &variants[1..] {
                    if dup_active {
                        set_variant_active(cx.tx(), &variant, dup_id, false).await?;
                        archived.push(dup_id);
                    }
                }
            }
            None => {
                let mut ptav_ids: Vec<i64> = Vec::with_capacity(combo.len());
                for &(line_id, value_id) in combo.iter() {
                    let pid = match cell_ptav.get(&(line_id, value_id)) {
                        Some(&p) => p,
                        None => {
                            let p = ensure_ptav(cx, &ptav, &su, template_id, line_id, value_id).await?;
                            cell_ptav.insert((line_id, value_id), p);
                            p
                        }
                    };
                    ptav_ids.push(pid);
                }
                let payload = json!({
                    "product_tmpl_id": template_id,
                    "product_template_attribute_value_ids": ptav_ids,
                });
                let vid = cx.insert_in_tx(&variant, &su, payload.as_object().unwrap()).await?;
                // Materialize the new variant's price_extra from its just-inserted PTAV set.
                set_variant_price_extra(cx.tx(), &variant, &ptav, vid).await?;
                created.push(vid);
            }
        }
    }

    // Stale: active variants whose combo is no longer selected are ARCHIVED, never deleted (they may carry
    // stock / order history). A later regeneration that re-selects the combo reactivates them above (same
    // id, no duplicate).
    for (key, variants) in &existing {
        if !desired_keys.contains(key) {
            for &(id, active) in variants {
                if active {
                    set_variant_active(cx.tx(), &variant, id, false).await?;
                    archived.push(id);
                }
            }
        }
    }

    Ok(ServiceOutput::json(json!({
        "created": created,
        "archived": archived,
        "kept": kept,
    })))
}

/// Returns the `product.template.attribute.value` id for (line, value), creating it elevated if absent. The
/// caller holds a per-template advisory lock, so the lookup-then-insert is race-free against another
/// generation of the same template.
async fn ensure_ptav(
    cx: &mut ServiceCtx<'_, '_>,
    ptav: &ResolvedModel,
    su: &Ctx,
    template_id: i64,
    line_id: i64,
    value_id: i64,
) -> Result<i64, DbError> {
    let existing = {
        let tx = cx.tx();
        sqlx::query_scalar::<_, i64>(&format!(
            "SELECT id FROM {} WHERE attribute_line_id = $1 AND product_attribute_value_id = $2",
            ptav.table
        ))
        .bind(line_id)
        .bind(value_id)
        .fetch_optional(&mut **tx)
        .await?
    };
    if let Some(id) = existing {
        return Ok(id);
    }
    let payload = json!({
        "product_tmpl_id": template_id,
        "attribute_line_id": line_id,
        "product_attribute_value_id": value_id,
    });
    cx.insert_in_tx(ptav, su, payload.as_object().unwrap()).await
}

/// Write trigger (registered on `product.template.attribute.value`, watching `price_extra`): when a manager
/// edits a cell's `price_extra`, re-materialize `price_extra` on every variant whose combo includes this
/// cell — the Many2many aggregate the compute engine can't do on read. Runs on the caller's write tx, so the
/// refresh commits atomically with the edit. Behavior-preserving relocation of the former in-core M15.1 hook.
pub async fn ptav_price_extra_recompute<'c, 't>(
    tx: &'c mut sqlx::Transaction<'t, sqlx::Postgres>,
    ptav_id: i64,
    _changed: &'c [&'c str],
) -> Result<(), DbError> {
    let variant = resolve_registered(VG_VARIANT).map_err(DbError::BadInput)?;
    let ptav = resolve_registered(VG_PTAV).map_err(DbError::BadInput)?;
    let vids: Vec<i64> = sqlx::query_scalar(&format!(
        "SELECT product_id FROM {} WHERE ptav_id = $1",
        VG_VARIANT_PTAV_REL
    ))
    .bind(ptav_id)
    .fetch_all(&mut **tx)
    .await?;
    for vid in vids {
        set_variant_price_extra(tx, &variant, &ptav, vid).await?;
    }
    Ok(())
}

/// Sets a variant's `active` flag (a direct UPDATE, so it never re-enters the secured write path).
async fn set_variant_active(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    variant: &ResolvedModel,
    id: i64,
    active: bool,
) -> Result<(), DbError> {
    sqlx::query(&format!("UPDATE {} SET active = $1 WHERE id = $2", variant.table))
        .bind(active)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Materializes a variant's `price_extra` = SUM of its combo PTAVs' `price_extra` — the Many2many aggregate
/// the compute engine can't do on read. Recomputed only at the two bounded write points (generation, and a
/// PTAV `price_extra` edit). The SUM is taken in-tx so a just-inserted PTAV set is visible. Idempotent.
async fn set_variant_price_extra(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    variant: &ResolvedModel,
    ptav: &ResolvedModel,
    variant_id: i64,
) -> Result<(), DbError> {
    let sql = format!(
        "UPDATE {v} SET price_extra = COALESCE(\
             (SELECT SUM(p.price_extra) FROM {rel} r JOIN {ptav} p ON p.id = r.ptav_id \
              WHERE r.product_id = $1), 0) \
         WHERE id = $1",
        v = variant.table,
        rel = VG_VARIANT_PTAV_REL,
        ptav = ptav.table,
    );
    sqlx::query(&sql).bind(variant_id).execute(&mut **tx).await?;
    Ok(())
}
