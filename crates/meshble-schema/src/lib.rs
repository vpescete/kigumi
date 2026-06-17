//! The projections: from a `ResolvedModel` to Postgres DDL and an agnostic UI contract (JSON).
//!
//! Same source of truth → multiple outputs. The UI contract carries visibility/readonly rules as
//! a portable [`Domain`] JSON AST — the frontend evaluates them client-side from data, never an
//! eval'd string, and they are the SAME domains the server compiles to SQL.

mod openapi;
pub use openapi::openapi;

use meshble_core::{actions_for, json_string, Domain, DomainError, FieldKind, ResolvedModel};

/// Postgres SQL type for a field with a column.
fn pg_type(kind: &FieldKind) -> &'static str {
    match kind {
        FieldKind::Text | FieldKind::Selection(_) => "text",
        FieldKind::Integer => "bigint",
        FieldKind::Decimal { .. } => "numeric",
        FieldKind::Bool => "boolean",
        FieldKind::Many2one { .. } => "bigint",
        FieldKind::One2many { .. } => "", // no column
    }
}

fn table_of(dotted: &str) -> String {
    dotted.replace('.', "_")
}

/// Generates the `CREATE TABLE`. Non-stored computed fields and one2many produce no columns.
pub fn to_ddl(m: &ResolvedModel) -> String {
    let mut lines = vec!["  id bigserial PRIMARY KEY".to_string()];
    for f in m.fields.iter().filter(|f| f.has_column()) {
        let mut col = format!("  {} {}", f.name, pg_type(&f.kind));
        if let FieldKind::Many2one { target } = f.kind {
            col.push_str(&format!(" REFERENCES {}(id)", table_of(target)));
        }
        if f.required {
            col.push_str(" NOT NULL");
        }
        if f.unique {
            col.push_str(" UNIQUE");
        }
        // A CHECK expression is trusted (compile-time, module-author-supplied), like a const SQL.
        if let Some(expr) = f.check {
            col.push_str(&format!(" CHECK ({expr})"));
        }
        lines.push(col);
    }
    format!("CREATE TABLE {} (\n{}\n);", m.table, lines.join(",\n"))
}

/// UI widget suggested by the field's type (the frontend is free to override it).
fn widget(kind: &FieldKind) -> &'static str {
    match kind {
        FieldKind::Text => "char",
        FieldKind::Integer => "integer",
        FieldKind::Decimal { currency_field: Some(_) } => "monetary",
        FieldKind::Decimal { currency_field: None } => "float",
        FieldKind::Bool => "boolean",
        FieldKind::Selection(_) => "selection",
        FieldKind::Many2one { .. } => "many2one",
        FieldKind::One2many { .. } => "one2many",
    }
}

/// Which dynamic rule a [`FieldRule`] expresses.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UiRule {
    Invisible,
    Readonly,
}

/// A dynamic UI rule bound to a field: when `domain` holds for a record, the field becomes
/// invisible/readonly. The domain is a thunk because a [`Domain`] is not const-constructible.
#[derive(Clone, Copy)]
pub struct FieldRule {
    pub field: &'static str,
    pub rule: UiRule,
    pub domain: fn() -> Domain,
}

fn rule_json(rules: &[FieldRule], field: &str, kind: UiRule) -> Option<String> {
    rules
        .iter()
        .find(|r| r.field == field && r.rule == kind)
        .map(|r| (r.domain)().to_json())
}

/// Agnostic UI contract: JSON consumable by ANY frontend. Computed fields are readonly; dynamic
/// `invisible_when` / `readonly_when` rules are emitted as portable domain ASTs.
///
/// Returns an error if any rule references an unknown/invalid field — UI rules are validated, not
/// discovered broken in production (the Odoo `attrs`/xpath failure mode).
/// ponytail: JSON built by hand (zero-dep). Switch to serde in phase 5.
pub fn to_ui_contract(m: &ResolvedModel, rules: &[FieldRule]) -> Result<String, DomainError> {
    for r in rules {
        (r.domain)().validate(m)?;
    }
    let fields: Vec<String> = m
        .fields
        .iter()
        .map(|f| {
            let mut parts = vec![
                format!("\"name\": {}", json_string(f.name)),
                format!("\"label\": {}", json_string(f.label)),
                format!("\"widget\": \"{}\"", widget(&f.kind)),
                format!("\"required\": {}", f.required),
                format!("\"readonly\": {}", f.is_computed()),
            ];
            if let FieldKind::Selection(opts) = &f.kind {
                let items: Vec<String> = opts
                    .iter()
                    .map(|(v, l)| format!("{{ \"value\": {}, \"label\": {} }}", json_string(v), json_string(l)))
                    .collect();
                parts.push(format!("\"options\": [{}]", items.join(", ")));
            }
            // Relation target, so a frontend can fetch the related model's contract (e.g. to render
            // an inlined One2many's columns) and resolve Many2one references.
            match &f.kind {
                FieldKind::Many2one { target } => {
                    parts.push(format!("\"relation\": {}", json_string(target)));
                }
                FieldKind::One2many { target, inverse } => {
                    parts.push(format!("\"relation\": {}", json_string(target)));
                    parts.push(format!("\"inverse\": {}", json_string(inverse)));
                }
                _ => {}
            }
            // Surface the field default so a new-record form can prefill it.
            if let Some(d) = f.default {
                parts.push(format!("\"default\": {}", json_string(d)));
            }
            if let Some(j) = rule_json(rules, f.name, UiRule::Invisible) {
                parts.push(format!("\"invisible_when\": {j}"));
            }
            if let Some(j) = rule_json(rules, f.name, UiRule::Readonly) {
                parts.push(format!("\"readonly_when\": {j}"));
            }
            format!("    {{ {} }}", parts.join(", "))
        })
        .collect();
    // List view (D7): the columns a generic list renders. Default = the scalar (column-backed)
    // fields in declaration order; One2many relations are not columns. The frontend may subset/reorder.
    let columns: Vec<String> = m
        .fields
        .iter()
        .filter(|f| f.has_column())
        .map(|f| {
            format!(
                "    {{ \"name\": {}, \"label\": {}, \"widget\": \"{}\" }}",
                json_string(f.name),
                json_string(f.label),
                widget(&f.kind)
            )
        })
        .collect();

    // Actions (D7): the state-transition actions a form can offer (the buttons), with the groups
    // allowed to run each (empty = everyone). The frontend hides those the caller's groups don't grant.
    let actions: Vec<String> = actions_for(m.name)
        .iter()
        .map(|a| {
            let groups: Vec<String> = a.groups.iter().map(|g| json_string(g)).collect();
            format!(
                "    {{ \"name\": {}, \"groups\": [{}] }}",
                json_string(a.name),
                groups.join(", ")
            )
        })
        .collect();

    Ok(format!(
        "{{\n  \"model\": {},\n  \"type\": \"form\",\n  \"fields\": [\n{}\n  ],\n  \"list\": {{ \"columns\": [\n{}\n  ] }},\n  \"actions\": [\n{}\n  ]\n}}",
        json_string(m.name),
        fields.join(",\n"),
        columns.join(",\n"),
        actions.join(",\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use meshble_core::{resolve, FieldDef, ModelDescriptor, ResolvedModel};

    static MODEL: ModelDescriptor = ModelDescriptor {
        name: "demo.model",
        table: "demo_model",
        fields: &[FieldDef {
            name: "state", label: "State",
            kind: FieldKind::Selection(&[("a", "A"), ("b", "B")]),
            required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
    };
    fn model() -> ResolvedModel {
        resolve(&MODEL, &[]).unwrap()
    }
    fn good() -> Domain {
        Domain::field("state").eq("a")
    }
    fn bad() -> Domain {
        Domain::field("nope").eq("a")
    }

    #[test]
    fn emits_invisible_when_as_domain_ast() {
        let rules = &[FieldRule { field: "state", rule: UiRule::Invisible, domain: good }];
        let c = to_ui_contract(&model(), rules).unwrap();
        assert!(c.contains("\"invisible_when\": {\"field\":\"state\",\"op\":\"=\",\"value\":\"a\"}"));
    }

    #[test]
    fn selection_fields_carry_their_options() {
        let c = to_ui_contract(&model(), &[]).unwrap();
        assert!(c.contains("\"options\": [{ \"value\": \"a\", \"label\": \"A\" }, { \"value\": \"b\", \"label\": \"B\" }]"));
    }

    #[test]
    fn rejects_rule_referencing_unknown_field() {
        // A typo'd rule field is an error at build/load time, not a silent broken UI.
        let rules = &[FieldRule { field: "state", rule: UiRule::Readonly, domain: bad }];
        assert!(to_ui_contract(&model(), rules).is_err());
    }
}
