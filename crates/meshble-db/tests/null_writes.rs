//! Writing an explicit NULL to a non-text column (Many2one/Decimal/Bool) must succeed. The bug:
//! a bound NULL was typed `text` by the driver, so `SET fk_id = $n` on a bigint column failed with
//! Postgres 42804. Fixed by casting placeholders to the column type. Live Postgres.

use meshble_core::{resolve, Acl, Ctx, FieldDef, FieldKind, ModelDescriptor, ResolvedModel};
use meshble_db::Db;
use serde_json::json;

static DOC: ModelDescriptor = ModelDescriptor {
    name: "nz.doc",
    table: "nz_doc",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        // Nullable self-referential Many2one (bigint column) — the field that broke.
        FieldDef { name: "ref_id", label: "Ref", kind: FieldKind::Many2one { target: "nz.doc" }, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "amount", label: "Amount", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "flag", label: "Flag", kind: FieldKind::Bool, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
static ACLS: &[Acl] = &[Acl { model: "nz.doc", group: "u", read: true, write: true, create: true, delete: true }];

fn model() -> ResolvedModel {
    resolve(&DOC, &[]).unwrap()
}

#[tokio::test]
async fn explicit_null_writes_to_non_text_columns() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let m = model();
    let su = Ctx::new(0, vec![]).sudo();
    db.drop_table(&m).await.unwrap();
    db.create_table(&m).await.unwrap();

    // CREATE with explicit nulls for the bigint / numeric / boolean columns.
    let a = db.insert_secured(&m, &su, ACLS, &[], json!({ "name": "a", "ref_id": null, "amount": null, "flag": null }).as_object().unwrap()).await.unwrap();
    // CREATE with the relation + values set, then UPDATE each back to NULL (the exact failing path).
    let b = db.insert_secured(&m, &su, ACLS, &[], json!({ "name": "b", "ref_id": a, "amount": 5.5, "flag": true }).as_object().unwrap()).await.unwrap();

    db.update_secured(&m, &su, ACLS, &[], b, json!({ "ref_id": null }).as_object().unwrap()).await.unwrap();
    db.update_secured(&m, &su, ACLS, &[], b, json!({ "amount": null, "flag": null }).as_object().unwrap()).await.unwrap();

    let got = db.find_one_secured(&m, &su, ACLS, &[], b).await.unwrap().unwrap();
    assert!(got["ref_id"].is_null(), "Many2one set back to null");
    assert!(got["amount"].is_null(), "Decimal set back to null");
    assert!(got["flag"].is_null(), "Bool set back to null");

    db.drop_table(&m).await.unwrap();
}
