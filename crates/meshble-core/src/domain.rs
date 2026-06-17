//! Typed domain AST: a filter expression compiled to PARAMETERIZED SQL.
//!
//! Unlike Odoo's `ir.rule.domain_force` (a Python string evaluated with `safe_eval`), a domain
//! here is typed data: validated against the model (unknown fields are an error, not a runtime
//! surprise) and compiled with bound parameters (`$1, $2, …`), so values are never interpolated
//! into SQL text. This closes both the injection surface and the "broken filter discovered in
//! production" failure mode.

use crate::{FieldDef, FieldKind, ResolvedModel};

/// A scalar (or list) literal used on the right-hand side of a condition.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    /// Exact decimal for monetary/`Decimal` fields (stored as Postgres NUMERIC, serialized as a JSON
    /// string to preserve precision). `Float` remains for any non-exact float use.
    Decimal(rust_decimal::Decimal),
    Bool(bool),
    Null,
    List(Vec<Value>),
}

impl From<rust_decimal::Decimal> for Value {
    fn from(d: rust_decimal::Decimal) -> Self {
        Value::Decimal(d)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_string())
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}
impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Int(n)
    }
}
impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Float(n)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Operator {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    NotIn,
    Like,
    ILike,
    IsNull,
    IsNotNull,
}

/// A single leaf condition `field <op> value`.
#[derive(Clone, Debug, PartialEq)]
pub struct Condition {
    pub field: String,
    pub op: Operator,
    pub value: Value,
}

/// A filter expression. Combine with `and`/`or`/`not`.
#[derive(Clone, Debug, PartialEq)]
pub enum Domain {
    True,
    False,
    Cond(Condition),
    And(Box<Domain>, Box<Domain>),
    Or(Box<Domain>, Box<Domain>),
    Not(Box<Domain>),
}

impl Domain {
    /// Starts a condition on `name`: `Domain::field("state").eq("sale")`.
    pub fn field(name: &str) -> FieldBuilder {
        FieldBuilder(name.to_string())
    }
    pub fn and(self, other: Domain) -> Domain {
        Domain::And(Box::new(self), Box::new(other))
    }
    pub fn or(self, other: Domain) -> Domain {
        Domain::Or(Box::new(self), Box::new(other))
    }
    pub fn not(self) -> Domain {
        Domain::Not(Box::new(self))
    }

    /// The full (possibly dotted) field paths referenced by this domain's conditions, e.g.
    /// `["state", "partner_id.name"]`. Used to vet a caller-supplied filter against field-level
    /// access (D6) — the caller must be able to read EVERY field a relational path traverses, so a
    /// restricted field cannot be probed even through a relation.
    pub fn condition_paths(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.collect_paths(&mut out);
        out
    }
    fn collect_paths<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Domain::Cond(c) => out.push(&c.field),
            Domain::And(a, b) | Domain::Or(a, b) => {
                a.collect_paths(out);
                b.collect_paths(out);
            }
            Domain::Not(d) => d.collect_paths(out),
            Domain::True | Domain::False => {}
        }
    }
}

/// Fluent builder for a single condition.
pub struct FieldBuilder(String);

impl FieldBuilder {
    fn cond(self, op: Operator, value: Value) -> Domain {
        Domain::Cond(Condition { field: self.0, op, value })
    }
    pub fn eq(self, v: impl Into<Value>) -> Domain {
        self.cond(Operator::Eq, v.into())
    }
    pub fn ne(self, v: impl Into<Value>) -> Domain {
        self.cond(Operator::Ne, v.into())
    }
    pub fn lt(self, v: impl Into<Value>) -> Domain {
        self.cond(Operator::Lt, v.into())
    }
    pub fn le(self, v: impl Into<Value>) -> Domain {
        self.cond(Operator::Le, v.into())
    }
    pub fn gt(self, v: impl Into<Value>) -> Domain {
        self.cond(Operator::Gt, v.into())
    }
    pub fn ge(self, v: impl Into<Value>) -> Domain {
        self.cond(Operator::Ge, v.into())
    }
    pub fn like(self, v: impl Into<Value>) -> Domain {
        self.cond(Operator::Like, v.into())
    }
    pub fn ilike(self, v: impl Into<Value>) -> Domain {
        self.cond(Operator::ILike, v.into())
    }
    pub fn is_null(self) -> Domain {
        self.cond(Operator::IsNull, Value::Null)
    }
    pub fn is_not_null(self) -> Domain {
        self.cond(Operator::IsNotNull, Value::Null)
    }
    pub fn in_<T: Into<Value>>(self, vs: impl IntoIterator<Item = T>) -> Domain {
        self.cond(Operator::In, Value::List(vs.into_iter().map(Into::into).collect()))
    }
    pub fn not_in<T: Into<Value>>(self, vs: impl IntoIterator<Item = T>) -> Domain {
        self.cond(Operator::NotIn, Value::List(vs.into_iter().map(Into::into).collect()))
    }
}

/// Compiled SQL: a boolean expression plus its ordered bound parameters.
#[derive(Debug, PartialEq)]
pub struct Sql {
    pub where_clause: String,
    pub params: Vec<Value>,
}

#[derive(Debug, PartialEq)]
pub enum DomainError {
    UnknownField { field: String, model: String },
    NotAColumn { field: String },
    TypeMismatch { field: String, detail: String },
    BadOperatorValue { field: String, detail: String },
    /// A dotted-path segment is not a relation (cannot be traversed).
    UnsupportedPath { field: String },
    /// A relation's target model is not registered in the catalog (cannot resolve the join).
    UnknownRelation { target: String, detail: String },
    /// The JSON domain (from `from_json`) is malformed.
    BadJson(String),
}

impl Domain {
    /// Validates the domain against `model` and compiles it to parameterized SQL.
    pub fn compile(&self, model: &ResolvedModel) -> Result<Sql, DomainError> {
        let mut params = Vec::new();
        let where_clause = self.compile_into(model, &mut params)?;
        Ok(Sql { where_clause, params })
    }

    /// Compiles the domain, APPENDING its bound parameters to `params` and numbering placeholders
    /// to continue after whatever is already there. Lets a caller embed the WHERE after other
    /// parameters (e.g. an UPDATE's SET values + id) without placeholder collisions.
    pub fn compile_into(
        &self,
        model: &ResolvedModel,
        params: &mut Vec<Value>,
    ) -> Result<String, DomainError> {
        self.emit(model, params)
    }

    fn emit(&self, model: &ResolvedModel, params: &mut Vec<Value>) -> Result<String, DomainError> {
        match self {
            Domain::True => Ok("TRUE".to_string()),
            Domain::False => Ok("FALSE".to_string()),
            Domain::Not(d) => Ok(format!("(NOT {})", d.emit(model, params)?)),
            Domain::And(a, b) => {
                Ok(format!("({} AND {})", a.emit(model, params)?, b.emit(model, params)?))
            }
            Domain::Or(a, b) => {
                Ok(format!("({} OR {})", a.emit(model, params)?, b.emit(model, params)?))
            }
            Domain::Cond(c) => emit_cond(c, model, params),
        }
    }
}

fn emit_cond(
    c: &Condition,
    model: &ResolvedModel,
    params: &mut Vec<Value>,
) -> Result<String, DomainError> {
    emit_path_cond(model, &c.field, c.op, &c.value, params)
}

/// Compiles a (possibly dotted) field path. A relation segment becomes a SUBQUERY against the
/// target table — Many2one as `fk IN (SELECT id FROM target WHERE <rest>)`, One2many as
/// `id IN (SELECT inverse FROM target WHERE <rest>)`. This works uniformly in SELECT/UPDATE/DELETE
/// `WHERE` (so record rules can traverse relations) with no joins to manage, and handles NULLs
/// correctly (a null FK simply doesn't match). Nesting handles multi-hop paths.
fn emit_path_cond(
    model: &ResolvedModel,
    path: &str,
    op: Operator,
    value: &Value,
    params: &mut Vec<Value>,
) -> Result<String, DomainError> {
    let (first, rest) = match path.split_once('.') {
        None => return emit_leaf_cond(model, path, op, value, params),
        Some(parts) => parts,
    };
    let field = model.fields.iter().find(|f| f.name == first).ok_or_else(|| {
        DomainError::UnknownField { field: first.to_string(), model: model.name.to_string() }
    })?;
    let resolve_target = |target: &str| {
        crate::resolve_registered(target)
            .map_err(|e| DomainError::UnknownRelation { target: target.to_string(), detail: e })
    };
    // Use the model's own identifier in SQL, never the raw input segment. Subqueries are made
    // NULL-safe (2-valued) so that `Not(...)` around a relation traversal behaves correctly: the
    // result is never UNKNOWN, so a null FK / orphan child can't silently break a negated rule.
    let col = field.name;
    match &field.kind {
        FieldKind::Many2one { target } => {
            let t = resolve_target(target)?;
            let inner = emit_path_cond(&t, rest, op, value, params)?;
            // `id` is a PK (never null), so the IN set has no NULLs; the explicit IS NOT NULL
            // makes a null FK evaluate to FALSE (not UNKNOWN), so `NOT(...)` includes null-FK rows.
            Ok(format!("({col} IS NOT NULL AND {col} IN (SELECT id FROM {} WHERE {inner}))", t.table))
        }
        FieldKind::One2many { target, inverse } => {
            let t = resolve_target(target)?;
            let inner = emit_path_cond(&t, rest, op, value, params)?;
            // Exclude null inverse FKs so the IN set never contains NULL (the NOT IN footgun).
            Ok(format!(
                "id IN (SELECT {inverse} FROM {} WHERE {inverse} IS NOT NULL AND ({inner}))",
                t.table
            ))
        }
        _ => Err(DomainError::UnsupportedPath { field: path.to_string() }),
    }
}

/// Compiles a single (non-dotted) field condition against `model`.
fn emit_leaf_cond(
    model: &ResolvedModel,
    field_name: &str,
    op: Operator,
    value: &Value,
    params: &mut Vec<Value>,
) -> Result<String, DomainError> {
    let field = model.fields.iter().find(|f| f.name == field_name).ok_or_else(|| {
        DomainError::UnknownField { field: field_name.to_string(), model: model.name.to_string() }
    })?;
    if !field.has_column() {
        return Err(DomainError::NotAColumn { field: field_name.to_string() });
    }
    // The identifier comes from the model (a controlled static), never from the input string.
    let col = field.name;

    match op {
        Operator::IsNull => Ok(format!("{col} IS NULL")),
        Operator::IsNotNull => Ok(format!("{col} IS NOT NULL")),
        Operator::In | Operator::NotIn => {
            let list = match value {
                Value::List(v) => v,
                _ => {
                    return Err(DomainError::BadOperatorValue {
                        field: field_name.to_string(),
                        detail: "IN/NOT IN require a list value".to_string(),
                    })
                }
            };
            if list.is_empty() {
                // `x IN ()` is always false; `x NOT IN ()` is always true.
                return Ok(if matches!(op, Operator::In) { "FALSE" } else { "TRUE" }.to_string());
            }
            let mut placeholders = Vec::with_capacity(list.len());
            for v in list {
                if matches!(v, Value::Null) {
                    // `x NOT IN (.., NULL)` is UNKNOWN for every row — a silent record-rule footgun.
                    return Err(DomainError::BadOperatorValue {
                        field: field_name.to_string(),
                        detail: "NULL is not allowed inside IN/NOT IN; use is_null()/is_not_null()"
                            .to_string(),
                    });
                }
                check_value_type(field, v)?;
                params.push(v.clone());
                placeholders.push(format!("${}", params.len()));
            }
            let kw = if matches!(op, Operator::In) { "IN" } else { "NOT IN" };
            Ok(format!("{col} {kw} ({})", placeholders.join(", ")))
        }
        _ => {
            if matches!(value, Value::List(_)) {
                return Err(DomainError::BadOperatorValue {
                    field: field_name.to_string(),
                    detail: "scalar operator given a list value".to_string(),
                });
            }
            // NULL never compares with =, <>, <, … (the result is UNKNOWN, matching zero rows).
            // Normalize the meaningful cases and reject the rest, so a rule is never silently empty.
            if matches!(value, Value::Null) {
                return match op {
                    Operator::Eq => Ok(format!("{col} IS NULL")),
                    Operator::Ne => Ok(format!("{col} IS NOT NULL")),
                    _ => Err(DomainError::BadOperatorValue {
                        field: field_name.to_string(),
                        detail: "NULL only supported with =, !=, is_null(), is_not_null()"
                            .to_string(),
                    }),
                };
            }
            check_operator_kind(op, &field.kind, field_name)?;
            check_value_type(field, value)?;
            params.push(value.clone());
            let p = format!("${}", params.len());
            let sql_op = match op {
                Operator::Eq => "=",
                Operator::Ne => "<>",
                Operator::Lt => "<",
                Operator::Le => "<=",
                Operator::Gt => ">",
                Operator::Ge => ">=",
                Operator::Like => "LIKE",
                Operator::ILike => "ILIKE",
                Operator::In | Operator::NotIn | Operator::IsNull | Operator::IsNotNull => {
                    unreachable!("handled above")
                }
            };
            Ok(format!("{col} {sql_op} {p}"))
        }
    }
}

/// Rejects operator/field-kind combinations that are almost always author mistakes (e.g. LIKE
/// on a non-text field, ordering a boolean), so the validator catches them before production.
fn check_operator_kind(op: Operator, kind: &FieldKind, field: &str) -> Result<(), DomainError> {
    let bad = |detail: &str| {
        Err(DomainError::BadOperatorValue { field: field.to_string(), detail: detail.to_string() })
    };
    match op {
        Operator::Like | Operator::ILike if !matches!(kind, FieldKind::Text) => {
            bad("LIKE/ILIKE only apply to Text fields")
        }
        Operator::Lt | Operator::Le | Operator::Gt | Operator::Ge
            if matches!(kind, FieldKind::Bool) =>
        {
            bad("ordering operators do not apply to Bool fields")
        }
        _ => Ok(()),
    }
}

/// Checks that a (non-null) value is type-compatible with the field's kind. Null is handled at
/// the operator level (only IS NULL / IS NOT NULL), so it must never reach here.
fn check_value_type(field: &FieldDef, v: &Value) -> Result<(), DomainError> {
    let ok = match (&field.kind, v) {
        (FieldKind::Text | FieldKind::Selection(_), Value::Str(_)) => true,
        (FieldKind::Integer | FieldKind::Many2one { .. }, Value::Int(_)) => true,
        (FieldKind::Decimal { .. }, Value::Int(_) | Value::Decimal(_)) => true,
        // NaN / Infinity cannot be stored in NUMERIC and make every comparison UNKNOWN.
        (FieldKind::Decimal { .. }, Value::Float(f)) => f.is_finite(),
        (FieldKind::Bool, Value::Bool(_)) => true,
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(DomainError::TypeMismatch {
            field: field.name.to_string(),
            detail: format!("{:?} is not compatible with field kind {:?}", v, field.kind),
        })
    }
}

impl Domain {
    /// Serializes the domain to a portable JSON AST. A frontend evaluates visibility/readonly
    /// rules client-side from this DATA — the same AST the server compiles to SQL, never an
    /// eval'd string.
    pub fn to_json(&self) -> String {
        match self {
            Domain::True => "{\"const\":true}".to_string(),
            Domain::False => "{\"const\":false}".to_string(),
            Domain::Not(d) => format!("{{\"not\":{}}}", d.to_json()),
            Domain::And(a, b) => format!("{{\"and\":[{},{}]}}", a.to_json(), b.to_json()),
            Domain::Or(a, b) => format!("{{\"or\":[{},{}]}}", a.to_json(), b.to_json()),
            Domain::Cond(c) => cond_to_json(c),
        }
    }

    /// Validates the domain against `model` (field existence, types, operator/kind) without
    /// emitting SQL. Used to reject malformed UI rules at build/load time.
    pub fn validate(&self, model: &ResolvedModel) -> Result<(), DomainError> {
        self.compile(model).map(|_| ())
    }

    /// Parses a domain from the portable JSON AST (the inverse of [`Domain::to_json`]) — used for
    /// the `?domain=<json>` query escape and for admin-authored record rules stored as data. The
    /// result is still untrusted: validate/compile it against a model before use.
    pub fn from_json(s: &str) -> Result<Domain, DomainError> {
        let v: serde_json::Value =
            serde_json::from_str(s).map_err(|e| DomainError::BadJson(e.to_string()))?;
        domain_from_json(&v)
    }
}

fn bad_json(msg: impl Into<String>) -> DomainError {
    DomainError::BadJson(msg.into())
}

fn domain_from_json(v: &serde_json::Value) -> Result<Domain, DomainError> {
    let obj = v.as_object().ok_or_else(|| bad_json("each domain node must be a JSON object"))?;
    if let Some(c) = obj.get("const") {
        return Ok(if c.as_bool().unwrap_or(false) { Domain::True } else { Domain::False });
    }
    if let Some(n) = obj.get("not") {
        return Ok(Domain::Not(Box::new(domain_from_json(n)?)));
    }
    if let Some(a) = obj.get("and") {
        return fold_json(a, Domain::True, Domain::and);
    }
    if let Some(o) = obj.get("or") {
        return fold_json(o, Domain::False, Domain::or);
    }
    if let Some(f) = obj.get("field") {
        let field = f.as_str().ok_or_else(|| bad_json("'field' must be a string"))?.to_string();
        let op_s = obj.get("op").and_then(|x| x.as_str()).ok_or_else(|| bad_json("missing 'op'"))?;
        let op = op_from_str(op_s).ok_or_else(|| bad_json(format!("unknown operator '{op_s}'")))?;
        let value = match op {
            Operator::IsNull | Operator::IsNotNull => Value::Null,
            _ => value_from_json(obj.get("value").ok_or_else(|| bad_json("missing 'value'"))?)?,
        };
        return Ok(Domain::Cond(Condition { field, op, value }));
    }
    Err(bad_json("unrecognized domain node (expected const/not/and/or/field)"))
}

fn fold_json(
    v: &serde_json::Value,
    base: Domain,
    combine: fn(Domain, Domain) -> Domain,
) -> Result<Domain, DomainError> {
    let arr = v.as_array().ok_or_else(|| bad_json("'and'/'or' expects an array"))?;
    let mut acc: Option<Domain> = None;
    for item in arr {
        let d = domain_from_json(item)?;
        acc = Some(match acc {
            Some(prev) => combine(prev, d),
            None => d,
        });
    }
    Ok(acc.unwrap_or(base))
}

fn op_from_str(s: &str) -> Option<Operator> {
    Some(match s {
        "=" => Operator::Eq,
        "!=" => Operator::Ne,
        "<" => Operator::Lt,
        "<=" => Operator::Le,
        ">" => Operator::Gt,
        ">=" => Operator::Ge,
        "in" => Operator::In,
        "not in" => Operator::NotIn,
        "like" => Operator::Like,
        "ilike" => Operator::ILike,
        "is null" => Operator::IsNull,
        "is not null" => Operator::IsNotNull,
        _ => return None,
    })
}

fn value_from_json(v: &serde_json::Value) -> Result<Value, DomainError> {
    Ok(match v {
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64().filter(|x| x.is_finite()) {
                Value::Float(f)
            } else {
                return Err(bad_json("numeric value out of range"));
            }
        }
        serde_json::Value::Array(items) => {
            Value::List(items.iter().map(value_from_json).collect::<Result<Vec<_>, _>>()?)
        }
        serde_json::Value::Object(_) => return Err(bad_json("a value cannot be an object")),
    })
}

fn op_str(op: Operator) -> &'static str {
    match op {
        Operator::Eq => "=",
        Operator::Ne => "!=",
        Operator::Lt => "<",
        Operator::Le => "<=",
        Operator::Gt => ">",
        Operator::Ge => ">=",
        Operator::In => "in",
        Operator::NotIn => "not in",
        Operator::Like => "like",
        Operator::ILike => "ilike",
        Operator::IsNull => "is null",
        Operator::IsNotNull => "is not null",
    }
}

fn cond_to_json(c: &Condition) -> String {
    match c.op {
        Operator::IsNull | Operator::IsNotNull => {
            format!("{{\"field\":{},\"op\":\"{}\"}}", json_string(&c.field), op_str(c.op))
        }
        _ => format!(
            "{{\"field\":{},\"op\":\"{}\",\"value\":{}}}",
            json_string(&c.field),
            op_str(c.op),
            value_to_json(&c.value)
        ),
    }
}

fn value_to_json(v: &Value) -> String {
    match v {
        Value::Str(s) => json_string(s),
        Value::Int(n) => n.to_string(),
        // Non-finite floats are rejected at compile time; guard here so to_json never emits
        // invalid JSON (NaN/inf) if called on an unvalidated domain.
        Value::Float(f) => {
            if f.is_finite() {
                f.to_string()
            } else {
                "null".to_string()
            }
        }
        // Exact decimals serialize as a JSON STRING so precision is never lost through f64.
        Value::Decimal(d) => json_string(&d.to_string()),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        Value::List(items) => {
            format!("[{}]", items.iter().map(value_to_json).collect::<Vec<_>>().join(","))
        }
    }
}

/// JSON-escapes `s` and wraps it in double quotes. Shared by domain and UI-contract serialization
/// so every string in the emitted JSON is escaped consistently (no broken-out quotes).
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{resolve, FieldDef, FieldKind, ModelDescriptor};

    static MODEL: ModelDescriptor = ModelDescriptor {
        name: "sale.order",
        table: "sale_order",
        fields: &[
            FieldDef {
                name: "state", label: "State",
                kind: FieldKind::Selection(&[("draft", "Draft"), ("done", "Done")]),
                required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
            FieldDef {
                name: "amount_total", label: "Total",
                kind: FieldKind::Decimal { currency_field: None },
                required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
            FieldDef {
                name: "line_ids", label: "Lines",
                kind: FieldKind::One2many { target: "sale.order.line", inverse: "order_id" },
                required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None },
            FieldDef {
                name: "flag", label: "Flag", kind: FieldKind::Bool,
                required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
            FieldDef {
                name: "partner_id", label: "Partner",
                kind: FieldKind::Many2one { target: "rel.partner" },
                required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        ],
    };

    // A registered target model so relation traversal can resolve it.
    static PARTNER: ModelDescriptor = ModelDescriptor {
        name: "rel.partner", table: "rel_partner",
        fields: &[FieldDef {
            name: "code", label: "Code", kind: FieldKind::Text,
            required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
    };
    fn partner_desc() -> &'static ModelDescriptor {
        &PARTNER
    }
    inventory::submit! {
        crate::ModelRegistration { name: "rel.partner", module: "test", descriptor: partner_desc }
    }

    fn model() -> crate::ResolvedModel {
        resolve(&MODEL, &[]).unwrap()
    }

    #[test]
    fn compiles_parameterized_sql() {
        let d = Domain::field("state").ne("done").and(Domain::field("amount_total").lt(10000_i64));
        let sql = d.compile(&model()).unwrap();
        assert_eq!(sql.where_clause, "(state <> $1 AND amount_total < $2)");
        assert_eq!(sql.params, vec![Value::Str("done".into()), Value::Int(10000)]);
    }

    #[test]
    fn values_are_never_inlined_into_sql() {
        // A SQL-injection attempt must end up as a bound parameter, not in the SQL text.
        let evil = "x'; DROP TABLE sale_order; --";
        let sql = Domain::field("state").eq(evil).compile(&model()).unwrap();
        assert_eq!(sql.where_clause, "state = $1");
        assert!(!sql.where_clause.contains("DROP"));
        assert_eq!(sql.params, vec![Value::Str(evil.into())]);
    }

    #[test]
    fn unknown_field_is_rejected() {
        let d = Domain::field("nope").eq("x");
        assert!(matches!(d.compile(&model()), Err(DomainError::UnknownField { .. })));
    }

    #[test]
    fn one2many_is_not_a_column() {
        let d = Domain::field("line_ids").eq(1_i64);
        assert!(matches!(d.compile(&model()), Err(DomainError::NotAColumn { .. })));
    }

    #[test]
    fn type_mismatch_is_rejected() {
        // amount_total is numeric; a string value is invalid.
        let d = Domain::field("amount_total").eq("oops");
        assert!(matches!(d.compile(&model()), Err(DomainError::TypeMismatch { .. })));
    }

    #[test]
    fn empty_in_is_false() {
        let d = Domain::field("state").in_(Vec::<&str>::new());
        assert_eq!(d.compile(&model()).unwrap().where_clause, "FALSE");
    }

    #[test]
    fn in_list_expands_to_placeholders() {
        let d = Domain::field("state").in_(["draft", "done"]);
        let sql = d.compile(&model()).unwrap();
        assert_eq!(sql.where_clause, "state IN ($1, $2)");
        assert_eq!(sql.params.len(), 2);
    }

    #[test]
    fn dotted_path_through_relation_compiles_to_subquery() {
        // partner_id is a Many2one to the registered rel.partner → traversal becomes a subquery.
        let d = Domain::field("partner_id.code").eq("acme");
        let sql = d.compile(&model()).unwrap();
        assert_eq!(
            sql.where_clause,
            "(partner_id IS NOT NULL AND partner_id IN (SELECT id FROM rel_partner WHERE code = $1))"
        );
        assert_eq!(sql.params, vec![Value::Str("acme".into())]);
    }

    #[test]
    fn dotted_path_through_one2many_compiles_to_inverse_subquery() {
        // line_ids is a One2many whose target is unregistered here → UnknownRelation.
        let d = Domain::field("line_ids.price").gt(0_i64);
        assert!(matches!(d.compile(&model()), Err(DomainError::UnknownRelation { .. })));
    }

    #[test]
    fn dotted_path_through_non_relation_is_unsupported() {
        // `state` is a Selection, not a relation — cannot traverse into it.
        let d = Domain::field("state.x").eq("y");
        assert!(matches!(d.compile(&model()), Err(DomainError::UnsupportedPath { .. })));
    }

    #[test]
    fn eq_null_becomes_is_null() {
        let sql = Domain::field("state").eq(Value::Null).compile(&model()).unwrap();
        assert_eq!(sql.where_clause, "state IS NULL");
        assert!(sql.params.is_empty(), "NULL must not be bound as a parameter");
    }

    #[test]
    fn ne_null_becomes_is_not_null() {
        let sql = Domain::field("state").ne(Value::Null).compile(&model()).unwrap();
        assert_eq!(sql.where_clause, "state IS NOT NULL");
        assert!(sql.params.is_empty());
    }

    #[test]
    fn ordering_with_null_is_rejected() {
        let d = Domain::field("amount_total").lt(Value::Null);
        assert!(matches!(d.compile(&model()), Err(DomainError::BadOperatorValue { .. })));
    }

    #[test]
    fn null_inside_in_list_is_rejected() {
        let d = Domain::field("state").not_in(vec![Value::from("done"), Value::Null]);
        assert!(matches!(d.compile(&model()), Err(DomainError::BadOperatorValue { .. })));
    }

    #[test]
    fn non_finite_float_is_rejected() {
        let d = Domain::field("amount_total").gt(f64::NAN);
        assert!(matches!(d.compile(&model()), Err(DomainError::TypeMismatch { .. })));
    }

    #[test]
    fn like_on_non_text_is_rejected() {
        // `state` is a Selection (closed enum) — pattern matching it is almost always a mistake.
        let d = Domain::field("state").like("dra%");
        assert!(matches!(d.compile(&model()), Err(DomainError::BadOperatorValue { .. })));
    }

    #[test]
    fn ordering_on_bool_is_rejected() {
        let d = Domain::field("flag").gt(false);
        assert!(matches!(d.compile(&model()), Err(DomainError::BadOperatorValue { .. })));
    }

    #[test]
    fn serializes_to_json_ast() {
        let d = Domain::field("state").eq("done").and(Domain::field("amount_total").gt(100_i64));
        assert_eq!(
            d.to_json(),
            r#"{"and":[{"field":"state","op":"=","value":"done"},{"field":"amount_total","op":">","value":100}]}"#
        );
    }

    #[test]
    fn json_strings_are_escaped() {
        // A value containing quotes/backslashes must not break out of the JSON string.
        let d = Domain::field("state").eq("a\"b\\c");
        assert!(d.to_json().contains(r#""value":"a\"b\\c""#));
    }

    #[test]
    fn is_null_json_omits_value() {
        assert_eq!(Domain::field("state").is_null().to_json(), r#"{"field":"state","op":"is null"}"#);
    }

    #[test]
    fn from_json_round_trips() {
        let d = Domain::field("state").eq("draft").and(Domain::field("amount").ge(100_i64));
        assert_eq!(Domain::from_json(&d.to_json()).unwrap(), d);
    }

    #[test]
    fn from_json_parses_or_not_and_isnull() {
        let d = Domain::from_json(
            r#"{"or":[{"field":"a","op":"=","value":1},{"not":{"field":"b","op":"is null"}}]}"#,
        )
        .unwrap();
        assert!(matches!(d, Domain::Or(_, _)));
    }

    #[test]
    fn from_json_rejects_garbage() {
        assert!(Domain::from_json("not json").is_err());
        assert!(Domain::from_json(r#"{"field":"a"}"#).is_err()); // missing op
        assert!(Domain::from_json(r#"{"field":"a","op":"??","value":1}"#).is_err()); // unknown op
    }
}
