//! Multi-company isolation: a caller scoped to company A sees/edits only A's rows (plus shared
//! NULL-company rows); an unscoped caller (empty allowed set) and sudo see everything; create
//! defaults company_id to the caller's active company. Live Postgres.

use kigumi_core::{
    resolve, Acl, Ctx, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, RecordRule,
};

static COMPANY: ModelDescriptor = ModelDescriptor {
    name: "mc.company",
    table: "mc_company",
    fields: &[FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
};
static DOC: ModelDescriptor = ModelDescriptor {
    name: "mc.doc",
    table: "mc_doc",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        // The company-scoping field: a Many2one named exactly `company_id`.
        FieldDef { name: "company_id", label: "Company", kind: FieldKind::Many2one { target: "mc.company" }, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
fn company_desc() -> &'static ModelDescriptor {
    &COMPANY
}
fn doc_desc() -> &'static ModelDescriptor {
    &DOC
}
kigumi_core::inventory::submit! { ModelRegistration { name: "mc.company", module: "test", descriptor: company_desc } }
kigumi_core::inventory::submit! { ModelRegistration { name: "mc.doc", module: "test", descriptor: doc_desc } }

static ACLS: &[Acl] = &[Acl { model: "mc.doc", group: "u", read: true, write: true, create: true, delete: true }];
static RULES: &[RecordRule] = &[];

fn names(rows: &[serde_json::Value]) -> Vec<String> {
    let mut v: Vec<String> = rows.iter().map(|r| r["name"].as_str().unwrap().to_string()).collect();
    v.sort();
    v
}

#[tokio::test]
async fn company_scope_isolates_rows() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let company = resolve(&COMPANY, &[]).unwrap();
    let doc = resolve(&DOC, &[]).unwrap();
    let su = kigumi_test::su();

    // Two companies.
    let c1 = db.insert_secured(&company, &su, ACLS, RULES, serde_json::json!({ "name": "C1" }).as_object().unwrap()).await.unwrap();
    let c2 = db.insert_secured(&company, &su, ACLS, RULES, serde_json::json!({ "name": "C2" }).as_object().unwrap()).await.unwrap();

    // A doc in each company + one shared (NULL company).
    let _d1 = db.insert_secured(&doc, &su, ACLS, RULES, serde_json::json!({ "name": "in-c1", "company_id": c1 }).as_object().unwrap()).await.unwrap();
    let d2 = db.insert_secured(&doc, &su, ACLS, RULES, serde_json::json!({ "name": "in-c2", "company_id": c2 }).as_object().unwrap()).await.unwrap();
    let _shared = db.insert_secured(&doc, &su, ACLS, RULES, serde_json::json!({ "name": "shared" }).as_object().unwrap()).await.unwrap();

    // Caller scoped to C1 sees its own + the shared row, never C2's.
    let in_c1 = Ctx::new(1, vec!["u".to_string()]).in_companies(c1, vec![c1]);
    let rows = db.find_secured(&doc, &in_c1, ACLS, RULES, None).await.unwrap();
    assert_eq!(names(&rows), vec!["in-c1", "shared"], "C1 caller sees only C1 + shared");
    assert_eq!(db.count_secured(&doc, &in_c1, ACLS, RULES, None).await.unwrap(), 2, "count is company-scoped too");

    // M7 default-deny: an unassigned (empty allowed) non-su caller sees ONLY shared (NULL-company)
    // rows — never everything. Only sudo is unrestricted.
    let unscoped = Ctx::new(1, vec!["u".to_string()]);
    assert_eq!(
        names(&db.find_secured(&doc, &unscoped, ACLS, RULES, None).await.unwrap()),
        vec!["shared"],
        "unassigned caller sees only the shared row"
    );
    assert_eq!(db.find_secured(&doc, &su, ACLS, RULES, None).await.unwrap().len(), 3, "sudo sees all");
    // ...and an unassigned caller cannot CREATE a company-scoped row (no active company to assign).
    assert!(
        db.insert_secured(&doc, &unscoped, ACLS, RULES, serde_json::json!({ "name": "x" }).as_object().unwrap()).await.is_err(),
        "unassigned caller cannot create a company-scoped row"
    );

    // find_one across the boundary is invisible; update/delete are no-ops.
    assert!(db.find_one_secured(&doc, &in_c1, ACLS, RULES, d2).await.unwrap().is_none(), "C2 doc not readable by C1");
    let upd = db.update_secured(&doc, &in_c1, ACLS, RULES, d2, serde_json::json!({ "name": "hacked" }).as_object().unwrap()).await.unwrap();
    assert_eq!(upd, 0, "cannot update another company's row");
    let del = db.delete_secured(&doc, &in_c1, ACLS, RULES, d2).await.unwrap();
    assert_eq!(del, 0, "cannot delete another company's row");

    // Create defaults company_id to the caller's active company.
    let new_id = db.insert_secured(&doc, &in_c1, ACLS, RULES, serde_json::json!({ "name": "new-in-c1" }).as_object().unwrap()).await.unwrap();
    let got = db.find_one_secured(&doc, &su, ACLS, RULES, new_id).await.unwrap().unwrap();
    assert_eq!(got["company_id"].as_i64().unwrap(), c1, "create defaulted company_id to the active company");

    // WRITE-SIDE enforcement (audit fixes): a scoped caller cannot write a foreign or NULL company.
    let foreign = serde_json::json!({ "name": "x", "company_id": c2 });
    assert!(db.insert_secured(&doc, &in_c1, ACLS, RULES, foreign.as_object().unwrap()).await.is_err(), "create with foreign company rejected");
    let shared = serde_json::json!({ "name": "x", "company_id": null });
    assert!(db.insert_secured(&doc, &in_c1, ACLS, RULES, shared.as_object().unwrap()).await.is_err(), "create with NULL (shared) company rejected");
    assert!(db.update_secured(&doc, &in_c1, ACLS, RULES, new_id, serde_json::json!({ "company_id": c2 }).as_object().unwrap()).await.is_err(), "reassign own row to a foreign company rejected");
    assert!(db.update_secured(&doc, &in_c1, ACLS, RULES, new_id, serde_json::json!({ "company_id": null }).as_object().unwrap()).await.is_err(), "demote own row to shared rejected");
    // Editing other fields (or re-stating the SAME company) stays allowed.
    assert_eq!(db.update_secured(&doc, &in_c1, ACLS, RULES, new_id, serde_json::json!({ "name": "renamed" }).as_object().unwrap()).await.unwrap(), 1);
    assert_eq!(db.update_secured(&doc, &in_c1, ACLS, RULES, new_id, serde_json::json!({ "company_id": c1 }).as_object().unwrap()).await.unwrap(), 1, "writing the caller's own company is fine");
}
