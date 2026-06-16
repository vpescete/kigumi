//! The projections: from a `ResolvedModel` to Postgres DDL and an agnostic UI contract (JSON).
//!
//! Same source of truth → multiple outputs. Here are two of the design's four projections
//! (DB + UI). API (OpenAPI/GraphQL) and security arrive in phases 4-5.

use meshble_core::{FieldKind, ResolvedModel};

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

/// Agnostic UI contract: JSON consumable by ANY frontend.
/// No interpreted XML, no proprietary framework. Computed fields are readonly.
/// ponytail: JSON built by hand (zero-dep). Switch to serde in phase 5.
pub fn to_ui_contract(m: &ResolvedModel) -> String {
    let fields: Vec<String> = m
        .fields
        .iter()
        .map(|f| {
            format!(
                "    {{ \"name\": \"{}\", \"label\": \"{}\", \"widget\": \"{}\", \"required\": {}, \"readonly\": {} }}",
                f.name, f.label, widget(&f.kind), f.required, f.is_computed()
            )
        })
        .collect();
    format!(
        "{{\n  \"model\": \"{}\",\n  \"type\": \"form\",\n  \"fields\": [\n{}\n  ]\n}}",
        m.name,
        fields.join(",\n")
    )
}
