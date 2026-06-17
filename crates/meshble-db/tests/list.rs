//! list_secured end-to-end: filter (typed Domain) + order + limit/offset + total, with the total
//! computed under the SAME secured domain (so record rules restrict the count too). Live Postgres.

use meshble_core::{
    resolve, Acl, Ctx, Domain, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, Operation,
    RecordRule, ResolvedModel,
};
use meshble_db::Db;

static ITEM: ModelDescriptor = ModelDescriptor {
    name: "lst.item",
    table: "lst_item",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[] },
        FieldDef { name: "qty", label: "Qty", kind: FieldKind::Integer, required: true, stored: true, compute: None, depends: &[] },
        FieldDef { name: "active", label: "Active", kind: FieldKind::Bool, required: false, stored: true, compute: None, depends: &[] },
    ],
};
fn item_desc() -> &'static ModelDescriptor {
    &ITEM
}
meshble_core::inventory::submit! { ModelRegistration { name: "lst.item", module: "test", descriptor: item_desc } }

fn item_model() -> ResolvedModel {
    resolve(&ITEM, &[]).unwrap()
}

fn active_only() -> Domain {
    Domain::field("active").eq(true)
}

#[tokio::test]
async fn list_secured_filters_sorts_paginates_and_totals() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let m = item_model();
    let su = Ctx::new(0, vec![]).sudo();

    db.drop_table(&m).await.unwrap();
    db.create_table(&m).await.unwrap();
    for (n, q, a) in [("a", 1, true), ("b", 2, true), ("c", 3, false), ("d", 4, true), ("e", 5, true)] {
        let v = serde_json::json!({ "name": n, "qty": q, "active": a });
        db.insert_secured(&m, &su, &[], &[], v.as_object().unwrap()).await.unwrap();
    }

    // All rows, default order, with the total.
    let p = db.list_secured(&m, &su, &[], &[], None, &[], 80, 0).await.unwrap();
    assert_eq!(p.total, 5);
    assert_eq!(p.data.len(), 5);

    // Typed filter qty >= 3.
    let f = Domain::field("qty").ge(3_i64);
    let p = db.list_secured(&m, &su, &[], &[], Some(&f), &[], 80, 0).await.unwrap();
    assert_eq!(p.total, 3);

    // Order by qty DESC, limit 2 → the two largest.
    let p = db.list_secured(&m, &su, &[], &[], None, &[("qty".into(), true)], 2, 0).await.unwrap();
    assert_eq!(p.data.len(), 2);
    assert_eq!(p.data[0]["qty"].as_i64().unwrap(), 5);
    assert_eq!(p.data[1]["qty"].as_i64().unwrap(), 4);
    assert_eq!(p.total, 5, "total ignores limit");

    // Offset into an ascending order.
    let p = db.list_secured(&m, &su, &[], &[], None, &[("qty".into(), false)], 2, 2).await.unwrap();
    assert_eq!(p.data[0]["qty"].as_i64().unwrap(), 3);

    // The total respects the read record rule (active = true): only 4 of 5 rows are visible.
    let ctx = Ctx::new(1, vec!["u".to_string()]);
    let acls = [Acl { model: "lst.item", group: "u", read: true, write: false, create: false, delete: false }];
    let rules = [RecordRule { model: "lst.item", groups: &["u"], ops: &[Operation::Read], domain: active_only }];
    let p = db.list_secured(&m, &ctx, &acls, &rules, None, &[], 80, 0).await.unwrap();
    assert_eq!(p.total, 4, "total counts only rule-visible rows");
    assert_eq!(p.data.len(), 4);

    // An unknown order field is rejected (never reaches SQL).
    assert!(db.list_secured(&m, &su, &[], &[], None, &[("nope".into(), false)], 80, 0).await.is_err());

    db.drop_table(&m).await.unwrap();
}
