//! Wizards (Odoo's TransientModel + `default_get`): a transient model opened with server-computed
//! defaults derived from the caller's context (the record(s) the wizard acts on). The transient row
//! is a secured scratchpad the user edits, then a per-wizard "apply" service method reads it and
//! mutates the real target (mirroring how `generate_variants` / `apply_pricelist` are dedicated
//! methods — the framework does NOT generalize apply, exactly as Odoo's button method is hand-written).

use crate::Value;

/// The records a wizard is opened against (Odoo's `active_model` / `active_id` / `active_ids` context).
#[derive(Default)]
pub struct WizardContext {
    pub active_model: Option<String>,
    pub active_id: Option<i64>,
    pub active_ids: Vec<i64>,
}

/// Computes a wizard's seed field values from the context (Odoo's `default_get`). Pure in v1 (no DB
/// reads); DB-backed defaults are a later extension done by a dedicated open service method.
pub type WizardDefaultGet = fn(&WizardContext) -> Vec<(&'static str, Value)>;

/// Registration of a wizard (emitted by `register_wizard!`): binds a transient model to its
/// `default_get`. The model must also be `register_transient!`-marked (so its rows are GC'd).
pub struct WizardRegistration {
    pub model: &'static str,
    pub default_get: WizardDefaultGet,
}
inventory::collect!(WizardRegistration);

/// Looks up a registered wizard by model name.
pub fn wizard_for(model: &str) -> Option<&'static WizardRegistration> {
    inventory::iter::<WizardRegistration>.into_iter().find(|w| w.model == model)
}
