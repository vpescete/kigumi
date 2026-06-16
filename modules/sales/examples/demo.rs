//! Demo: `cargo run -p meshble-mod-sales --example demo`
//! Mostra: check di compatibilità versione + risoluzione modello → DDL + contratto-UI.

use meshble::prelude::*;
use meshble_mod_sales::{resolved_sale_order, MANIFEST};

fn main() {
    // 1. Versioning: il modulo è compatibile col framework?
    match check_compat(&MANIFEST, FRAMEWORK_VERSION) {
        Ok(()) => println!(
            "modulo '{}' v{} compatibile con framework v{} (range \"{}\")\n",
            MANIFEST.name, MANIFEST.version, FRAMEWORK_VERSION, MANIFEST.framework
        ),
        Err(e) => {
            eprintln!("incompatibile: {e:?}");
            std::process::exit(1);
        }
    }

    // 2. Metamodello → proiezioni.
    let model = resolved_sale_order();
    println!("== Modello risolto: {} ({} campi) ==\n", model.name, model.fields.len());
    println!("--- DDL Postgres ---\n{}\n", to_ddl(&model));
    println!("--- Contratto-UI (JSON, per qualsiasi frontend) ---\n{}", to_ui_contract(&model));
}
