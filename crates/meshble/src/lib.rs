//! Facade for the Meshble framework. Application modules depend only on this crate.

// Re-export of inventory so that macros can emit `::meshble::inventory::submit!`
// without every module having to add the dependency.
pub use meshble_core::inventory;

/// Everything needed to define a module: `use meshble::prelude::*;`
pub mod prelude {
    pub use meshble_core::{
        action_for, check_access, check_compat, compute_fn, compute_stored, computed_fields,
        external_tables, field_accessible, field_required_groups, json_string, migration_plan,
        record_rule_domain, registered_acls, registered_group_names, registered_model_names,
        registered_rules, resolve, resolve_all_registered, resolve_module_set, resolve_modules,
        resolve_registered, validate_depends, Acl, AclRegistration, ActionFn, ActionInput,
        ActionOutcome, ActionRegistration, ComputeFn, ComputeInput, ComputeRegistration, Condition,
        Ctx, Domain, DomainError, ExternalTable, FieldBuilder, FieldDef, FieldExtension,
        FieldGroupRegistration, FieldKind, MigrationTarget, Model, ModelDescriptor,
        ModelRegistration, ModuleDep, ModuleManifest, ModuleRegistration, Operation, Operator,
        RecordRule, RecordRuleRegistration, ResolutionError, ResolvedModel, RuleDomain, Sql, Value,
        FRAMEWORK_VERSION,
    };
    pub use meshble_macros::{extend, model};
    pub use meshble_schema::{openapi, to_ddl, to_ui_contract, FieldRule, UiRule};
}

/// Registers a compute function by name, so the engine runs it on write for fields declaring it.
/// Use at module top level: `meshble::register_compute!("compute_total", compute_total);`
#[macro_export]
macro_rules! register_compute {
    ($name:expr, $func:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ComputeRegistration { name: $name, func: $func }
        }
    };
}

/// Registers a module's manifest in the global catalog, so `resolve_modules` can see it.
/// Use at module top level: `meshble::register_module!(MANIFEST);`
#[macro_export]
macro_rules! register_module {
    ($manifest:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ModuleRegistration { manifest: || $manifest, crate_path: ::core::module_path!() }
        }
    };
}

/// Marks a model's table as owned outside the metamodel (Odoo's `_auto = False`): the model is
/// resolved/served normally but migration never creates or alters its table. For models mapped onto
/// a pre-existing table (e.g. `res.users` onto the auth subsystem's `meshble_user`) or a SQL view.
/// Use at module top level: `meshble::register_external!("res.users");`
#[macro_export]
macro_rules! register_external {
    ($model:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ExternalTable { model: $model }
        }
    };
}

/// Restricts a model field to the given groups (D6 field-level security): read AND write of that
/// field require membership in at least one group; superuser bypasses. Usually emitted automatically
/// by `#[field(groups = "...")]`, but can be declared by hand:
/// `meshble::register_field_groups!("res.users", "login", &["admin"]);`
#[macro_export]
macro_rules! register_field_groups {
    ($model:expr, $field:expr, $groups:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::FieldGroupRegistration { model: $model, field: $field, groups: $groups }
        }
    };
}

/// Registers a module's ACLs so a server collects them via `registered_acls()`.
/// Use at module top level: `meshble::register_acls!(ACLS);` where `ACLS: &'static [Acl]`.
#[macro_export]
macro_rules! register_acls {
    ($acls:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::AclRegistration { acls: || $acls }
        }
    };
}

/// Registers a module's record rules so a server collects them via `registered_rules()`.
/// Use at module top level: `meshble::register_rules!(RULES);` where `RULES: &'static [RecordRule]`.
#[macro_export]
macro_rules! register_rules {
    ($rules:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::RecordRuleRegistration { rules: || $rules }
        }
    };
}

/// Registers a state-transition action on a model, runnable via `POST /api/<model>/<id>/action/<name>`.
/// `meshble::register_action!("sale.order", "confirm", confirm_order, &["sales.user"]);`
#[macro_export]
macro_rules! register_action {
    ($model:expr, $name:expr, $func:expr, $groups:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ActionRegistration { model: $model, name: $name, func: $func, groups: $groups }
        }
    };
}
