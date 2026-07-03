//! On-read (non-stored) compute: a field with `compute=` but no `store` has NO column and is derived
//! on every read (Odoo `compute=` without `store=True`). It must appear in the projection with the
//! computed value, never be a column, and reject writes (it is computed, not stored). Live Postgres.

use kigumi_core::{resolve, Acl, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, Value};
use serde_json::json;

// `qty_doubled` reads the record's own `qty`; `label` builds a string from it. Both on-read.
fn orc_double(i: &kigumi_core::ComputeInput) -> Value {
    Value::Int(i.int("qty") * 2)
}
fn orc_label(i: &kigumi_core::ComputeInput) -> Value {
    Value::Str(format!("qty={}", i.int("qty")))
}
kigumi_core::inventory::submit! { kigumi_core::ComputeRegistration { name: "orc_double", func: orc_double } }
kigumi_core::inventory::submit! { kigumi_core::ComputeRegistration { name: "orc_label", func: orc_label } }

static ITEM: ModelDescriptor = ModelDescriptor {
    name: "orc.item",
    table: "orc_item",
    fields: &[
        FieldDef { name: "qty", label: "Qty", kind: FieldKind::Integer, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        // Non-stored computes: stored=false, compute=Some, no column.
        FieldDef { name: "qty_doubled", label: "Doubled", kind: FieldKind::Integer, required: false, stored: false, compute: Some("orc_double"), depends: &["qty"], default: None, unique: false, check: None },
        FieldDef { name: "label", label: "Label", kind: FieldKind::Text, required: false, stored: false, compute: Some("orc_label"), depends: &["qty"], default: None, unique: false, check: None },
    ],
};
fn item() -> &'static ModelDescriptor { &ITEM }
kigumi_core::inventory::submit! { ModelRegistration { name: "orc.item", module: "test", descriptor: item } }

static ACLS: &[Acl] = &[Acl { model: "orc.item", group: "u", read: true, write: true, create: true, delete: true }];

#[tokio::test]
async fn non_stored_compute_is_derived_on_read_only() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let m = resolve(&ITEM, &[]).unwrap();
    let su = kigumi_test::su();

    // The on-read fields are NOT columns: a raw select of qty_doubled fails (column does not exist).
    let raw = sqlx::query("SELECT qty_doubled FROM orc_item").fetch_all(db.pool()).await;
    assert!(raw.is_err(), "non-stored compute has no column");

    let id = db.insert_secured(&m, &su, ACLS, &[], json!({ "qty": 21 }).as_object().unwrap()).await.unwrap();

    // On read, the computes are derived from the row.
    let row = db.find_one_secured(&m, &su, ACLS, &[], id).await.unwrap().unwrap();
    assert_eq!(row["qty"].as_i64(), Some(21));
    assert_eq!(row["qty_doubled"].as_i64(), Some(42), "derived on read");
    assert_eq!(row["label"].as_str(), Some("qty=21"));

    // A list read derives them too, and re-derives after an update (no stale stored value).
    db.update_secured(&m, &su, ACLS, &[], id, json!({ "qty": 5 }).as_object().unwrap()).await.unwrap();
    let page = db.list_secured(&m, &su, ACLS, &[], None, &[], 10, 0).await.unwrap();
    assert_eq!(page.data[0]["qty_doubled"].as_i64(), Some(10), "re-derived after update");

    // Writing a computed field is rejected (it is derived, not stored).
    assert!(
        db.update_secured(&m, &su, ACLS, &[], id, json!({ "qty_doubled": 999 }).as_object().unwrap()).await.is_err(),
        "cannot write a computed field"
    );
}
