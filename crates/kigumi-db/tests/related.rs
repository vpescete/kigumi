//! Fase A part 2: related fields (Odoo `related=`). A non-stored, read-only field mirrors a value
//! reached by a relational path; it is resolved at read time by a correlated subquery, so it is
//! always fresh (reflects a change to the target without rewriting the record). Live Postgres.

use kigumi_core::{
    resolve, Acl, Ctx, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, RelatedRegistration,
    ResolvedModel,
};
use kigumi_db::Db;
use serde_json::json;

static PARENT: ModelDescriptor = ModelDescriptor {
    name: "rl.parent",
    table: "rl_parent",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "code", label: "Code", kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "due", label: "Due", kind: FieldKind::Date, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
static CHILD: ModelDescriptor = ModelDescriptor {
    name: "rl.child",
    table: "rl_child",
    fields: &[
        FieldDef { name: "parent_id", label: "Parent", kind: FieldKind::Many2one { target: "rl.parent" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        // Related (non-stored): mirror of parent_id.code / parent_id.due.
        FieldDef { name: "parent_code", label: "Parent Code", kind: FieldKind::Text, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "parent_due", label: "Parent Due", kind: FieldKind::Date, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
fn p_desc() -> &'static ModelDescriptor { &PARENT }
fn c_desc() -> &'static ModelDescriptor { &CHILD }
kigumi_core::inventory::submit! { ModelRegistration { name: "rl.parent", module: "test", descriptor: p_desc } }
kigumi_core::inventory::submit! { ModelRegistration { name: "rl.child", module: "test", descriptor: c_desc } }
kigumi_core::inventory::submit! { RelatedRegistration { model: "rl.child", field: "parent_code", path: "parent_id.code" } }
kigumi_core::inventory::submit! { RelatedRegistration { model: "rl.child", field: "parent_due", path: "parent_id.due" } }

static ACLS: &[Acl] = &[
    Acl { model: "rl.parent", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "rl.child", group: "u", read: true, write: true, create: true, delete: true },
];

fn m(d: &'static ModelDescriptor) -> ResolvedModel {
    resolve(d, &[]).unwrap()
}

#[tokio::test]
async fn related_fields_mirror_the_path_and_stay_fresh() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let (parent, child) = (m(&PARENT), m(&CHILD));
    let su = Ctx::new(0, vec![]).sudo();

    db.drop_table(&child).await.unwrap();
    db.drop_table(&parent).await.unwrap();
    db.create_table(&parent).await.unwrap();
    db.create_table(&child).await.unwrap();

    let pid = db.insert_secured(&parent, &su, ACLS, &[], json!({ "name": "P", "code": "ABC", "due": "2026-03-01" }).as_object().unwrap()).await.unwrap();
    let cid = db.insert_secured(&child, &su, ACLS, &[], json!({ "parent_id": pid }).as_object().unwrap()).await.unwrap();

    // The related fields mirror the parent's values.
    let got = db.find_one_secured(&child, &su, ACLS, &[], cid).await.unwrap().unwrap();
    assert_eq!(got["parent_code"], "ABC");
    assert_eq!(got["parent_due"], "2026-03-01");

    // Changing the PARENT is reflected without rewriting the child (non-stored = always fresh).
    db.update_secured(&parent, &su, ACLS, &[], pid, json!({ "code": "XYZ" }).as_object().unwrap()).await.unwrap();
    let fresh = db.find_one_secured(&child, &su, ACLS, &[], cid).await.unwrap().unwrap();
    assert_eq!(fresh["parent_code"], "XYZ", "related field reflects the target change");

    // A related field is READ-ONLY — writing it is rejected (it is not a stored column).
    assert!(
        db.update_secured(&child, &su, ACLS, &[], cid, json!({ "parent_code": "hack" }).as_object().unwrap()).await.is_err(),
        "related fields cannot be written"
    );

    db.drop_table(&child).await.unwrap();
    db.drop_table(&parent).await.unwrap();
}
