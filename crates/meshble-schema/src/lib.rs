//! The projections: from a `ResolvedModel` to Postgres DDL and an agnostic UI contract (JSON).
//!
//! Same source of truth → multiple outputs. The UI contract carries visibility/readonly rules as
//! a portable [`Domain`] JSON AST — the frontend evaluates them client-side from data, never an
//! eval'd string, and they are the SAME domains the server compiles to SQL.

mod openapi;
pub use openapi::openapi;

use meshble_core::{actions_for, delegated_fields, field_is_readonly, field_required_groups, is_mailed, json_string, related_path, reports_for, view_for, Domain, DomainError, FieldDef, FieldKind, ResolvedModel};

/// Postgres SQL type for a field with a column.
fn pg_type(kind: &FieldKind) -> &'static str {
    match kind {
        FieldKind::Text | FieldKind::Html | FieldKind::Selection(_) => "text",
        FieldKind::Integer => "bigint",
        FieldKind::Float => "double precision",
        FieldKind::Decimal { .. } => "numeric",
        FieldKind::Bool => "boolean",
        FieldKind::Date => "date",
        FieldKind::Datetime => "timestamptz",
        // An Image is a bigint FK to ir.attachment (the FK clause is added in `to_ddl`).
        FieldKind::Many2one { .. } | FieldKind::Image => "bigint",
        // No column on this model: One2many lives on the inverse, Many2many in a junction table.
        FieldKind::One2many { .. } | FieldKind::Many2many { .. } => "",
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
        match f.kind {
            FieldKind::Many2one { target } => col.push_str(&format!(" REFERENCES {}(id)", table_of(target))),
            // An Image references the attachment table directly (ir.attachment → meshble_attachment).
            FieldKind::Image => col.push_str(" REFERENCES meshble_attachment(id)"),
            _ => {}
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
        FieldKind::Html => "html",
        FieldKind::Image => "image",
        FieldKind::Integer => "integer",
        FieldKind::Float => "float",
        FieldKind::Decimal { currency_field: Some(_) } => "monetary",
        FieldKind::Decimal { currency_field: None } => "float",
        FieldKind::Bool => "boolean",
        FieldKind::Date => "date",
        FieldKind::Datetime => "datetime",
        FieldKind::Selection(_) => "selection",
        FieldKind::Many2one { .. } => "many2one",
        FieldKind::One2many { .. } => "one2many",
        FieldKind::Many2many { .. } => "many2many",
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
    // Emits one field's contract JSON. `readonly` is decided by the caller (computed/related →
    // read-only; own and delegated scalar fields → editable). UI rules are looked up by field name.
    let emit = |f: &FieldDef, readonly: bool| -> String {
        let mut parts = vec![
            format!("\"name\": {}", json_string(f.name)),
            format!("\"label\": {}", json_string(f.label)),
            format!("\"widget\": \"{}\"", widget(&f.kind)),
            format!("\"required\": {}", f.required),
            format!("\"readonly\": {readonly}"),
        ];
        if let FieldKind::Selection(opts) = &f.kind {
            let items: Vec<String> = opts
                .iter()
                .map(|(v, l)| format!("{{ \"value\": {}, \"label\": {} }}", json_string(v), json_string(l)))
                .collect();
            parts.push(format!("\"options\": [{}]", items.join(", ")));
        }
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
    };
    let mut fields: Vec<String> = m
        .fields
        .iter()
        .map(|f| {
            // Computed and related fields are read-only mirrors; `#[field(readonly)]` marks a stored
            // field (e.g. a materialized on-hand) read-only too.
            let readonly = f.is_computed() || related_path(m.name, f.name).is_some() || field_is_readonly(m.name, f.name);
            emit(f, readonly)
        })
        .collect();
    // _inherits: delegated parent fields are exposed transparently as editable fields (the write-split
    // routes them to the parent), so the generic form/list shows them with no knowledge of the split.
    let delegated = delegated_fields(m.name).unwrap_or_default();
    for d in &delegated {
        fields.push(emit(&d.def, false));
    }
    // List view (D7): the columns a generic list (and an inline One2many table) renders. List-friendly
    // = a scalar-ish widget (no image/html blob, no relational table). A field gated to a group is left
    // out of the generic list (it stays available on the form). When the model declares a form view, its
    // grouped fields ARE the curated, ordered column set (capped); otherwise the column-backed / computed
    // / related / delegated fields are used.
    let list_friendly = |kind: &FieldKind| {
        !matches!(
            kind,
            FieldKind::Image | FieldKind::Html | FieldKind::One2many { .. } | FieldKind::Many2many { .. }
        )
    };
    let col_json = |name: &str, label: &str, kind: &FieldKind| {
        format!(
            "    {{ \"name\": {}, \"label\": {}, \"widget\": \"{}\" }}",
            json_string(name),
            json_string(label),
            widget(kind)
        )
    };
    const MAX_LIST_COLUMNS: usize = 8;
    let columns: Vec<String> = if let Some(v) = view_for(m.name) {
        let mut cols = Vec::new();
        for g in v.groups {
            for s in g.fields {
                if cols.len() >= MAX_LIST_COLUMNS {
                    break;
                }
                if field_required_groups(m.name, s.name).is_some() {
                    continue;
                }
                // The slot field is either the model's own or a delegated parent field; format inline so
                // no reference into the local `delegated` vec escapes.
                if let Some(f) = m.fields.iter().find(|f| f.name == s.name) {
                    if list_friendly(&f.kind) {
                        cols.push(col_json(f.name, f.label, &f.kind));
                    }
                } else if let Some(d) = delegated.iter().find(|d| d.def.name == s.name) {
                    if list_friendly(&d.def.kind) {
                        cols.push(col_json(d.def.name, d.def.label, &d.def.kind));
                    }
                }
            }
        }
        cols
    } else {
        let mut cols: Vec<String> = m
            .fields
            .iter()
            .filter(|f| {
                (f.has_column() || f.is_computed() || related_path(m.name, f.name).is_some())
                    && list_friendly(&f.kind)
                    && field_required_groups(m.name, f.name).is_none()
            })
            .take(MAX_LIST_COLUMNS)
            .map(|f| col_json(f.name, f.label, &f.kind))
            .collect();
        for d in &delegated {
            if cols.len() < MAX_LIST_COLUMNS && list_friendly(&d.def.kind) {
                cols.push(col_json(d.def.name, d.def.label, &d.def.kind));
            }
        }
        cols
    };

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

    // Reports: the HTML/PDF documents a form can print for one record (GET .../report/<name>).
    let reports: Vec<String> = reports_for(m.name)
        .iter()
        .map(|r| format!("    {{ \"name\": {}, \"title\": {} }}", json_string(r.name), json_string(r.title)))
        .collect();

    // Form view (D-layout): the model's declared layout — groups of scalar fields + notebook pages.
    // `null` when the model declares no view (the frontend applies a smart default layout).
    let view = match view_for(m.name) {
        None => "null".to_string(),
        Some(v) => {
            let groups: Vec<String> = v
                .groups
                .iter()
                .map(|g| {
                    let slots: Vec<String> = g
                        .fields
                        .iter()
                        .map(|f| format!("{{ \"name\": {}, \"full\": {} }}", json_string(f.name), f.full))
                        .collect();
                    let title = g.title.map(json_string).unwrap_or_else(|| "null".to_string());
                    format!("{{ \"title\": {}, \"fields\": [{}] }}", title, slots.join(", "))
                })
                .collect();
            let pages: Vec<String> = v
                .pages
                .iter()
                .map(|p| {
                    let pf: Vec<String> = p.fields.iter().map(|f| json_string(f)).collect();
                    format!("{{ \"title\": {}, \"fields\": [{}] }}", json_string(p.title), pf.join(", "))
                })
                .collect();
            format!("{{ \"groups\": [{}], \"pages\": [{}] }}", groups.join(", "), pages.join(", "))
        }
    };

    Ok(format!(
        "{{\n  \"model\": {},\n  \"type\": \"form\",\n  \"mailed\": {},\n  \"fields\": [\n{}\n  ],\n  \"list\": {{ \"columns\": [\n{}\n  ] }},\n  \"actions\": [\n{}\n  ],\n  \"reports\": [\n{}\n  ],\n  \"view\": {}\n}}",
        json_string(m.name),
        is_mailed(m.name),
        fields.join(",\n"),
        columns.join(",\n"),
        actions.join(",\n"),
        reports.join(",\n"),
        view
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

    #[test]
    fn on_read_compute_is_contract_readonly_and_a_column() {
        // A non-stored computed field (no DDL column) must still be in the contract — as a readonly
        // form field AND a list column — so the generic FE shows the derived value.
        static M: ModelDescriptor = ModelDescriptor {
            name: "c.demo", table: "c_demo",
            fields: &[
                FieldDef { name: "qty", label: "Qty", kind: FieldKind::Integer, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
                FieldDef { name: "doubled", label: "Doubled", kind: FieldKind::Integer, required: false, stored: false, compute: Some("x"), depends: &["qty"], default: None, unique: false, check: None },
            ],
        };
        let m = resolve(&M, &[]).unwrap();
        // No DDL column for the non-stored compute.
        assert!(!to_ddl(&m).contains("doubled"), "non-stored compute has no column");
        let c = to_ui_contract(&m, &[]).unwrap();
        // Readonly form field.
        assert!(c.contains("\"name\": \"doubled\", \"label\": \"Doubled\", \"widget\": \"integer\", \"required\": false, \"readonly\": true"));
        // And a list column.
        let list = c.split("\"list\"").nth(1).unwrap_or("");
        assert!(list.contains("\"name\": \"doubled\""), "on-read compute is a list column: {list}");
    }
}
