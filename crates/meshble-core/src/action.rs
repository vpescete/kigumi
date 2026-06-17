//! State-transition actions: a named server-side operation on a record (e.g. confirm draft→sale),
//! run under ACL + record rules. An action is a pure `fn(&ActionInput) -> Result<ActionOutcome>`:
//! it reads the current record (for guards) and returns the field updates to apply; a structured
//! `assign_sequence` directive lets it request gapless numbering without needing async DB access.

use crate::Value;
use std::collections::BTreeMap;

/// Read-only view of the record an action runs on (its current stored values), with typed accessors.
pub struct ActionInput<'a> {
    values: &'a BTreeMap<String, Value>,
}

impl<'a> ActionInput<'a> {
    pub fn new(values: &'a BTreeMap<String, Value>) -> Self {
        Self { values }
    }
    pub fn get(&self, field: &str) -> Option<&Value> {
        self.values.get(field)
    }
    pub fn str(&self, field: &str) -> &str {
        match self.values.get(field) {
            Some(Value::Str(s)) => s,
            _ => "",
        }
    }
    pub fn int(&self, field: &str) -> i64 {
        match self.values.get(field) {
            Some(Value::Int(n)) => *n,
            _ => 0,
        }
    }
    pub fn decimal(&self, field: &str) -> rust_decimal::Decimal {
        match self.values.get(field) {
            Some(Value::Decimal(d)) => *d,
            Some(Value::Int(n)) => rust_decimal::Decimal::from(*n),
            _ => rust_decimal::Decimal::ZERO,
        }
    }
    pub fn bool(&self, field: &str) -> bool {
        matches!(self.values.get(field), Some(Value::Bool(true)))
    }
}

/// What an action produces: field updates to apply, plus an optional "assign field = next_value(code)"
/// directive resolved by the persistence layer (which can call the sequence engine).
#[derive(Default)]
pub struct ActionOutcome {
    pub set: BTreeMap<String, Value>,
    pub assign_sequence: Option<(String, String)>,
}

impl ActionOutcome {
    pub fn new() -> Self {
        Self::default()
    }
    /// Sets `field` to `value` in the resulting write.
    pub fn set(mut self, field: &str, value: Value) -> Self {
        self.set.insert(field.to_string(), value);
        self
    }
    /// Requests that `field` be assigned the next value of sequence `code` (e.g. "SO").
    pub fn assign_sequence(mut self, field: &str, code: &str) -> Self {
        self.assign_sequence = Some((field.to_string(), code.to_string()));
        self
    }
}

/// A registered action: a pure transition function. Guards (e.g. "only a draft") live in the body
/// and return `Err(message)` to refuse the transition.
pub type ActionFn = fn(&ActionInput) -> Result<ActionOutcome, String>;

/// Registration of an action by (model, name), emitted by `register_action!`. `groups` (if non-empty)
/// restricts who may run it, on top of the model's Write ACL + record rules.
pub struct ActionRegistration {
    pub model: &'static str,
    pub name: &'static str,
    pub func: ActionFn,
    pub groups: &'static [&'static str],
}
inventory::collect!(ActionRegistration);

/// Looks up a registered action by model + name.
pub fn action_for(model: &str, name: &str) -> Option<&'static ActionRegistration> {
    inventory::iter::<ActionRegistration>.into_iter().find(|a| a.model == model && a.name == name)
}
