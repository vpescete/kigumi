//! Le proiezioni: da un `ResolvedModel` a DDL Postgres e contratto-UI agnostico (JSON).
//!
//! Stessa sorgente di verità → più output. Qui due delle quattro proiezioni del design
//! (DB + UI). API (OpenAPI/GraphQL) e security arrivano alle fasi 4-5.

use meshble_core::{FieldKind, ResolvedModel};

/// Tipo SQL Postgres per un campo con colonna.
fn pg_type(kind: &FieldKind) -> &'static str {
    match kind {
        FieldKind::Text | FieldKind::Selection(_) => "text",
        FieldKind::Integer => "bigint",
        FieldKind::Decimal { .. } => "numeric",
        FieldKind::Bool => "boolean",
        FieldKind::Many2one { .. } => "bigint",
        FieldKind::One2many { .. } => "", // nessuna colonna
    }
}

fn table_of(dotted: &str) -> String {
    dotted.replace('.', "_")
}

/// Genera il `CREATE TABLE`. Computed non-stored e one2many non producono colonne.
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

/// Widget UI suggerito dal tipo del campo (il frontend è libero di sovrascriverlo).
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

/// Contratto-UI agnostico: JSON consumabile da QUALSIASI frontend.
/// Niente XML interpretato, niente framework proprietario. I computed sono readonly.
/// ponytail: JSON costruito a mano (zero-dep). Passa a serde alla fase 5.
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
