//! Facade for the Meshble framework. Application modules depend only on this crate.

// Re-export of inventory so that macros can emit `::meshble::inventory::submit!`
// without every module having to add the dependency.
pub use meshble_core::inventory;

/// Everything needed to define a module: `use meshble::prelude::*;`
pub mod prelude {
    pub use meshble_core::{
        check_access, check_compat, json_string, record_rule_domain, registered_model_names,
        resolve, resolve_all_registered, resolve_module_set, resolve_modules, resolve_registered,
        validate_depends, Acl, Condition, Ctx, Domain,
        DomainError, FieldBuilder, FieldDef, FieldExtension, FieldKind, Model, ModelDescriptor,
        ModelRegistration, ModuleDep, ModuleManifest, ModuleRegistration, Operation, Operator,
        RecordRule, ResolutionError, ResolvedModel, Sql, Value, FRAMEWORK_VERSION,
    };
    pub use meshble_macros::{extend, model};
    pub use meshble_schema::{openapi, to_ddl, to_ui_contract, FieldRule, UiRule};
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
