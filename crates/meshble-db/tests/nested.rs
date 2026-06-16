//! Nested x2many writes: create a parent WITH its One2many children in one insert (the iconic
//! "create order with lines"), atomically, with the aggregate computed from the children. A child
//! ACL denial rolls the whole thing back. Requires `DATABASE_URL`; skipped otherwise.

use meshble_core::{
    resolve, Acl, ComputeInput, Ctx, Domain, FieldDef, FieldKind, ModelDescriptor,
    ModelRegistration, Operation, RecordRule, ResolvedModel, Value,
};
use meshble_db::Db;

static ORDER: ModelDescriptor = ModelDescriptor {
    name: "nst.order",
    table: "nst_order",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[] },
        FieldDef { name: "line_ids", label: "Lines", kind: FieldKind::One2many { target: "nst.line", inverse: "order_id" }, required: false, stored: false, compute: None, depends: &[] },
        FieldDef { name: "amount_total", label: "Total", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: Some("nst_total"), depends: &["line_ids.price"] },
    ],
};
static LINE: ModelDescriptor = ModelDescriptor {
    name: "nst.line",
    table: "nst_line",
    fields: &[
        FieldDef { name: "order_id", label: "Order", kind: FieldKind::Many2one { target: "nst.order" }, required: true, stored: true, compute: None, depends: &[] },
        FieldDef { name: "price", label: "Price", kind: FieldKind::Decimal { currency_field: None }, required: true, stored: true, compute: None, depends: &[] },
    ],
};

fn order_desc() -> &'static ModelDescriptor {
    &ORDER
}
fn line_desc() -> &'static ModelDescriptor {
    &LINE
}
meshble_core::inventory::submit! { ModelRegistration { name: "nst.order", module: "test", descriptor: order_desc } }
meshble_core::inventory::submit! { ModelRegistration { name: "nst.line", module: "test", descriptor: line_desc } }

fn nst_total(i: &ComputeInput) -> Value {
    Value::Float(i.sum_float("line_ids", "price"))
}
meshble_core::inventory::submit! { meshble_core::ComputeRegistration { name: "nst_total", func: nst_total } }

static RULES: &[RecordRule] = &[];

fn order_model() -> ResolvedModel {
    resolve(&ORDER, &[]).unwrap()
}
fn line_model() -> ResolvedModel {
    resolve(&LINE, &[]).unwrap()
}

async fn count_lines(db: &Db, oid: i64) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM nst_line WHERE order_id = $1")
        .bind(oid)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

fn url_or_skip() -> Option<String> {
    match std::env::var("DATABASE_URL") {
        Ok(u) => Some(u),
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            None
        }
    }
}

async fn fresh(db: &Db, order: &ResolvedModel, line: &ResolvedModel) {
    db.drop_table(line).await.unwrap();
    db.drop_table(order).await.unwrap();
    db.create_table(order).await.unwrap();
    db.create_table(line).await.unwrap();
}

#[tokio::test]
async fn creates_a_parent_with_its_children_in_one_write() {
    let url = match url_or_skip() {
        Some(u) => u,
        None => return,
    };
    let db = Db::connect(&url).await.unwrap();
    let order = order_model();
    let line = line_model();
    let su = Ctx::new(0, vec![]).sudo();
    fresh(&db, &order, &line).await;

    // One write creates the order AND two lines, with the inverse FK set by the parent.
    let payload = serde_json::json!({
        "name": "O1",
        "line_ids": [ { "price": 10.0 }, { "price": 5.0 } ]
    });
    let oid = db
        .insert_secured(&order, &su, &[], RULES, payload.as_object().unwrap())
        .await
        .unwrap();

    assert_eq!(count_lines(&db, oid).await, 2, "both lines created and linked to the order");
    let total: f64 = sqlx::query_scalar("SELECT amount_total::float8 FROM nst_order WHERE id = $1")
        .bind(oid)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(total, 15.0, "aggregate computed from the nested children");

    db.drop_table(&line).await.unwrap();
    db.drop_table(&order).await.unwrap();
}

#[tokio::test]
async fn child_acl_denial_rolls_back_the_parent() {
    let url = match url_or_skip() {
        Some(u) => u,
        None => return,
    };
    let db = Db::connect(&url).await.unwrap();
    let order = order_model();
    let line = line_model();
    fresh(&db, &order, &line).await;

    // Group "u" may create orders but NOT lines (no ACL row for nst.line → Create denied).
    let acls = [Acl { model: "nst.order", group: "u", read: true, write: true, create: true, delete: true }];
    let ctx = Ctx::new(1, vec!["u".to_string()]);
    let payload = serde_json::json!({
        "name": "O1",
        "line_ids": [ { "price": 10.0 } ]
    });
    let res = db.insert_secured(&order, &ctx, &acls, RULES, payload.as_object().unwrap()).await;
    assert!(res.is_err(), "child Create is denied → the whole insert fails");

    let orders: i64 = sqlx::query_scalar("SELECT count(*) FROM nst_order")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(orders, 0, "parent rolled back when a child write is rejected");

    db.drop_table(&line).await.unwrap();
    db.drop_table(&order).await.unwrap();
}

#[tokio::test]
async fn reads_a_parent_with_its_children_inlined() {
    let url = match url_or_skip() {
        Some(u) => u,
        None => return,
    };
    let db = Db::connect(&url).await.unwrap();
    let order = order_model();
    let line = line_model();
    let su = Ctx::new(0, vec![]).sudo();
    fresh(&db, &order, &line).await;

    let oid = db
        .insert_secured(
            &order,
            &su,
            &[],
            RULES,
            serde_json::json!({ "name": "O1", "line_ids": [ { "price": 10.0 }, { "price": 5.0 } ] })
                .as_object()
                .unwrap(),
        )
        .await
        .unwrap();

    let got = db.find_one_secured(&order, &su, &[], RULES, oid).await.unwrap().expect("order visible");
    let obj = got.as_object().unwrap();
    assert_eq!(obj["name"], "O1");
    assert_eq!(obj["amount_total"].as_f64().unwrap(), 15.0, "aggregate present on the parent");
    let lines = obj["line_ids"].as_array().expect("line_ids inlined as an array");
    assert_eq!(lines.len(), 2, "both children inlined");
    assert_eq!(lines[0]["order_id"].as_i64().unwrap(), oid, "child points back to the parent");

    db.drop_table(&line).await.unwrap();
    db.drop_table(&order).await.unwrap();
}

#[tokio::test]
async fn child_model_not_readable_is_omitted_from_inline() {
    let url = match url_or_skip() {
        Some(u) => u,
        None => return,
    };
    let db = Db::connect(&url).await.unwrap();
    let order = order_model();
    let line = line_model();
    let su = Ctx::new(0, vec![]).sudo();
    fresh(&db, &order, &line).await;

    let oid = db
        .insert_secured(
            &order,
            &su,
            &[],
            RULES,
            serde_json::json!({ "name": "O1", "line_ids": [ { "price": 10.0 } ] }).as_object().unwrap(),
        )
        .await
        .unwrap();

    // Group "u" may read orders but NOT lines (no Read ACL on nst.line).
    let acls = [Acl { model: "nst.order", group: "u", read: true, write: false, create: false, delete: false }];
    let ctx = Ctx::new(1, vec!["u".to_string()]);
    let got = db.find_one_secured(&order, &ctx, &acls, RULES, oid).await.unwrap().expect("order visible");
    let obj = got.as_object().unwrap();
    assert!(obj.get("line_ids").is_none(), "unreadable child model → relation omitted, parent still served");

    db.drop_table(&line).await.unwrap();
    db.drop_table(&order).await.unwrap();
}

fn line_price_min() -> Domain {
    Domain::field("price").ge(10.0)
}
// Create rule on the CHILD: a line is only creatable if price >= 10.
static PRICE_RULES: &[RecordRule] = &[RecordRule {
    model: "nst.line", groups: &["u"], ops: &[Operation::Create], domain: line_price_min,
}];

#[tokio::test]
async fn child_create_rule_violation_rolls_back_the_parent() {
    let url = match url_or_skip() {
        Some(u) => u,
        None => return,
    };
    let db = Db::connect(&url).await.unwrap();
    let order = order_model();
    let line = line_model();
    fresh(&db, &order, &line).await;

    // Group "u" may create both, but the child Create rule requires price >= 10.
    let acls = [
        Acl { model: "nst.order", group: "u", read: true, write: true, create: true, delete: true },
        Acl { model: "nst.line", group: "u", read: true, write: true, create: true, delete: true },
    ];
    let ctx = Ctx::new(1, vec!["u".to_string()]);

    // A nested line with price 5 violates the child Create rule → the whole insert rolls back
    // (nesting must not be a weaker path than the line's own endpoint).
    let bad = serde_json::json!({ "name": "O1", "line_ids": [ { "price": 5.0 } ] });
    let res = db.insert_secured(&order, &ctx, &acls, PRICE_RULES, bad.as_object().unwrap()).await;
    assert!(res.is_err(), "child Create rule (price>=10) blocks the nested line");
    let orders: i64 = sqlx::query_scalar("SELECT count(*) FROM nst_order")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(orders, 0, "parent rolled back on a child record-rule violation");

    // A conforming line (price 10) is accepted.
    let ok = serde_json::json!({ "name": "O2", "line_ids": [ { "price": 10.0 } ] });
    let oid = db.insert_secured(&order, &ctx, &acls, PRICE_RULES, ok.as_object().unwrap()).await.unwrap();
    assert_eq!(count_lines(&db, oid).await, 1, "conforming nested line is created");

    db.drop_table(&line).await.unwrap();
    db.drop_table(&order).await.unwrap();
}
