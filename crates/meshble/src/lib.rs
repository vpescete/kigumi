//! Facade del framework Meshble. I moduli applicativi dipendono solo da questo crate.

// Re-export di inventory così le macro possono emettere `::meshble::inventory::submit!`
// senza che ogni modulo debba aggiungere la dipendenza.
pub use meshble_core::inventory;

/// Tutto ciò che serve per definire un modulo: `use meshble::prelude::*;`
pub mod prelude {
    pub use meshble_core::{
        check_compat, resolve, resolve_registered, validate_depends, FieldDef, FieldExtension,
        FieldKind, Model, ModelDescriptor, ModelRegistration, ModuleDep, ModuleManifest,
        ResolutionError, ResolvedModel, FRAMEWORK_VERSION,
    };
    pub use meshble_macros::{extend, model};
    pub use meshble_schema::{to_ddl, to_ui_contract};
}
