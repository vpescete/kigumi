//! Multi-level aggregate cascade (M4): a line change recomputes its order's amount AND the order's
//! group total (line → order → group), two levels up. Live Postgres.

use kigumi_core::{
    resolve, ComputeInput, FieldDef, FieldKind, ModelDescriptor, ModelRegistration,
    ResolvedModel, Value,
};
use kigumi_db::Db;

static GROUP: ModelDescriptor = ModelDescriptor {
    name: "cas.group",
    table: "cas_group",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "order_ids", label: "Orders", kind: FieldKind::One2many { target: "cas.order", inverse: "group_id" }, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "total", label: "Total", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: Some("cas_group_total"), depends: &["order_ids.amount"], default: None, unique: false, check: None },
    ],
};
static ORDER: ModelDescriptor = ModelDescriptor {
    name: "cas.order",
    table: "cas_order",
    fields: &[
        FieldDef { name: "group_id", label: "Group", kind: FieldKind::Many2one { target: "cas.group" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "line_ids", label: "Lines", kind: FieldKind::One2many { target: "cas.line", inverse: "order_id" }, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "amount", label: "Amount", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: Some("cas_order_amount"), depends: &["line_ids.price"], default: None, unique: false, check: None },
    ],
};
static LINE: ModelDescriptor = ModelDescriptor {
    name: "cas.line",
    table: "cas_line",
    fields: &[
        FieldDef { name: "order_id", label: "Order", kind: FieldKind::Many2one { target: "cas.order" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "price", label: "Price", kind: FieldKind::Decimal { currency_field: None }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
fn g_desc() -> &'static ModelDescriptor { &GROUP }
fn o_desc() -> &'static ModelDescriptor { &ORDER }
fn l_desc() -> &'static ModelDescriptor { &LINE }
kigumi_core::inventory::submit! { ModelRegistration { name: "cas.group", module: "test", descriptor: g_desc } }
kigumi_core::inventory::submit! { ModelRegistration { name: "cas.order", module: "test", descriptor: o_desc } }
kigumi_core::inventory::submit! { ModelRegistration { name: "cas.line", module: "test", descriptor: l_desc } }

fn order_amount(i: &ComputeInput) -> Value {
    Value::Decimal(i.sum_decimal("line_ids", "price"))
}
fn group_total(i: &ComputeInput) -> Value {
    Value::Decimal(i.sum_decimal("order_ids", "amount"))
}
kigumi_core::inventory::submit! { kigumi_core::ComputeRegistration { name: "cas_order_amount", func: order_amount } }
kigumi_core::inventory::submit! { kigumi_core::ComputeRegistration { name: "cas_group_total", func: group_total } }

fn m(d: &'static ModelDescriptor) -> ResolvedModel {
    resolve(d, &[]).unwrap()
}

async fn group_total_of(db: &Db, gid: i64) -> f64 {
    sqlx::query_scalar("SELECT total::float8 FROM cas_group WHERE id = $1")
        .bind(gid)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn line_change_cascades_to_group_total() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let (group, order, line) = (m(&GROUP), m(&ORDER), m(&LINE));
    let su = kigumi_test::su();

    let g = db.insert_secured(&group, &su, &[], &[], serde_json::json!({ "name": "G" }).as_object().unwrap()).await.unwrap();
    let o = db.insert_secured(&order, &su, &[], &[], serde_json::json!({ "group_id": g }).as_object().unwrap()).await.unwrap();
    assert_eq!(group_total_of(db, g).await, 0.0);

    // Add a line (10) → order.amount = 10 → group.total cascades to 10.
    let l1 = db.insert_secured(&line, &su, &[], &[], serde_json::json!({ "order_id": o, "price": 10.0 }).as_object().unwrap()).await.unwrap();
    assert_eq!(group_total_of(db, g).await, 10.0, "line → order → group cascade on insert");

    // Add another line (5) → 15 cascades up.
    let _l2 = db.insert_secured(&line, &su, &[], &[], serde_json::json!({ "order_id": o, "price": 5.0 }).as_object().unwrap()).await.unwrap();
    assert_eq!(group_total_of(db, g).await, 15.0);

    // Edit the first line → cascade on update.
    db.update_secured(&line, &su, &[], &[], l1, serde_json::json!({ "price": 20.0 }).as_object().unwrap()).await.unwrap();
    assert_eq!(group_total_of(db, g).await, 25.0, "cascade on update");

    // Delete a line → cascade on delete.
    db.delete_secured(&line, &su, &[], &[], l1).await.unwrap();
    assert_eq!(group_total_of(db, g).await, 5.0, "cascade on delete");
}
