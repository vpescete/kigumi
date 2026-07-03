//! State-transition actions (M4): run_action enforces the guard, applies the field updates, and
//! resolves a sequence assignment (gapless numbering). Live Postgres.

use kigumi_core::{
    resolve, ActionInput, ActionOutcome, FieldDef, FieldKind, ModelDescriptor,
    ModelRegistration, ResolvedModel, Value,
};
use kigumi_db::DbError;

static ORDER: ModelDescriptor = ModelDescriptor {
    name: "act.order",
    table: "act_order",
    fields: &[
        FieldDef { name: "name", label: "Ref", kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "state", label: "State", kind: FieldKind::Selection(&[("draft", "Draft"), ("sale", "Sale"), ("done", "Done")]), required: false, stored: true, compute: None, depends: &[], default: Some("draft"), unique: false, check: None },
    ],
};
fn order_desc() -> &'static ModelDescriptor {
    &ORDER
}
kigumi_core::inventory::submit! { ModelRegistration { name: "act.order", module: "test", descriptor: order_desc } }

/// confirm: draft → sale, and assign the order reference from the "ACT" sequence.
fn confirm(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "draft" => Ok(ActionOutcome::new()
            .set("state", Value::Str("sale".to_string()))
            .assign_sequence("name", "ACT")),
        s => Err(format!("can only confirm a draft (state is '{s}')")),
    }
}
kigumi_core::inventory::submit! { kigumi_core::ActionRegistration { model: "act.order", name: "confirm", func: confirm, groups: &[] } }

fn model() -> ResolvedModel {
    resolve(&ORDER, &[]).unwrap()
}

#[tokio::test]
async fn confirm_action_transitions_and_numbers() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let m = model();
    let su = kigumi_test::su();
    db.ensure_sequence("ACT", "ACT/", "", 4).await.unwrap();

    // Create → state defaults to draft.
    let id = db.insert_secured(&m, &su, &[], &[], serde_json::json!({ "name": "draft-x" }).as_object().unwrap()).await.unwrap();
    assert_eq!(db.find_one_secured(&m, &su, &[], &[], id).await.unwrap().unwrap()["state"], "draft");

    // confirm: draft → sale, name assigned from the sequence.
    db.run_action(&m, &su, &[], &[], id, "confirm").await.unwrap();
    let got = db.find_one_secured(&m, &su, &[], &[], id).await.unwrap().unwrap();
    assert_eq!(got["state"], "sale", "transitioned to sale");
    assert!(got["name"].as_str().unwrap().starts_with("ACT/"), "assigned an SO-style number, got {:?}", got["name"]);

    // Re-running the guard fails (no longer a draft).
    assert!(matches!(db.run_action(&m, &su, &[], &[], id, "confirm").await, Err(DbError::BadInput(_))), "guard rejects a non-draft");

    // An unknown action is a bad request.
    assert!(db.run_action(&m, &su, &[], &[], id, "nope").await.is_err(), "unknown action rejected");
}
