//! Demo: `cargo run -p kigumi-mod-sales --example demo`
//! Shows: module-graph resolution (versioning) + model resolution → DDL + UI contract.

use kigumi::prelude::*;
use kigumi_mod_base as base; // depend on base so it joins the catalog
use kigumi_mod_sales::{resolved_sale_order, ACLS, MANIFEST, RECORD_RULES, UI_RULES};

fn main() {
    // Reference a base symbol so the base module is linked and self-registers in the catalog.
    let _ = base::MANIFEST;

    // 1. Resolve the whole module graph: framework compatibility, dependency version ranges,
    //    no cycles — returned in dependency (load) order. This is what Odoo cannot verify.
    match resolve_modules() {
        Ok(order) => {
            let names: Vec<String> =
                order.iter().map(|m| format!("{} v{}", m.name, m.version)).collect();
            println!("Module load order (resolved): {}\n", names.join("  ->  "));
        }
        Err(e) => {
            eprintln!("module resolution failed: {e:?}");
            std::process::exit(1);
        }
    }

    // 2. Per-module framework compatibility (sales).
    match check_compat(&MANIFEST, FRAMEWORK_VERSION) {
        Ok(()) => println!(
            "module '{}' v{} compatible with framework v{} (range \"{}\")\n",
            MANIFEST.name, MANIFEST.version, FRAMEWORK_VERSION, MANIFEST.framework
        ),
        Err(e) => {
            eprintln!("incompatible: {e:?}");
            std::process::exit(1);
        }
    }

    // 3. Metamodel → projections.
    let model = resolved_sale_order();
    println!("== Resolved model: {} ({} fields) ==\n", model.name, model.fields.len());
    println!("--- Postgres DDL ---\n{}\n", to_ddl(&model));
    let contract = to_ui_contract(&model, UI_RULES).expect("valid UI rules");
    println!("--- UI contract (JSON, for any frontend) ---\n{}\n", contract);

    // 4. Security: ACL + row-level record rules → parameterized SQL (no string eval).
    let junior = Ctx::new(7, vec!["sales.user".to_string()]);
    println!("== Security (user 7, group 'sales.user') ==");
    println!("  can read:   {}", check_access(Operation::Read, "sale.order", &junior, ACLS));
    println!("  can delete: {}", check_access(Operation::Delete, "sale.order", &junior, ACLS));
    if let Some(rule) = record_rule_domain(Operation::Read, "sale.order", &junior, RECORD_RULES) {
        let sql = rule.compile(&model).expect("compile record rule");
        println!("  read row-filter WHERE: {}", sql.where_clause);
        println!("  bound params:          {:?}", sql.params);
    }
    println!(
        "  sudo read restriction: {:?}",
        record_rule_domain(Operation::Read, "sale.order", &junior.sudo(), RECORD_RULES)
    );
}
