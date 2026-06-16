//! Facade for the Meshble framework. Application modules depend only on this crate.

// Re-export of inventory so that macros can emit `::meshble::inventory::submit!`
// without every module having to add the dependency.
pub use meshble_core::inventory;

/// Everything needed to define a module: `use meshble::prelude::*;`
pub mod prelude {
    pub use meshble_core::{
        check_compat, resolve, resolve_registered, validate_depends, FieldDef, FieldExtension,
        FieldKind, Model, ModelDescriptor, ModelRegistration, ModuleDep, ModuleManifest,
        ResolutionError, ResolvedModel, FRAMEWORK_VERSION,
    };
    pub use meshble_macros::{extend, model};
    pub use meshble_schema::{to_ddl, to_ui_contract};
}
