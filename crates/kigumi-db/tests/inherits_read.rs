//! `_inherits` slice 2: a child transparently READS its parent's delegated fields through the `via`
//! FK. The read uses a correlated subquery per delegated field (reusing the related-field pattern),
//! so find_secured/find_one_secured return the parent's `name`/`list_price` on the child row, with no
//! column for them on the child table. Live Postgres.

use kigumi_core::{
    resolve_registered, Acl, FieldDef, FieldKind, InheritsRegistration, ModelDescriptor,
    ModelRegistration, ResolvedModel,
};
use serde_json::json;

static TPL: ModelDescriptor = ModelDescriptor {
    name: "rd.tpl",
    table: "rd_tpl",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "list_price", label: "Price", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
static VAR: ModelDescriptor = ModelDescriptor {
    name: "rd.var",
    table: "rd_var",
    fields: &[
        FieldDef { name: "tpl_id", label: "Template", kind: FieldKind::Many2one { target: "rd.tpl" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "default_code", label: "Ref", kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
fn tpl() -> &'static ModelDescriptor { &TPL }
fn var() -> &'static ModelDescriptor { &VAR }
kigumi_core::inventory::submit! { ModelRegistration { name: "rd.tpl", module: "test", descriptor: tpl } }
kigumi_core::inventory::submit! { ModelRegistration { name: "rd.var", module: "test", descriptor: var } }
kigumi_core::inventory::submit! { InheritsRegistration { model: "rd.var", parent: "rd.tpl", via: "tpl_id" } }

static ACLS: &[Acl] = &[
    Acl { model: "rd.tpl", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "rd.var", group: "u", read: true, write: true, create: true, delete: true },
];

#[tokio::test]
async fn child_read_exposes_parent_fields_through_via() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let (tpl, var): (ResolvedModel, ResolvedModel) =
        (resolve_registered("rd.tpl").unwrap(), resolve_registered("rd.var").unwrap());
    let su = kigumi_test::su();

    // A template + a variant pointing at it (the variant has only its own fields + the via FK).
    let t = db.insert_secured(&tpl, &su, ACLS, &[], json!({ "name": "Widget", "list_price": 19.99 }).as_object().unwrap()).await.unwrap();
    let v = db.insert_secured(&var, &su, ACLS, &[], json!({ "tpl_id": t, "default_code": "W-RED" }).as_object().unwrap()).await.unwrap();

    // Reading the VARIANT exposes the template's delegated fields transparently.
    let row = db.find_one_secured(&var, &su, ACLS, &[], v).await.unwrap().unwrap();
    assert_eq!(row["default_code"], json!("W-RED"), "own field");
    assert_eq!(row["tpl_id"], json!(t), "via FK");
    assert_eq!(row["name"], json!("Widget"), "delegated parent field");
    assert_eq!(row["list_price"], json!("19.99"), "delegated exact decimal as string");

    // Changing the template is reflected on every variant read (shared, no duplication).
    db.update_secured(&tpl, &su, ACLS, &[], t, json!({ "name": "Widget Pro" }).as_object().unwrap()).await.unwrap();
    let row = db.find_one_secured(&var, &su, ACLS, &[], v).await.unwrap().unwrap();
    assert_eq!(row["name"], json!("Widget Pro"), "delegated read follows the parent");
}
