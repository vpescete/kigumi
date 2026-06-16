//! Facade del framework Meshble. I moduli applicativi dipendono solo da questo crate.

/// Tutto ciò che serve per definire un modulo: `use meshble::prelude::*;`
pub mod prelude {
    pub use meshble_core::{
        check_compat, resolve, validate_depends, FieldDef, FieldKind, Model, ModelDescriptor,
        ModuleDep, ModuleManifest, ResolutionError, ResolvedModel, FRAMEWORK_VERSION,
    };
    pub use meshble_macros::model;
    pub use meshble_schema::{to_ddl, to_ui_contract};
}
