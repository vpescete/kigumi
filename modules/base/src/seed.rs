//! Base reference data, run by the `register_seed!` seam at every migrate while `base` is
//! installed: one default currency + company (multi-company needs a company to exist), a starter
//! country set, and the read-only `res.groups` projection of every registered ACL/rule group.
//! Every block is guarded by an exists-check — the DB is the authority, an operator change is
//! never overwritten. Relocated verbatim from kigumi-cli so adopter binaries get it too.

use kigumi::prelude::*;

fn res(name: &str) -> Result<ResolvedModel, DbError> {
    resolve_registered(name).map_err(|e| DbError::Migration(format!("{name} not resolvable: {e:?}")))
}

/// Seeds one default currency + company on a fresh instance, plus countries and groups.
pub async fn seed_base_data(db: &Db) -> Result<(), DbError> {
    let currency = res("res.currency")?;
    let company = res("res.company")?;
    let su = Ctx::new(0, vec![]).sudo();

    let cur_id = if db.count_secured(&currency, &su, &[], &[], None).await? == 0 {
        let v = serde_json::json!({
            "name": "Euro", "code": "EUR", "symbol": "€",
            "decimal_places": 2, "rounding": 0.01, "position": "after", "active": true
        });
        db.insert_secured(&currency, &su, &[], &[], v.as_object().unwrap()).await?
    } else {
        db.find_ids_secured(&currency, &su, &[], &[], None).await?[0]
    };

    if db.count_secured(&company, &su, &[], &[], None).await? == 0 {
        let v = serde_json::json!({ "name": "Main Company", "currency_id": cur_id, "active": true });
        db.insert_secured(&company, &su, &[], &[], v.as_object().unwrap()).await?;
        println!("seeded default company + currency");
    }

    // Starter countries (insert only the ones not already present by code).
    let country = res("res.country")?;
    for (name, code) in [
        ("Italy", "IT"), ("France", "FR"), ("Germany", "DE"), ("Spain", "ES"),
        ("United Kingdom", "GB"), ("United States", "US"), ("Switzerland", "CH"), ("Netherlands", "NL"),
    ] {
        let by_code = Domain::field("code").eq(code);
        if db.count_secured(&country, &su, &[], &[], Some(&by_code)).await? == 0 {
            let v = serde_json::json!({ "name": name, "code": code, "active": true });
            db.insert_secured(&country, &su, &[], &[], v.as_object().unwrap()).await?;
        }
    }

    // The read-only res.groups list, projected from every group referenced by registered ACLs/rules.
    let groups = res("res.groups")?;
    for name in registered_group_names() {
        let by_name = Domain::field("name").eq(name.as_str());
        if db.count_secured(&groups, &su, &[], &[], Some(&by_name)).await? == 0 {
            let v = serde_json::json!({ "name": name });
            db.insert_secured(&groups, &su, &[], &[], v.as_object().unwrap()).await?;
        }
    }
    Ok(())
}
