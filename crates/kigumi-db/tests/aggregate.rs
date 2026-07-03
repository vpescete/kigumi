//! Aggregate compute end-to-end: an order's `amount_total` = sum of its lines' `price`, and it
//! AUTO-UPDATES when a line is added, edited, or removed (the compute trigger). Live Postgres.

use kigumi_core::{
    resolve, Acl, ComputeInput, FieldDef, FieldKind, ModelDescriptor, ModelRegistration,
    RecordRule, ResolvedModel, Value,
};
use kigumi_db::Db;

static ORDER: ModelDescriptor = ModelDescriptor {
    name: "agg.order",
    table: "agg_order",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "line_ids", label: "Lines", kind: FieldKind::One2many { target: "agg.line", inverse: "order_id" }, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "amount_total", label: "Total", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: Some("agg_total"), depends: &["line_ids.price"], default: None, unique: false, check: None },
    ],
};
static LINE: ModelDescriptor = ModelDescriptor {
    name: "agg.line",
    table: "agg_line",
    fields: &[
        FieldDef { name: "order_id", label: "Order", kind: FieldKind::Many2one { target: "agg.order" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "price", label: "Price", kind: FieldKind::Decimal { currency_field: None }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};

fn order_desc() -> &'static ModelDescriptor {
    &ORDER
}
fn line_desc() -> &'static ModelDescriptor {
    &LINE
}
kigumi_core::inventory::submit! { ModelRegistration { name: "agg.order", module: "test", descriptor: order_desc } }
kigumi_core::inventory::submit! { ModelRegistration { name: "agg.line", module: "test", descriptor: line_desc } }

fn agg_total(i: &ComputeInput) -> Value {
    Value::Decimal(i.sum_decimal("line_ids", "price"))
}
kigumi_core::inventory::submit! { kigumi_core::ComputeRegistration { name: "agg_total", func: agg_total } }

static ACLS: &[Acl] = &[];
static RULES: &[RecordRule] = &[];

fn order_model() -> ResolvedModel {
    resolve(&ORDER, &[]).unwrap()
}
fn line_model() -> ResolvedModel {
    resolve(&LINE, &[]).unwrap()
}

async fn total(db: &Db, order_id: i64) -> f64 {
    sqlx::query_scalar("SELECT amount_total::float8 FROM agg_order WHERE id = $1")
        .bind(order_id)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn aggregate_total_tracks_line_changes() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let order = order_model();
    let line = line_model();
    let su = kigumi_test::su();

    // New order: no lines yet → total 0.
    let oid = db.insert_secured(&order, &su, ACLS, RULES, serde_json::json!({ "name": "O1" }).as_object().unwrap()).await.unwrap();
    assert_eq!(total(db, oid).await, 0.0);

    // Add a line (price 10) → order total recomputes to 10.
    let l1 = db.insert_secured(&line, &su, ACLS, RULES, serde_json::json!({ "order_id": oid, "price": 10.0 }).as_object().unwrap()).await.unwrap();
    assert_eq!(total(db, oid).await, 10.0);

    // Add another line (price 5) → total 15.
    let l2 = db.insert_secured(&line, &su, ACLS, RULES, serde_json::json!({ "order_id": oid, "price": 5.0 }).as_object().unwrap()).await.unwrap();
    assert_eq!(total(db, oid).await, 15.0);

    // Edit the first line to 20 → total 25.
    db.update_secured(&line, &su, ACLS, RULES, l1, serde_json::json!({ "price": 20.0 }).as_object().unwrap()).await.unwrap();
    assert_eq!(total(db, oid).await, 25.0);

    // Remove the second line → total back to 20.
    db.delete_secured(&line, &su, ACLS, RULES, l2).await.unwrap();
    assert_eq!(total(db, oid).await, 20.0);

    // Re-parent the remaining line to a new order B → A drops to 0, B becomes 20 (both recomputed).
    let oid_b = db.insert_secured(&order, &su, ACLS, RULES, serde_json::json!({ "name": "O2" }).as_object().unwrap()).await.unwrap();
    db.update_secured(&line, &su, ACLS, RULES, l1, serde_json::json!({ "order_id": oid_b }).as_object().unwrap()).await.unwrap();
    assert_eq!(total(db, oid).await, 0.0, "old parent recomputed after re-parenting");
    assert_eq!(total(db, oid_b).await, 20.0, "new parent recomputed after re-parenting");
}
