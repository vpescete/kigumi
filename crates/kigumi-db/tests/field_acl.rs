//! D6 field-level security: a field restricted by `#[field(groups=...)]` (here registered directly)
//! is gated on BOTH read and write at the DB boundary — non-members never see it and cannot write
//! it; members do; superuser bypasses. Mirrors Odoo's `Field.groups`. Live Postgres.

use kigumi_core::{
    resolve, Acl, Ctx, Domain, FieldDef, FieldGroupRegistration, FieldKind, ModelDescriptor,
    ModelRegistration, ResolvedModel,
};
use serde_json::json;

static DOC: ModelDescriptor = ModelDescriptor {
    name: "fa.doc",
    table: "fa_doc",
    fields: &[
        FieldDef { name: "title", label: "Title", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "secret", label: "Secret", kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
fn doc_desc() -> &'static ModelDescriptor {
    &DOC
}
kigumi_core::inventory::submit! { ModelRegistration { name: "fa.doc", module: "test", descriptor: doc_desc } }
// `secret` requires the "mgr" group (what `#[field(groups = "mgr")]` would emit).
kigumi_core::inventory::submit! { FieldGroupRegistration { model: "fa.doc", field: "secret", groups: &["mgr"] } }

static ACLS: &[Acl] = &[Acl { model: "fa.doc", group: "u", read: true, write: true, create: true, delete: true }];

fn model() -> ResolvedModel {
    resolve(&DOC, &[]).unwrap()
}

// Both tests touch the shared `fa_doc` table; the kit's advisory lock (held by each TestDb for the
// test's lifetime) serializes them.

// --- Models for the two audit-found holes: a restricted One2many, and a relation to fa.doc. ---
static PAR: ModelDescriptor = ModelDescriptor {
    name: "fa.par",
    table: "fa_par",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "lines", label: "Lines", kind: FieldKind::One2many { target: "fa.lin", inverse: "par_id" }, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
static LIN: ModelDescriptor = ModelDescriptor {
    name: "fa.lin",
    table: "fa_lin",
    fields: &[
        FieldDef { name: "par_id", label: "Parent", kind: FieldKind::Many2one { target: "fa.par" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "note", label: "Note", kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
static REFM: ModelDescriptor = ModelDescriptor {
    name: "fa.ref",
    table: "fa_ref",
    fields: &[
        FieldDef { name: "tag", label: "Tag", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "doc_id", label: "Doc", kind: FieldKind::Many2one { target: "fa.doc" }, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
fn par_desc() -> &'static ModelDescriptor { &PAR }
fn lin_desc() -> &'static ModelDescriptor { &LIN }
fn ref_desc() -> &'static ModelDescriptor { &REFM }
kigumi_core::inventory::submit! { ModelRegistration { name: "fa.par", module: "test", descriptor: par_desc } }
kigumi_core::inventory::submit! { ModelRegistration { name: "fa.lin", module: "test", descriptor: lin_desc } }
kigumi_core::inventory::submit! { ModelRegistration { name: "fa.ref", module: "test", descriptor: ref_desc } }
// The One2many `lines` field is itself manager-only (D6 restriction on a relation field).
kigumi_core::inventory::submit! { FieldGroupRegistration { model: "fa.par", field: "lines", groups: &["mgr"] } }

static ACLS2: &[Acl] = &[
    Acl { model: "fa.par", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "fa.lin", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "fa.ref", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "fa.doc", group: "u", read: true, write: true, create: true, delete: true },
];

#[tokio::test]
async fn field_groups_gate_read_and_write() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let m = model();
    let su = kigumi_test::su();
    let clerk = Ctx::new(1, vec!["u".to_string()]); // not a manager
    let mgr = Ctx::new(2, vec!["u".to_string(), "mgr".to_string()]);

    let id = db.insert_secured(&m, &su, ACLS, &[], json!({ "title": "t", "secret": "s" }).as_object().unwrap()).await.unwrap();

    // READ: clerk sees title but NOT secret; manager and su see secret.
    let c = db.find_one_secured(&m, &clerk, ACLS, &[], id).await.unwrap().unwrap();
    assert_eq!(c["title"], "t");
    assert!(c.get("secret").is_none(), "restricted field hidden from a non-member");
    assert_eq!(db.find_one_secured(&m, &mgr, ACLS, &[], id).await.unwrap().unwrap()["secret"], "s", "member sees it");
    assert_eq!(db.find_one_secured(&m, &su, ACLS, &[], id).await.unwrap().unwrap()["secret"], "s", "su sees it");
    // list_secured strips too.
    assert!(db.find_secured(&m, &clerk, ACLS, &[], None).await.unwrap()[0].get("secret").is_none());

    // WRITE: clerk cannot write secret, but can write title; manager can write secret.
    assert!(
        db.update_secured(&m, &clerk, ACLS, &[], id, json!({ "secret": "x" }).as_object().unwrap()).await.is_err(),
        "non-member write to a restricted field is rejected"
    );
    assert_eq!(db.update_secured(&m, &clerk, ACLS, &[], id, json!({ "title": "t2" }).as_object().unwrap()).await.unwrap(), 1);
    assert_eq!(db.update_secured(&m, &mgr, ACLS, &[], id, json!({ "secret": "s2" }).as_object().unwrap()).await.unwrap(), 1);

    // CREATE: clerk cannot create WITH the restricted field, but can without it.
    assert!(
        db.insert_secured(&m, &clerk, ACLS, &[], json!({ "title": "n", "secret": "z" }).as_object().unwrap()).await.is_err(),
        "non-member create touching a restricted field is rejected"
    );
    assert!(db.insert_secured(&m, &clerk, ACLS, &[], json!({ "title": "ok" }).as_object().unwrap()).await.is_ok());

    // PROBE: a non-member cannot FILTER on the restricted field (else they could extract its values),
    // nor ORDER BY it; a member can filter on it.
    let probe = Domain::field("secret").eq("s2");
    assert!(db.find_secured(&m, &clerk, ACLS, &[], Some(&probe)).await.is_err(), "non-member cannot filter on a restricted field");
    assert!(db.find_secured(&m, &mgr, ACLS, &[], Some(&probe)).await.is_ok(), "member can filter on it");
    assert!(
        db.list_secured(&m, &clerk, ACLS, &[], None, &[("secret".to_string(), false)], 10, 0).await.is_err(),
        "non-member cannot order by a restricted field"
    );
}

#[tokio::test]
async fn restricted_relation_and_dotted_filter_are_blocked() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let (par, _lin, refm, doc) =
        (resolve(&PAR, &[]).unwrap(), resolve(&LIN, &[]).unwrap(), resolve(&REFM, &[]).unwrap(), resolve(&DOC, &[]).unwrap());
    let su = kigumi_test::su();
    let clerk = Ctx::new(1, vec!["u".to_string()]);
    let mgr = Ctx::new(2, vec!["u".to_string(), "mgr".to_string()]);

    let did = db.insert_secured(&doc, &su, ACLS2, &[], json!({ "title": "t", "secret": "s" }).as_object().unwrap()).await.unwrap();
    let pid = db.insert_secured(&par, &su, ACLS2, &[], json!({ "name": "p", "lines": [{ "note": "n" }] }).as_object().unwrap()).await.unwrap();
    db.insert_secured(&refm, &su, ACLS2, &[], json!({ "tag": "r", "doc_id": did }).as_object().unwrap()).await.unwrap();

    // HOLE 1: the restricted One2many relation is omitted for a non-member, present for a manager.
    let c = db.find_one_secured(&par, &clerk, ACLS2, &[], pid).await.unwrap().unwrap();
    assert!(c.get("lines").is_none(), "restricted One2many relation hidden from a non-member");
    let g = db.find_one_secured(&par, &mgr, ACLS2, &[], pid).await.unwrap().unwrap();
    assert!(g["lines"].is_array(), "manager sees the relation");

    // HOLE 2: probing fa.doc.secret via a dotted relational filter is blocked for a non-member.
    let probe = Domain::field("doc_id.secret").eq("s");
    assert!(db.find_secured(&refm, &clerk, ACLS2, &[], Some(&probe)).await.is_err(), "non-member cannot probe a restricted field through a relation");
    assert!(db.find_secured(&refm, &mgr, ACLS2, &[], Some(&probe)).await.is_ok(), "manager can");
    // ...but an UNrestricted dotted path is fine for the non-member.
    let ok_probe = Domain::field("doc_id.title").eq("t");
    assert!(db.find_secured(&refm, &clerk, ACLS2, &[], Some(&ok_probe)).await.is_ok(), "unrestricted relational filter allowed");
}
