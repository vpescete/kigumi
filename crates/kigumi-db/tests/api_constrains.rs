//! @api.constrains: a cross-record constraint runs IN the write transaction after the record and its
//! One2many children are written, and rejects (rolls back) the write if violated. The canonical case:
//! a header whose total must equal the sum of its lines — an invariant a single-row SQL CHECK cannot
//! express. Live Postgres.

use kigumi_core::{resolve, Acl, ComputeInput, Ctx, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, ResolvedModel};
use kigumi_db::Db;
use serde_json::json;

/// The header total must equal the sum of its line amounts.
fn balanced(i: &ComputeInput) -> Result<(), String> {
    let total = i.int("header_total") as f64;
    let sum = i.sum_float("line_ids", "amount");
    if (sum - total).abs() > 1e-9 {
        Err(format!("unbalanced: lines sum {sum} != header {total}"))
    } else {
        Ok(())
    }
}
kigumi_core::inventory::submit! {
    kigumi_core::ConstraintRegistration { model: "con.order", fields: &["header_total", "line_ids"], func: balanced }
}

static ORDER: ModelDescriptor = ModelDescriptor {
    name: "con.order",
    table: "con_order",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "header_total", label: "Total", kind: FieldKind::Integer, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "line_ids", label: "Lines", kind: FieldKind::One2many { target: "con.line", inverse: "order_id" }, required: false, stored: false, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
static LINE: ModelDescriptor = ModelDescriptor {
    name: "con.line",
    table: "con_line",
    fields: &[
        FieldDef { name: "order_id", label: "Order", kind: FieldKind::Many2one { target: "con.order" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "amount", label: "Amount", kind: FieldKind::Integer, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
fn order_d() -> &'static ModelDescriptor { &ORDER }
fn line_d() -> &'static ModelDescriptor { &LINE }
kigumi_core::inventory::submit! { ModelRegistration { name: "con.order", module: "test", descriptor: order_d } }
kigumi_core::inventory::submit! { ModelRegistration { name: "con.line", module: "test", descriptor: line_d } }

static ACLS: &[Acl] = &[
    Acl { model: "con.order", group: "u", read: true, write: true, create: true, delete: true },
    Acl { model: "con.line", group: "u", read: true, write: true, create: true, delete: true },
];

fn m(d: &'static ModelDescriptor) -> ResolvedModel { resolve(d, &[]).unwrap() }
async fn order_count(db: &Db) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM con_order").fetch_one(db.pool()).await.unwrap()
}

#[tokio::test]
async fn constraint_rejects_unbalanced_writes_in_tx() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => { eprintln!("skipping: DATABASE_URL not set"); return; }
    };
    let db = Db::connect(&url).await.unwrap();
    let su = Ctx::new(0, vec![]).sudo();
    let (order, line) = (m(&ORDER), m(&LINE));

    db.drop_table(&line).await.unwrap();
    db.drop_table(&order).await.unwrap();
    db.create_table(&order).await.unwrap();
    db.create_table(&line).await.unwrap();

    // Balanced create (10 = 4 + 6) → succeeds; the constraint saw the children written in the same tx.
    let oid = db
        .insert_secured(&order, &su, ACLS, &[], json!({ "name": "ok", "header_total": 10, "line_ids": [ { "amount": 4 }, { "amount": 6 } ] }).as_object().unwrap())
        .await
        .unwrap();
    assert_eq!(order_count(&db).await, 1);

    // Unbalanced create (10 != 4 + 5) → rejected, and the whole create rolls back (no order, no lines).
    let err = db
        .insert_secured(&order, &su, ACLS, &[], json!({ "name": "bad", "header_total": 10, "line_ids": [ { "amount": 4 }, { "amount": 5 } ] }).as_object().unwrap())
        .await;
    assert!(err.is_err(), "unbalanced create rejected");
    assert_eq!(order_count(&db).await, 1, "rolled back — no second order");
    let orphan_lines: i64 = sqlx::query_scalar("SELECT count(*) FROM con_line WHERE order_id NOT IN (SELECT id FROM con_order)").fetch_one(db.pool()).await.unwrap();
    assert_eq!(orphan_lines, 0, "no orphan lines from the rolled-back create");

    // Update header alone to break the balance (header 11 vs lines 10) → rejected, header unchanged.
    let err = db.update_secured(&order, &su, ACLS, &[], oid, json!({ "header_total": 11 }).as_object().unwrap()).await;
    assert!(err.is_err(), "unbalanced header update rejected");
    let total: i64 = sqlx::query_scalar("SELECT header_total FROM con_order WHERE id = $1").bind(oid).fetch_one(db.pool()).await.unwrap();
    assert_eq!(total, 10, "header rolled back to its balanced value");

    // Adding a line that breaks the balance → rejected, line not added.
    let err = db.update_secured(&order, &su, ACLS, &[], oid, json!({ "line_ids": [ { "op": "create", "values": { "amount": 5 } } ] }).as_object().unwrap()).await;
    assert!(err.is_err(), "adding an unbalancing line rejected");
    let nlines: i64 = sqlx::query_scalar("SELECT count(*) FROM con_line WHERE order_id = $1").bind(oid).fetch_one(db.pool()).await.unwrap();
    assert_eq!(nlines, 2, "line not added (rolled back)");

    // A balanced change in one call (header 15 AND a new line 5: 15 = 4 + 6 + 5) → succeeds.
    db.update_secured(&order, &su, ACLS, &[], oid, json!({ "header_total": 15, "line_ids": [ { "op": "create", "values": { "amount": 5 } } ] }).as_object().unwrap()).await.unwrap();
    let nlines: i64 = sqlx::query_scalar("SELECT count(*) FROM con_line WHERE order_id = $1").bind(oid).fetch_one(db.pool()).await.unwrap();
    assert_eq!(nlines, 3, "balanced multi-field update applied");

    db.drop_table(&line).await.unwrap();
    db.drop_table(&order).await.unwrap();
}
