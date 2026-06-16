//! Catalogo dei moduli: i modelli e le estensioni si AUTO-REGISTRANO via `inventory`,
//! e il resolver li fonde senza wiring manuale.
//!
//! È l'equivalente dell'`_inherit` di Odoo, ma a build/link time e con confini netti:
//! le estensioni sono dati separati, fusi con check dei conflitti (`resolve`), non un
//! monkey-patch che muta una classe a runtime.

use crate::{resolve, validate_depends, FieldDef, ModelDescriptor, ResolvedModel};

/// Registrazione di un modello base (emessa da `#[model]`).
pub struct ModelRegistration {
    pub name: &'static str,
    pub module: &'static str,
    pub descriptor: fn() -> &'static ModelDescriptor,
}
inventory::collect!(ModelRegistration);

/// Registrazione di un'estensione di campi (emessa da `#[extend]`).
pub struct FieldExtension {
    pub target: &'static str,
    pub module: &'static str,
    pub fields: fn() -> &'static [FieldDef],
}
inventory::collect!(FieldExtension);

/// Risolve un modello dal CATALOGO: base registrata + tutte le estensioni auto-registrate,
/// fuse (con check dei conflitti) e validate. Nessun wiring: i moduli si auto-registrano.
pub fn resolve_registered(model: &str) -> Result<ResolvedModel, String> {
    let base = inventory::iter::<ModelRegistration>
        .into_iter()
        .find(|r| r.name == model)
        .ok_or_else(|| format!("modello '{model}' non registrato nel catalogo"))?;

    let mut exts: Vec<&'static FieldExtension> = inventory::iter::<FieldExtension>
        .into_iter()
        .filter(|e| e.target == model)
        .collect();
    exts.sort_by_key(|e| e.module); // ordine deterministico tra moduli

    let ext_fields: Vec<&'static [FieldDef]> = exts.iter().map(|e| (e.fields)()).collect();
    let m = resolve((base.descriptor)(), &ext_fields)?;
    validate_depends(&m)?;
    Ok(m)
}
