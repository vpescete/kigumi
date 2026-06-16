//! Module catalog: models and extensions AUTO-REGISTER via `inventory`,
//! and the resolver merges them with no manual wiring.
//!
//! It is the equivalent of Odoo's `_inherit`, but at build/link time and with clean
//! boundaries: extensions are separate data, merged with conflict checks (`resolve`), not a
//! monkey-patch that mutates a class at runtime.

use crate::{
    resolve, resolve_module_set, validate_depends, FieldDef, ModelDescriptor, ModuleManifest,
    ResolutionError, ResolvedModel, FRAMEWORK_VERSION,
};

/// Registration of a base model (emitted by `#[model]`).
pub struct ModelRegistration {
    pub name: &'static str,
    pub module: &'static str,
    pub descriptor: fn() -> &'static ModelDescriptor,
}
inventory::collect!(ModelRegistration);

/// Registration of a field extension (emitted by `#[extend]`).
pub struct FieldExtension {
    pub target: &'static str,
    pub module: &'static str,
    pub fields: fn() -> &'static [FieldDef],
}
inventory::collect!(FieldExtension);

/// Registration of a module manifest (emitted by `register_module!`).
pub struct ModuleRegistration {
    pub manifest: fn() -> ModuleManifest,
}
inventory::collect!(ModuleRegistration);

/// Resolves every module registered in the catalog: framework compatibility, dependency
/// version ranges, no duplicates, no cycles — returning them in dependency order.
/// The validation `resolve_module_set` does is exactly what Odoo's `depends` cannot express.
pub fn resolve_modules() -> Result<Vec<ModuleManifest>, ResolutionError> {
    let modules: Vec<ModuleManifest> = inventory::iter::<ModuleRegistration>
        .into_iter()
        .map(|r| (r.manifest)())
        .collect();
    resolve_module_set(&modules, FRAMEWORK_VERSION)
}

/// Names of all models registered in the catalog (sorted, deterministic).
pub fn registered_model_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> =
        inventory::iter::<ModelRegistration>.into_iter().map(|r| r.name).collect();
    names.sort_unstable();
    names
}

/// Resolves every registered model (base + extensions). The bridge from the catalog to a server
/// or any consumer that needs the full set of models.
pub fn resolve_all_registered() -> Result<Vec<ResolvedModel>, String> {
    registered_model_names().iter().map(|n| resolve_registered(n)).collect()
}

/// Resolves a model from the CATALOG: registered base + all auto-registered extensions,
/// merged (with conflict checks) and validated. No wiring: modules auto-register.
pub fn resolve_registered(model: &str) -> Result<ResolvedModel, String> {
    let base = inventory::iter::<ModelRegistration>
        .into_iter()
        .find(|r| r.name == model)
        .ok_or_else(|| format!("model '{model}' not registered in the catalog"))?;

    let mut exts: Vec<&'static FieldExtension> = inventory::iter::<FieldExtension>
        .into_iter()
        .filter(|e| e.target == model)
        .collect();
    exts.sort_by_key(|e| e.module); // deterministic ordering across modules

    let ext_fields: Vec<&'static [FieldDef]> = exts.iter().map(|e| (e.fields)()).collect();
    let m = resolve((base.descriptor)(), &ext_fields)?;
    validate_depends(&m)?;
    Ok(m)
}
