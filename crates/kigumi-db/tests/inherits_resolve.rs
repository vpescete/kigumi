//! `_inherits` slice 1: resolution + delegated-field discovery + DDL. A child declares a required
//! Many2one `via` to a parent and an InheritsRegistration; the framework validates the declaration
//! (acyclic, valid via, no name collision), exposes the parent's stored scalar fields as delegated,
//! and the child's DDL has the via FK column but NO column for delegated fields (they live on the
//! parent). No live database needed — pure resolution/DDL — but kept in the db tests for proximity.

use kigumi_core::{
    delegated_fields, inherits_of, resolve_registered, FieldDef, FieldKind, InheritsRegistration,
    ModelDescriptor, ModelRegistration,
};
use kigumi_schema::{to_ddl, to_ui_contract};

// Parent: a product template-like model with shared scalar fields + one computed field (NOT delegated).
static TPL: ModelDescriptor = ModelDescriptor {
    name: "inh.tpl",
    table: "inh_tpl",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "list_price", label: "Price", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        // A computed parent field must NOT be delegated.
        FieldDef { name: "display_name", label: "Display", kind: FieldKind::Text, required: false, stored: true, compute: Some("noop"), depends: &[], default: None, unique: false, check: None },
    ],
};
// Child variant: declares the via FK + its own field; inherits the template's scalars.
static VAR: ModelDescriptor = ModelDescriptor {
    name: "inh.var",
    table: "inh_var",
    fields: &[
        FieldDef { name: "tpl_id", label: "Template", kind: FieldKind::Many2one { target: "inh.tpl" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "default_code", label: "Ref", kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
fn tpl() -> &'static ModelDescriptor { &TPL }
fn var() -> &'static ModelDescriptor { &VAR }
kigumi_core::inventory::submit! { ModelRegistration { name: "inh.tpl", module: "test", descriptor: tpl } }
kigumi_core::inventory::submit! { ModelRegistration { name: "inh.var", module: "test", descriptor: var } }
kigumi_core::inventory::submit! { InheritsRegistration { model: "inh.var", parent: "inh.tpl", via: "tpl_id" } }

#[test]
fn child_resolves_and_exposes_delegated_parent_scalars() {
    assert_eq!(inherits_of("inh.var"), Some(("inh.tpl", "tpl_id")));
    assert_eq!(inherits_of("inh.tpl"), None);

    // Resolution succeeds (valid via + no collision).
    let child = resolve_registered("inh.var").unwrap();
    // The child's OWN columns: via FK + default_code (NOT name/list_price — those are the parent's).
    assert!(child.fields.iter().any(|f| f.name == "tpl_id"));
    assert!(child.fields.iter().any(|f| f.name == "default_code"));
    assert!(!child.fields.iter().any(|f| f.name == "name"), "delegated field is not a child column");

    // Delegated fields = parent's stored SCALARS, minus the computed one.
    let deleg = delegated_fields("inh.var").unwrap();
    let names: Vec<&str> = deleg.iter().map(|d| d.def.name).collect();
    assert!(names.contains(&"name"), "name is delegated");
    assert!(names.contains(&"list_price"), "list_price is delegated");
    assert!(!names.contains(&"display_name"), "computed parent field is NOT delegated");
    assert!(deleg.iter().all(|d| d.parent_table == "inh_tpl" && d.via == "tpl_id"));

    // A non-inheriting model has no delegated fields.
    assert!(delegated_fields("inh.tpl").unwrap().is_empty());
}

// --- validation error paths (each registers a deliberately-invalid child) ---
// shadow: own field `name` intentionally shadows the parent's `name` (Odoo behaviour — allowed).
static COLLIDE: ModelDescriptor = ModelDescriptor { name: "inh.collide", table: "inh_collide", fields: &[
    FieldDef { name: "c_id", label: "T", kind: FieldKind::Many2one { target: "inh.tpl" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
] };
// bad via: `b_id` is Text, not a Many2one to the parent.
static BADVIA: ModelDescriptor = ModelDescriptor { name: "inh.badvia", table: "inh_badvia", fields: &[
    FieldDef { name: "b_id", label: "B", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
] };
// cycle: a <-> b inherit each other.
static CYCA: ModelDescriptor = ModelDescriptor { name: "inh.cyca", table: "inh_cyca", fields: &[
    FieldDef { name: "a_id", label: "A", kind: FieldKind::Many2one { target: "inh.cycb" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
] };
static CYCB: ModelDescriptor = ModelDescriptor { name: "inh.cycb", table: "inh_cycb", fields: &[
    FieldDef { name: "b_id", label: "B", kind: FieldKind::Many2one { target: "inh.cyca" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
] };
fn collide() -> &'static ModelDescriptor { &COLLIDE }
fn badvia() -> &'static ModelDescriptor { &BADVIA }
fn cyca() -> &'static ModelDescriptor { &CYCA }
fn cycb() -> &'static ModelDescriptor { &CYCB }
kigumi_core::inventory::submit! { ModelRegistration { name: "inh.collide", module: "test", descriptor: collide } }
kigumi_core::inventory::submit! { ModelRegistration { name: "inh.badvia", module: "test", descriptor: badvia } }
kigumi_core::inventory::submit! { ModelRegistration { name: "inh.cyca", module: "test", descriptor: cyca } }
kigumi_core::inventory::submit! { ModelRegistration { name: "inh.cycb", module: "test", descriptor: cycb } }
kigumi_core::inventory::submit! { InheritsRegistration { model: "inh.collide", parent: "inh.tpl", via: "c_id" } }
kigumi_core::inventory::submit! { InheritsRegistration { model: "inh.badvia", parent: "inh.tpl", via: "b_id" } }
kigumi_core::inventory::submit! { InheritsRegistration { model: "inh.cyca", parent: "inh.cycb", via: "a_id" } }
kigumi_core::inventory::submit! { InheritsRegistration { model: "inh.cycb", parent: "inh.cyca", via: "b_id" } }

#[test]
fn invalid_inherits_declarations_are_rejected() {
    // via field that is not a Many2one to the parent is an error.
    let e = resolve_registered("inh.badvia").unwrap_err();
    assert!(e.contains("Many2one"), "bad-via error: {e}");
    // An inherits cycle is rejected (resolution terminates with a clear error).
    let e = resolve_registered("inh.cyca").unwrap_err();
    assert!(e.contains("cycle"), "cycle error: {e}");
}

#[test]
fn own_field_shadows_inherited_field() {
    // An own field of the same name as a parent's intentionally SHADOWS it (Odoo behaviour): the
    // child resolves, owns its own `name` column, and that name is NOT delegated — while the other
    // parent scalars still are. This is what lets product.product have its own `active`.
    let child = resolve_registered("inh.collide").unwrap();
    assert!(child.fields.iter().any(|f| f.name == "name"), "shadowed field is the child's own column");
    let deleg = delegated_fields("inh.collide").unwrap();
    let names: Vec<&str> = deleg.iter().map(|d| d.def.name).collect();
    assert!(!names.contains(&"name"), "shadowed name is not delegated");
    assert!(names.contains(&"list_price"), "non-shadowed parent scalars are still delegated");
}

#[test]
fn contract_exposes_delegated_fields_as_editable() {
    let child = resolve_registered("inh.var").unwrap();
    let c = to_ui_contract(&child, &[]).unwrap();
    // The inherited parent fields appear in the contract (so the generic form/list shows them)...
    assert!(c.contains("\"name\": \"name\""), "delegated name in contract: {c}");
    assert!(c.contains("\"name\": \"list_price\""), "delegated list_price in contract");
    // ...and they are editable (not read-only): the write-split routes them to the parent.
    assert!(c.contains("\"name\": \"list_price\", \"label\": \"Price\", \"widget\": \"float\", \"required\": false, \"readonly\": false"), "delegated field editable: {c}");
}

#[test]
fn ddl_has_via_fk_but_no_delegated_columns() {
    let child = resolve_registered("inh.var").unwrap();
    let ddl = to_ddl(&child);
    // The via FK is a real column referencing the parent (default ON DELETE = RESTRICT/NO ACTION).
    assert!(ddl.contains("tpl_id bigint REFERENCES inh_tpl(id) NOT NULL"), "via FK column: {ddl}");
    assert!(ddl.contains("default_code text"));
    // Delegated fields are NOT columns on the child (they live on the parent table).
    assert!(!ddl.contains("list_price"), "delegated field must not be a child column: {ddl}");
    assert!(!ddl.contains("\n  name "), "delegated 'name' must not be a child column: {ddl}");
}
