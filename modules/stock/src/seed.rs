//! Warehouse seeding, run by the `register_seed!` seam at migrate while `stock` is installed:
//! one warehouse + the four standard locations for the default company. Skipped once any location
//! exists (an operator's topology is never touched). Relocated verbatim from kigumi-cli so adopter
//! binaries get it too.

use kigumi::prelude::*;

fn res(name: &str) -> Result<ResolvedModel, DbError> {
    resolve_registered(name).map_err(|e| DbError::Migration(format!("{name} not resolvable: {e:?}")))
}

/// Seeds a default warehouse + stock locations for the default company.
pub async fn seed_stock_data(db: &Db) -> Result<(), DbError> {
    let location = res("stock.location")?;
    let warehouse = res("stock.warehouse")?;
    let company = res("res.company")?;
    let su = Ctx::new(0, vec![]).sudo();

    if db.count_secured(&location, &su, &[], &[], None).await? > 0 {
        return Ok(());
    }
    let Some(&comp_id) = db.find_ids_secured(&company, &su, &[], &[], None).await?.first() else {
        return Ok(());
    };

    let loc = |name: &str, usage: &str| {
        serde_json::json!({ "name": name, "usage": usage, "company_id": comp_id, "active": true })
    };
    let new_location = |v: serde_json::Value| {
        let location = &location;
        let su = &su;
        async move { db.insert_secured(location, su, &[], &[], v.as_object().unwrap()).await }
    };
    let stock = new_location(loc("Stock", "internal")).await?;
    new_location(loc("Vendors", "supplier")).await?;
    new_location(loc("Customers", "customer")).await?;
    new_location(loc("Inventory adjustment", "inventory")).await?;

    db.insert_secured(
        &warehouse,
        &su,
        &[],
        &[],
        serde_json::json!({ "name": "Main Warehouse", "code": "WH", "location_id": stock, "company_id": comp_id, "active": true })
            .as_object()
            .unwrap(),
    )
    .await?;
    println!("seeded default warehouse + stock locations");
    Ok(())
}
