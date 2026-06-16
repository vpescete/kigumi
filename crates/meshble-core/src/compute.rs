//! Compute engine: actually EXECUTES the `compute` declared on a field (until now it was inert
//! metadata). A compute function is a pure `fn(&ComputeInput) -> Value` registered by name; on
//! write, the engine fills each stored computed field whose function is registered.
//!
//! Scope: SAME-RECORD computes (a field derived from other fields of the same record, e.g.
//! `total = qty * price`). Relational/aggregate computes (dotted `depends` like `line_ids.x`)
//! are skipped until relations exist — they simply have no registered function yet.

use crate::{ResolvedModel, Value};
use std::collections::BTreeMap;

/// Read-only view of a record's field values, passed to a compute function.
pub struct ComputeInput<'a> {
    values: &'a BTreeMap<String, Value>,
}

impl ComputeInput<'_> {
    pub fn get(&self, field: &str) -> Option<&Value> {
        self.values.get(field)
    }
    pub fn int(&self, field: &str) -> i64 {
        match self.values.get(field) {
            Some(Value::Int(n)) => *n,
            _ => 0,
        }
    }
    pub fn float(&self, field: &str) -> f64 {
        match self.values.get(field) {
            Some(Value::Float(f)) => *f,
            Some(Value::Int(n)) => *n as f64,
            _ => 0.0,
        }
    }
    pub fn str(&self, field: &str) -> &str {
        match self.values.get(field) {
            Some(Value::Str(s)) => s,
            _ => "",
        }
    }
    pub fn bool(&self, field: &str) -> bool {
        matches!(self.values.get(field), Some(Value::Bool(true)))
    }
}

/// A registered compute function.
pub type ComputeFn = fn(&ComputeInput) -> Value;

/// Registration of a compute function by name (emitted by `register_compute!`).
pub struct ComputeRegistration {
    pub name: &'static str,
    pub func: ComputeFn,
}
inventory::collect!(ComputeRegistration);

/// Looks up a registered compute function by its declared name.
pub fn compute_fn(name: &str) -> Option<ComputeFn> {
    inventory::iter::<ComputeRegistration>
        .into_iter()
        .find(|r| r.name == name)
        .map(|r| r.func)
}

/// Names of the model's stored computed fields that have a registered function.
pub fn computed_fields(model: &ResolvedModel) -> Vec<&'static str> {
    model
        .fields
        .iter()
        .filter(|f| f.has_column() && f.is_computed())
        .filter(|f| f.compute.map(compute_fn).flatten().is_some())
        .map(|f| f.name)
        .collect()
}

/// Fills `values` with the result of each stored computed field whose function is registered.
/// Computes from a snapshot taken BEFORE writing any result, so computes read the user-provided
/// inputs (chained compute-on-compute is not ordered — out of scope).
pub fn compute_stored(model: &ResolvedModel, values: &mut BTreeMap<String, Value>) {
    let funcs: Vec<(&'static str, ComputeFn)> = model
        .fields
        .iter()
        .filter(|f| f.has_column() && f.is_computed())
        .filter_map(|f| f.compute.and_then(compute_fn).map(|func| (f.name, func)))
        .collect();
    if funcs.is_empty() {
        return;
    }
    let snapshot = values.clone();
    let input = ComputeInput { values: &snapshot };
    for (name, func) in funcs {
        values.insert(name.to_string(), func(&input));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{resolve, FieldDef, FieldKind, ModelDescriptor};

    fn compute_total(i: &ComputeInput) -> Value {
        Value::Float(i.float("qty") * i.float("price"))
    }
    inventory::submit! { ComputeRegistration { name: "test_compute_total", func: compute_total } }

    static MODEL: ModelDescriptor = ModelDescriptor {
        name: "line", table: "line",
        fields: &[
            FieldDef { name: "qty", label: "Qty", kind: FieldKind::Decimal { currency_field: None }, required: true, stored: true, compute: None, depends: &[] },
            FieldDef { name: "price", label: "Price", kind: FieldKind::Decimal { currency_field: None }, required: true, stored: true, compute: None, depends: &[] },
            FieldDef { name: "total", label: "Total", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: Some("test_compute_total"), depends: &["qty", "price"] },
        ],
    };

    #[test]
    fn computes_a_stored_field_from_the_record() {
        let m = resolve(&MODEL, &[]).unwrap();
        let mut values = BTreeMap::new();
        values.insert("qty".to_string(), Value::Float(3.0));
        values.insert("price".to_string(), Value::Float(5.0));
        compute_stored(&m, &mut values);
        assert_eq!(values.get("total"), Some(&Value::Float(15.0)));
    }

    #[test]
    fn unregistered_compute_is_left_untouched() {
        static M2: ModelDescriptor = ModelDescriptor {
            name: "x", table: "x",
            fields: &[FieldDef { name: "y", label: "Y", kind: FieldKind::Integer, required: false, stored: true, compute: Some("no_such_fn"), depends: &[] }],
        };
        let m = resolve(&M2, &[]).unwrap();
        let mut values = BTreeMap::new();
        compute_stored(&m, &mut values);
        assert!(values.get("y").is_none(), "no registered fn → field left alone");
    }
}
