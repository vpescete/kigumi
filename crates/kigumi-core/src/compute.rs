//! Compute engine: actually EXECUTES the `compute` declared on a field (until now it was inert
//! metadata). A compute function is a pure `fn(&ComputeInput) -> Value` registered by name; on
//! write, the engine fills each stored computed field whose function is registered.
//!
//! Scope: SAME-RECORD computes (a field derived from other fields of the same record, e.g.
//! `total = qty * price`). Relational/aggregate computes (dotted `depends` like `line_ids.x`)
//! are skipped until relations exist — they simply have no registered function yet.

use crate::{ResolvedModel, Value};
use std::collections::BTreeMap;

/// One2many children grouped by the o2m field name (each child is its own field→value map).
pub type Children = BTreeMap<String, Vec<BTreeMap<String, Value>>>;

/// Read-only view of a record (its own fields + its One2many children), passed to a compute
/// function. Same-record computes read `int`/`float`/…; aggregate computes read `children`/`sum_float`.
pub struct ComputeInput<'a> {
    values: &'a BTreeMap<String, Value>,
    children: &'a Children,
}

impl<'a> ComputeInput<'a> {
    /// Builds a read-only view over a record's `values` and its One2many `children`. Used by the
    /// constraint engine (and internally by the compute engine).
    pub fn new(values: &'a BTreeMap<String, Value>, children: &'a Children) -> Self {
        ComputeInput { values, children }
    }
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

    /// The One2many children for `o2m_field` (empty if none / not loaded).
    pub fn children(&self, o2m_field: &str) -> &[BTreeMap<String, Value>] {
        self.children.get(o2m_field).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Sums a numeric `child_field` over the children of `o2m_field` (the classic aggregate).
    pub fn sum_float(&self, o2m_field: &str, child_field: &str) -> f64 {
        self.children(o2m_field)
            .iter()
            .map(|c| match c.get(child_field) {
                Some(Value::Float(f)) => *f,
                Some(Value::Int(n)) => *n as f64,
                _ => 0.0,
            })
            .sum()
    }

    pub fn count(&self, o2m_field: &str) -> usize {
        self.children(o2m_field).len()
    }

    /// An exact Decimal field value (0 if absent / not numeric).
    pub fn decimal(&self, field: &str) -> rust_decimal::Decimal {
        match self.values.get(field) {
            Some(Value::Decimal(d)) => *d,
            Some(Value::Int(n)) => rust_decimal::Decimal::from(*n),
            _ => rust_decimal::Decimal::ZERO,
        }
    }

    /// Sums an exact Decimal `child_field` over the children of `o2m_field` — the exact-money
    /// aggregate (no f64 rounding), e.g. `amount_total = sum(line_ids.price_subtotal)`.
    pub fn sum_decimal(&self, o2m_field: &str, child_field: &str) -> rust_decimal::Decimal {
        self.children(o2m_field)
            .iter()
            .map(|c| match c.get(child_field) {
                Some(Value::Decimal(d)) => *d,
                Some(Value::Int(n)) => rust_decimal::Decimal::from(*n),
                _ => rust_decimal::Decimal::ZERO,
            })
            .sum()
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
        .filter(|f| f.compute.and_then(compute_fn).is_some())
        .map(|f| f.name)
        .collect()
}

/// Evaluates the model's NON-stored computed fields (Odoo `compute=` without `store=True`) over a
/// record's `values` at READ time, returning the (name, value) pairs to inject into the projection.
/// These have no column and are never written — they are derived on every read. Same-record: `children`
/// is empty, so a function reads the record's own scalar fields plus any related / delegated values
/// already present in `values` (both are read alongside the columns), not its One2many children.
pub fn compute_on_read(model: &ResolvedModel, values: &BTreeMap<String, Value>) -> Vec<(&'static str, Value)> {
    let funcs: Vec<(&'static str, ComputeFn)> = model
        .fields
        .iter()
        .filter(|f| !f.has_column() && f.is_computed())
        .filter_map(|f| f.compute.and_then(compute_fn).map(|func| (f.name, func)))
        .collect();
    if funcs.is_empty() {
        return Vec::new();
    }
    let empty = Children::new();
    let input = ComputeInput { values, children: &empty };
    funcs.into_iter().map(|(name, func)| (name, func(&input))).collect()
}

/// True iff the model has any NON-stored computed field (a cheap gate before doing the extra decode
/// that [`compute_on_read`] needs).
pub fn has_read_computes(model: &ResolvedModel) -> bool {
    model.fields.iter().any(|f| !f.has_column() && f.is_computed())
}

/// Fills `values` with the result of each stored computed field whose function is registered,
/// given the record's own fields and its One2many `children` (for aggregate computes).
/// Computes from a snapshot taken BEFORE writing any result, so computes read the inputs
/// (chained compute-on-compute is not ordered — out of scope).
pub fn compute_stored(model: &ResolvedModel, values: &mut BTreeMap<String, Value>, children: &Children) {
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
    let input = ComputeInput { values: &snapshot, children };
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
            FieldDef { name: "qty", label: "Qty", kind: FieldKind::Decimal { currency_field: None }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
            FieldDef { name: "price", label: "Price", kind: FieldKind::Decimal { currency_field: None }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
            FieldDef { name: "total", label: "Total", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: Some("test_compute_total"), depends: &["qty", "price"], default: None, unique: false, check: None },
        ],
    };

    #[test]
    fn computes_a_stored_field_from_the_record() {
        let m = resolve(&MODEL, &[]).unwrap();
        let mut values = BTreeMap::new();
        values.insert("qty".to_string(), Value::Float(3.0));
        values.insert("price".to_string(), Value::Float(5.0));
        compute_stored(&m, &mut values, &Children::new());
        assert_eq!(values.get("total"), Some(&Value::Float(15.0)));
    }

    #[test]
    fn unregistered_compute_is_left_untouched() {
        static M2: ModelDescriptor = ModelDescriptor {
            name: "x", table: "x",
            fields: &[FieldDef { name: "y", label: "Y", kind: FieldKind::Integer, required: false, stored: true, compute: Some("no_such_fn"), depends: &[], default: None, unique: false, check: None }],
        };
        let m = resolve(&M2, &[]).unwrap();
        let mut values = BTreeMap::new();
        compute_stored(&m, &mut values, &Children::new());
        assert!(values.get("y").is_none(), "no registered fn → field left alone");
    }

    fn compute_order_total(i: &ComputeInput) -> Value {
        Value::Float(i.sum_float("line_ids", "price"))
    }
    inventory::submit! { ComputeRegistration { name: "test_order_total", func: compute_order_total } }

    #[test]
    fn computes_an_aggregate_over_children() {
        static ORDER: ModelDescriptor = ModelDescriptor {
            name: "order", table: "order",
            fields: &[
                FieldDef { name: "line_ids", label: "Lines", kind: FieldKind::One2many { target: "order.line", inverse: "order_id" }, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None },
                FieldDef { name: "amount_total", label: "Total", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: Some("test_order_total"), depends: &["line_ids.price"], default: None, unique: false, check: None },
            ],
        };
        let m = resolve(&ORDER, &[]).unwrap();
        let mut children = Children::new();
        let mut line1 = BTreeMap::new();
        line1.insert("price".to_string(), Value::Float(5.0));
        let mut line2 = BTreeMap::new();
        line2.insert("price".to_string(), Value::Float(3.0));
        children.insert("line_ids".to_string(), vec![line1, line2]);

        let mut values = BTreeMap::new();
        compute_stored(&m, &mut values, &children);
        assert_eq!(values.get("amount_total"), Some(&Value::Float(8.0)));
    }

    #[test]
    fn decimal_aggregate_is_exact() {
        use rust_decimal::Decimal;
        use std::str::FromStr;
        // 0.1 + 0.1 + 0.1 must be EXACTLY 0.3 — the f64 path this replaces gives 0.30000000000000004.
        let line = |p: &str| {
            let mut m = BTreeMap::new();
            m.insert("price".to_string(), Value::Decimal(Decimal::from_str(p).unwrap()));
            m
        };
        let mut children = Children::new();
        children.insert("line_ids".to_string(), vec![line("0.1"), line("0.1"), line("0.1")]);
        let snapshot = BTreeMap::new();
        let input = ComputeInput { values: &snapshot, children: &children };
        assert_eq!(input.sum_decimal("line_ids", "price"), Decimal::from_str("0.3").unwrap());
        assert_ne!(0.1_f64 + 0.1 + 0.1, 0.3, "f64 is inexact — the reason for rust_decimal");
    }
}
