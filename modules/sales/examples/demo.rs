//! Demo: `cargo run -p meshble-mod-sales --example demo`
//! Shows: version compatibility check + model resolution → DDL + UI contract.

use meshble::prelude::*;
use meshble_mod_sales::{resolved_sale_order, MANIFEST};

fn main() {
    // 1. Versioning: is the module compatible with the framework?
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

    // 2. Metamodel → projections.
    let model = resolved_sale_order();
    println!("== Resolved model: {} ({} fields) ==\n", model.name, model.fields.len());
    println!("--- Postgres DDL ---\n{}\n", to_ddl(&model));
    println!("--- UI contract (JSON, for any frontend) ---\n{}", to_ui_contract(&model));
}
