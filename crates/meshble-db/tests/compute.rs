//! Compute engine end-to-end: a stored computed field is derived on insert and recomputed on
//! update, against a live Postgres. Requires `DATABASE_URL`; skipped otherwise.

use meshble_core::{
    resolve, Acl, ComputeInput, Ctx, FieldDef, FieldKind, ModelDescriptor, RecordRule, ResolvedModel,
    Value,
};
use meshble_db::Db;

fn compute_subtotal(i: &ComputeInput) -> Value {
    Value::Decimal(i.decimal("qty") * i.decimal("price"))
}
meshble_core::inventory::submit! {
    meshble_core::ComputeRegistration { name: "compute_subtotal", func: compute_subtotal }
}

static MODEL: ModelDescriptor = ModelDescriptor {
    name: "line",
    table: "compute_line_test",
    fields: &[
        FieldDef { name: "qty", label: "Qty", kind: FieldKind::Decimal { currency_field: None }, required: true, stored: true, compute: None, depends: &[] },
        FieldDef { name: "price", label: "Price", kind: FieldKind::Decimal { currency_field: None }, required: true, stored: true, compute: None, depends: &[] },
        FieldDef { name: "subtotal", label: "Subtotal", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: Some("compute_subtotal"), depends: &["qty", "price"] },
    ],
};
static ACLS: &[Acl] = &[];
static RULES: &[RecordRule] = &[];

fn model() -> ResolvedModel {
    resolve(&MODEL, &[]).unwrap()
}

async fn subtotal(db: &Db, id: i64) -> f64 {
    sqlx::query_scalar("SELECT subtotal::float8 FROM compute_line_test WHERE id = $1")
        .bind(id)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn compute_runs_on_insert_and_update() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let m = model();
    db.drop_table(&m).await.unwrap();
    db.create_table(&m).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();

    // Insert qty=2, price=5 → subtotal is COMPUTED to 10 (not provided by the caller).
    let v = serde_json::json!({ "qty": 2.0, "price": 5.0 });
    let id = db.insert_secured(&m, &su, ACLS, RULES, v.as_object().unwrap()).await.unwrap();
    assert_eq!(subtotal(&db, id).await, 10.0);

    // Update qty=3 → subtotal is RECOMPUTED to 15 from the merged record.
    let upd = serde_json::json!({ "qty": 3.0 });
    assert_eq!(db.update_secured(&m, &su, ACLS, RULES, id, upd.as_object().unwrap()).await.unwrap(), 1);
    assert_eq!(subtotal(&db, id).await, 15.0);

    db.drop_table(&m).await.unwrap();
}
