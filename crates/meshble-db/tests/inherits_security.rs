//! `_inherits` security (non-superuser): writing an inherited field is a write to the shared parent,
//! so it needs the PARENT's Write ACL; a field-group restriction on an inherited parent field is
//! enforced transparently on the child for BOTH read (redacted) and write (rejected) via the
//! delegation-aware field_accessible. Live Postgres. Exercises the slice-3 review's HIGH/MEDIUM fixes.

use meshble_core::{
    resolve_registered, Acl, Ctx, FieldDef, FieldGroupRegistration, FieldKind, InheritsRegistration,
    ModelDescriptor, ModelRegistration, ResolvedModel,
};
use meshble_db::{Db, DbError};
use serde_json::json;

static TPL: ModelDescriptor = ModelDescriptor {
    name: "sec.tpl",
    table: "sec_tpl",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "cost", label: "Cost", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
static VAR: ModelDescriptor = ModelDescriptor {
    name: "sec.var",
    table: "sec_var",
    fields: &[
        FieldDef { name: "tpl_id", label: "Template", kind: FieldKind::Many2one { target: "sec.tpl" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "code", label: "Code", kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
fn tpl() -> &'static ModelDescriptor { &TPL }
fn var() -> &'static ModelDescriptor { &VAR }
meshble_core::inventory::submit! { ModelRegistration { name: "sec.tpl", module: "test", descriptor: tpl } }
meshble_core::inventory::submit! { ModelRegistration { name: "sec.var", module: "test", descriptor: var } }
meshble_core::inventory::submit! { InheritsRegistration { model: "sec.var", parent: "sec.tpl", via: "tpl_id" } }
// The parent's `cost` is manager-only (D6). It must stay restricted when read/written THROUGH the variant.
meshble_core::inventory::submit! { FieldGroupRegistration { model: "sec.tpl", field: "cost", groups: &["mgr"] } }

static ACLS: &[Acl] = &[
    // Regular users manage variants and read templates; only managers/editors write templates.
    Acl { model: "sec.var", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "sec.tpl", group: "mgr", read: true, write: true, create: true, delete: true },
    Acl { model: "sec.tpl", group: "editor", read: true, write: true, create: true, delete: true },
];

#[tokio::test]
async fn inherited_field_security_is_enforced_through_the_child() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let (tpl, var): (ResolvedModel, ResolvedModel) =
        (resolve_registered("sec.tpl").unwrap(), resolve_registered("sec.var").unwrap());
    let su = Ctx::new(0, vec![]).sudo();
    let user = Ctx::new(1, vec!["u".into()]); // variant write, NO template write
    let editor = Ctx::new(2, vec!["u".into(), "editor".into()]); // template write, NOT mgr
    let mgr = Ctx::new(3, vec!["u".into(), "mgr".into()]); // template write + cost access

    db.drop_table(&var).await.unwrap();
    db.drop_table(&tpl).await.unwrap();
    db.create_table(&tpl).await.unwrap();
    db.create_table(&var).await.unwrap();

    let t = db.insert_secured(&tpl, &su, ACLS, &[], json!({ "name": "T1", "cost": 7 }).as_object().unwrap()).await.unwrap();
    let v = db.insert_secured(&var, &su, ACLS, &[], json!({ "tpl_id": t, "code": "V1" }).as_object().unwrap()).await.unwrap();

    // READ D6: a non-manager reading the variant sees `name` but NOT the restricted inherited `cost`.
    let row = db.find_one_secured(&var, &user, ACLS, &[], v).await.unwrap().unwrap();
    assert_eq!(row["name"], json!("T1"));
    assert!(!row.as_object().unwrap().contains_key("cost"), "restricted inherited field is redacted for a non-member");
    // A manager sees it.
    let row = db.find_one_secured(&var, &mgr, ACLS, &[], v).await.unwrap().unwrap();
    assert_eq!(row["cost"], json!("7"), "manager sees the inherited restricted field");

    // WRITE parent-ACL: a user with variant write but NO template write cannot write an inherited field.
    let e = db.update_secured(&var, &user, ACLS, &[], v, json!({ "name": "T1b" }).as_object().unwrap()).await;
    assert!(matches!(e, Err(DbError::AccessDenied { .. })), "no template write -> denied: {e:?}");

    // An editor (template write) may write an unrestricted inherited field...
    db.update_secured(&var, &editor, ACLS, &[], v, json!({ "name": "T1c" }).as_object().unwrap()).await.unwrap();
    assert_eq!(db.find_one_secured(&var, &mgr, ACLS, &[], v).await.unwrap().unwrap()["name"], json!("T1c"));
    // ...but NOT the manager-only `cost` (D6 on the inherited field, enforced through the variant).
    let e = db.update_secured(&var, &editor, ACLS, &[], v, json!({ "cost": 9 }).as_object().unwrap()).await;
    assert!(matches!(e, Err(DbError::AccessDenied { .. })), "editor cannot write the mgr-only inherited cost: {e:?}");
    // A manager can.
    db.update_secured(&var, &mgr, ACLS, &[], v, json!({ "cost": 9 }).as_object().unwrap()).await.unwrap();
    assert_eq!(db.find_one_secured(&var, &mgr, ACLS, &[], v).await.unwrap().unwrap()["cost"], json!("9"));

    // Re-parent + inherited write in one call is rejected (ambiguous target).
    let t2 = db.insert_secured(&tpl, &su, ACLS, &[], json!({ "name": "T2" }).as_object().unwrap()).await.unwrap();
    let e = db.update_secured(&var, &mgr, ACLS, &[], v, json!({ "tpl_id": t2, "name": "X" }).as_object().unwrap()).await;
    assert!(matches!(e, Err(DbError::BadInput(_))), "re-parent + inherited write rejected: {e:?}");

    db.drop_table(&var).await.unwrap();
    db.drop_table(&tpl).await.unwrap();
}
