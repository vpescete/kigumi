//! Cross-record constraints (Odoo `@api.constrains`): a validation that runs INSIDE the write
//! transaction after the record and its One2many children are written, re-reading them, and rejects
//! the write (a typed error, rolled back) if violated.
//!
//! Unlike a SQL CHECK — which is single-row — a constraint reads the record together with its children
//! through the same [`ComputeInput`] the compute engine uses, so it can express invariants that span a
//! header and its lines. The canonical case is a balanced accounting entry: the sum of a move's debit
//! lines must equal the sum of its credit lines.
//!
//! v1 scope: constraints run on the top-level model being written. A constraint on a CHILD written
//! through its parent's nested One2many commands, or on an `_inherits` parent, is not evaluated — the
//! canonical "header constraint over its lines" works because the constraint sits on the header.

use crate::{Children, ComputeInput, Value};
use std::collections::BTreeMap;

/// A constraint over a just-written record: returns `Err(message)` to reject (and roll back) the write.
pub type ConstraintFn = fn(&ComputeInput) -> Result<(), String>;

/// Registration of a constraint on a model, evaluated when any of `fields` is written (an empty
/// `fields` runs on every write). Emitted by `register_constraint!`.
pub struct ConstraintRegistration {
    pub model: &'static str,
    pub fields: &'static [&'static str],
    pub func: ConstraintFn,
}
inventory::collect!(ConstraintRegistration);

/// True iff the model has any registered constraint — a cheap gate before the in-transaction re-read
/// that [`check_constraints`] needs.
pub fn has_constraints(model: &str) -> bool {
    inventory::iter::<ConstraintRegistration>.into_iter().any(|c| c.model == model)
}

/// A constraint violation: the failing rule's message plus the fields the rule DECLARED as its
/// triggers — the fields a form should highlight. Empty for an unconditional (whole-record) rule.
#[derive(Debug)]
pub struct ConstraintViolation {
    pub message: String,
    pub fields: &'static [&'static str],
}

/// Runs the model's constraints over a just-written record (`values` + its `children`). `changed` is
/// the set of written field names, or `None` on create (every constraint runs). A constraint runs when
/// it is unconditional (empty `fields`), on create, or when one of its trigger `fields` was written.
/// Returns the first violation (message + the rule's declared fields), which the caller maps to a
/// typed error that rolls back the tx.
pub fn check_constraints(
    model: &str,
    changed: Option<&[String]>,
    values: &BTreeMap<String, Value>,
    children: &Children,
) -> Result<(), ConstraintViolation> {
    for c in inventory::iter::<ConstraintRegistration>.into_iter().filter(|c| c.model == model) {
        let runs = match changed {
            None => true, // create: the whole record is new, so every constraint is checked
            Some(written) => {
                c.fields.is_empty() || c.fields.iter().any(|f| written.iter().any(|w| w == f))
            }
        };
        if runs {
            let input = ComputeInput::new(values, children);
            (c.func)(&input).map_err(|message| ConstraintViolation { message, fields: c.fields })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A constraint that always fails, so we can observe WHETHER it ran by the result.
    fn always_fail(_: &ComputeInput) -> Result<(), String> {
        Err("fired".to_string())
    }
    inventory::submit! { ConstraintRegistration { model: "cstr.scoped", fields: &["amount"], func: always_fail } }
    inventory::submit! { ConstraintRegistration { model: "cstr.always", fields: &[], func: always_fail } }

    fn run(model: &str, changed: Option<&[String]>) -> Result<(), ConstraintViolation> {
        check_constraints(model, changed, &BTreeMap::new(), &Children::new())
    }

    #[test]
    fn scoped_constraint_runs_only_when_a_trigger_field_changed() {
        // Trigger field present → runs (fails).
        assert!(run("cstr.scoped", Some(&["amount".to_string()])).is_err());
        // Only a non-trigger field changed → does NOT run (passes).
        assert!(run("cstr.scoped", Some(&["other".to_string()])).is_ok());
        // Create (changed = None) → every constraint runs.
        assert!(run("cstr.scoped", None).is_err());
    }

    #[test]
    fn unconditional_constraint_runs_on_any_write() {
        // Empty trigger list → runs regardless of which fields changed.
        assert!(run("cstr.always", Some(&["whatever".to_string()])).is_err());
        assert!(run("cstr.always", None).is_err());
    }

    #[test]
    fn model_without_constraints_is_a_noop() {
        assert!(!has_constraints("cstr.none"));
        assert!(run("cstr.none", None).is_ok());
    }
}
