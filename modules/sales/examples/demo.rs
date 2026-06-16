//! Demo: `cargo run -p meshble-mod-sales --example demo`
//! Shows: module-graph resolution (versioning) + model resolution → DDL + UI contract.

use meshble::prelude::*;
use meshble_mod_base as base; // depend on base so it joins the catalog
use meshble_mod_sales::{resolved_sale_order, MANIFEST};

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
    println!("--- UI contract (JSON, for any frontend) ---\n{}", to_ui_contract(&model));
}
