//! Facade for the Meshble framework. Application modules depend only on this crate.

// Re-export of inventory so that macros can emit `::meshble::inventory::submit!`
// without every module having to add the dependency.
pub use meshble_core::inventory;

/// Everything needed to define a module: `use meshble::prelude::*;`
pub mod prelude {
    pub use meshble_core::{
        check_compat, resolve, resolve_module_set, resolve_modules, resolve_registered,
        validate_depends, FieldDef, FieldExtension, FieldKind, Model, ModelDescriptor,
        ModelRegistration, ModuleDep, ModuleManifest, ModuleRegistration, ResolutionError,
        ResolvedModel, FRAMEWORK_VERSION,
    };
    pub use meshble_macros::{extend, model};
    pub use meshble_schema::{to_ddl, to_ui_contract};
}

/// Registers a module's manifest in the global catalog, so `resolve_modules` can see it.
/// Use at module top level: `meshble::register_module!(MANIFEST);`
#[macro_export]
macro_rules! register_module {
    ($manifest:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ModuleRegistration { manifest: || $manifest }
        }
    };
}
