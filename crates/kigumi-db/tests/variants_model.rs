//! Slice 1 of the variant engine is pure data model. The one integration risk it introduces is a
//! model carrying TWO Many2many fields (the real `product.product` now has `tag_ids` AND
//! `product_template_attribute_value_ids`) — `many2many.rs` only ever exercised one. This test mirrors
//! that shape with a synthetic `va_item` (two junctions) and asserts each relation reads/writes
//! independently, plus that create is denied to a group without the create ACL. Live Postgres.

use kigumi_core::{resolve, Acl, Ctx, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, ResolvedModel};
use serde_json::json;

static TAG: ModelDescriptor = ModelDescriptor {
    name: "va.tag",
    table: "va_tag",
    fields: &[FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
};
static CAT: ModelDescriptor = ModelDescriptor {
    name: "va.cat",
    table: "va_cat",
    fields: &[FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
};
static ITEM: ModelDescriptor = ModelDescriptor {
    name: "va.item",
    table: "va_item",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "tag_ids", label: "Tags", kind: FieldKind::Many2many { target: "va.tag", relation: "va_item_tag_rel", column: "item_id", target_column: "tag_id" }, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "cat_ids", label: "Cats", kind: FieldKind::Many2many { target: "va.cat", relation: "va_item_cat_rel", column: "item_id", target_column: "cat_id" }, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
fn tag_desc() -> &'static ModelDescriptor { &TAG }
fn cat_desc() -> &'static ModelDescriptor { &CAT }
fn item_desc() -> &'static ModelDescriptor { &ITEM }
kigumi_core::inventory::submit! { ModelRegistration { name: "va.tag", module: "test", descriptor: tag_desc } }
kigumi_core::inventory::submit! { ModelRegistration { name: "va.cat", module: "test", descriptor: cat_desc } }
kigumi_core::inventory::submit! { ModelRegistration { name: "va.item", module: "test", descriptor: item_desc } }

// `mgr` maintains items; `usr` may only read (no create) — mirrors product attribute ACLs.
static ACLS: &[Acl] = &[
    Acl { model: "va.tag", group: "mgr", read: true, write: true, create: true, delete: true },
    Acl { model: "va.cat", group: "mgr", read: true, write: true, create: true, delete: true },
    Acl { model: "va.item", group: "mgr", read: true, write: true, create: true, delete: true },
    Acl { model: "va.item", group: "usr", read: true, write: false, create: false, delete: false },
];

fn m(d: &'static ModelDescriptor) -> ResolvedModel {
    resolve(d, &[]).unwrap()
}
// Sorted so the assertion tests membership, not array_agg's (unordered) read-back order.
fn ids(v: &serde_json::Value, field: &str) -> Vec<i64> {
    let mut out: Vec<i64> = v[field].as_array().unwrap().iter().map(|x| x.as_i64().unwrap()).collect();
    out.sort_unstable();
    out
}

#[tokio::test]
async fn two_many2many_on_one_model_are_independent_and_acl_gated() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let (tag, cat, item) = (m(&TAG), m(&CAT), m(&ITEM));
    let su = kigumi_test::su();

    let t1 = db.insert_secured(&tag, &su, ACLS, &[], json!({ "name": "t1" }).as_object().unwrap()).await.unwrap();
    let t2 = db.insert_secured(&tag, &su, ACLS, &[], json!({ "name": "t2" }).as_object().unwrap()).await.unwrap();
    let c1 = db.insert_secured(&cat, &su, ACLS, &[], json!({ "name": "c1" }).as_object().unwrap()).await.unwrap();
    let c2 = db.insert_secured(&cat, &su, ACLS, &[], json!({ "name": "c2" }).as_object().unwrap()).await.unwrap();

    // Create with BOTH relations set; each reads back independently.
    let id = db.insert_secured(&item, &su, ACLS, &[], json!({ "name": "i", "tag_ids": [t1, t2], "cat_ids": [c1] }).as_object().unwrap()).await.unwrap();
    let row = db.find_one_secured(&item, &su, ACLS, &[], id).await.unwrap().unwrap();
    assert_eq!(ids(&row, "tag_ids"), vec![t1, t2]);
    assert_eq!(ids(&row, "cat_ids"), vec![c1]);

    // Writing one relation leaves the other untouched (no cross-talk between the two junctions).
    db.update_secured(&item, &su, ACLS, &[], id, json!({ "cat_ids": [c1, c2] }).as_object().unwrap()).await.unwrap();
    let row = db.find_one_secured(&item, &su, ACLS, &[], id).await.unwrap().unwrap();
    assert_eq!(ids(&row, "tag_ids"), vec![t1, t2], "tags unchanged when only cats written");
    assert_eq!(ids(&row, "cat_ids"), vec![c1, c2]);

    // ACL: a read-only group cannot create.
    let reader = Ctx::new(7, vec!["usr".into()]);
    assert!(
        db.insert_secured(&item, &reader, ACLS, &[], json!({ "name": "nope" }).as_object().unwrap()).await.is_err(),
        "create denied without the create ACL"
    );
}
