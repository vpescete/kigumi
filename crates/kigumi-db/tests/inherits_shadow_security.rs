//! D6 field-level security under `_inherits` shadowing. A restriction on a DELEGATED parent field
//! must apply through the child (the value lives on the parent). But a field the child declares as its
//! OWN column SHADOWS the parent's, so the parent's restriction must NOT leak onto the child's
//! independent column — otherwise adding a group to `product.template.active` would silently gate the
//! variant's own `active`. Pure resolution; no database.

use kigumi_core::{
    field_required_groups, FieldDef, FieldGroupRegistration, FieldKind, InheritsRegistration,
    ModelDescriptor, ModelRegistration,
};

const fn txt(name: &'static str) -> FieldDef {
    FieldDef { name, label: name, kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }
}

// Parent: `cost` and `secret` are both restricted to "mgr".
static TPL: ModelDescriptor = ModelDescriptor { name: "sp.tpl", table: "sp_tpl", fields: &[txt("name"), txt("cost"), txt("secret")] };
// Child: declares its OWN `secret` (a shadow) + the required via FK + a plain field. `cost`/`name` are
// delegated (not declared here).
static VAR: ModelDescriptor = ModelDescriptor {
    name: "sp.var",
    table: "sp_var",
    fields: &[
        FieldDef { name: "tpl_id", label: "T", kind: FieldKind::Many2one { target: "sp.tpl" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        txt("secret"),
        txt("code"),
    ],
};
fn tpl() -> &'static ModelDescriptor { &TPL }
fn var() -> &'static ModelDescriptor { &VAR }
kigumi_core::inventory::submit! { ModelRegistration { name: "sp.tpl", module: "test", descriptor: tpl } }
kigumi_core::inventory::submit! { ModelRegistration { name: "sp.var", module: "test", descriptor: var } }
kigumi_core::inventory::submit! { InheritsRegistration { model: "sp.var", parent: "sp.tpl", via: "tpl_id" } }
kigumi_core::inventory::submit! { FieldGroupRegistration { model: "sp.tpl", field: "cost", groups: &["mgr"] } }
kigumi_core::inventory::submit! { FieldGroupRegistration { model: "sp.tpl", field: "secret", groups: &["mgr"] } }

#[test]
fn shadowed_field_does_not_inherit_parent_restriction() {
    // Parent restrictions stand on the parent.
    assert_eq!(field_required_groups("sp.tpl", "cost"), Some(&["mgr"][..]));
    assert_eq!(field_required_groups("sp.tpl", "secret"), Some(&["mgr"][..]));

    // `cost` is genuinely DELEGATED (child has no own column) → the restriction applies through the child.
    assert_eq!(field_required_groups("sp.var", "cost"), Some(&["mgr"][..]), "delegated restricted field inherits");

    // `secret` is SHADOWED (the child owns the column) → it must NOT borrow the parent's restriction.
    assert_eq!(field_required_groups("sp.var", "secret"), None, "shadowed own column is not parent-restricted");

    // A child-own field with no restriction anywhere is unrestricted.
    assert_eq!(field_required_groups("sp.var", "code"), None);
}
