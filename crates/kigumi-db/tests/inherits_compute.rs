//! An on-read (non-stored) compute on an `_inherits` child may depend on a DELEGATED parent field —
//! the value has no column on the child but is read on the record, so the compute sees it. This is
//! exactly product.product.display_name (template `name` + the variant's own `default_code`). The
//! delegated dependency must validate (it has no column) and resolve at read time. Live Postgres.

use kigumi_core::{resolve_registered, Acl, ComputeInput, Ctx, FieldDef, FieldKind, InheritsRegistration, ModelDescriptor, ModelRegistration, Value};
use kigumi_db::Db;
use serde_json::json;

/// display = "<label> [<code>]" — reads the delegated `label` and the child's own `code`.
fn ic_display(i: &ComputeInput) -> Value {
    Value::Str(format!("{} [{}]", i.str("label"), i.str("code")))
}
kigumi_core::inventory::submit! { kigumi_core::ComputeRegistration { name: "ic_display", func: ic_display } }

static TPL: ModelDescriptor = ModelDescriptor {
    name: "ic.tpl",
    table: "ic_tpl",
    fields: &[FieldDef { name: "label", label: "Label", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
};
static VAR: ModelDescriptor = ModelDescriptor {
    name: "ic.var",
    table: "ic_var",
    fields: &[
        FieldDef { name: "tpl_id", label: "Template", kind: FieldKind::Many2one { target: "ic.tpl" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "code", label: "Code", kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        // On-read compute depending on the DELEGATED `label` (no column here) + the own `code`.
        FieldDef { name: "display", label: "Display", kind: FieldKind::Text, required: false, stored: false, compute: Some("ic_display"), depends: &["label", "code"], default: None, unique: false, check: None },
    ],
};
fn tpl() -> &'static ModelDescriptor { &TPL }
fn var() -> &'static ModelDescriptor { &VAR }
kigumi_core::inventory::submit! { ModelRegistration { name: "ic.tpl", module: "test", descriptor: tpl } }
kigumi_core::inventory::submit! { ModelRegistration { name: "ic.var", module: "test", descriptor: var } }
kigumi_core::inventory::submit! { InheritsRegistration { model: "ic.var", parent: "ic.tpl", via: "tpl_id" } }

static ACLS: &[Acl] = &[
    Acl { model: "ic.tpl", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "ic.var", group: "u", read: true, write: true, create: true, delete: true },
];

#[tokio::test]
async fn on_read_compute_reads_a_delegated_field() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let db = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();
    // The model resolves: the delegated `label` is an allowed dependency even though it has no column.
    let tpl = resolve_registered("ic.tpl").unwrap();
    let var = resolve_registered("ic.var").unwrap();

    db.drop_table(&var).await.unwrap();
    db.drop_table(&tpl).await.unwrap();
    db.create_table(&tpl).await.unwrap();
    db.create_table(&var).await.unwrap();

    // Create a variant; the delegated `label` in the payload auto-creates the parent template.
    let id = db.insert_secured(&var, &su, ACLS, &[], json!({ "label": "Shirt", "code": "RED-S" }).as_object().unwrap()).await.unwrap();
    let row = db.find_one_secured(&var, &su, ACLS, &[], id).await.unwrap().unwrap();
    assert_eq!(row["label"].as_str(), Some("Shirt"), "delegated label read transparently");
    assert_eq!(row["display"].as_str(), Some("Shirt [RED-S]"), "on-read compute used the delegated label");

    // Editing the parent label flows into the variant's derived display on the next read.
    let pid = row["tpl_id"].as_i64().unwrap();
    db.update_secured(&tpl, &su, ACLS, &[], pid, json!({ "label": "Blouse" }).as_object().unwrap()).await.unwrap();
    let row = db.find_one_secured(&var, &su, ACLS, &[], id).await.unwrap().unwrap();
    assert_eq!(row["display"].as_str(), Some("Blouse [RED-S]"), "derived from the live delegated value");

    db.drop_table(&var).await.unwrap();
    db.drop_table(&tpl).await.unwrap();
}
