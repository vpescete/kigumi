//! Fase A new field kinds: Float (double precision), Date, Datetime — create/read/update, domain
//! filtering (incl. date casts), explicit-null writes, and a malformed date → clean BadInput. Live PG.

use kigumi_core::{resolve, Acl, Domain, FieldDef, FieldKind, ModelDescriptor, ResolvedModel};
use kigumi_db::DbError;
use serde_json::json;

static DOC: ModelDescriptor = ModelDescriptor {
    name: "ft.doc",
    table: "ft_doc",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "qty", label: "Qty", kind: FieldKind::Float, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "due", label: "Due", kind: FieldKind::Date, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "at", label: "At", kind: FieldKind::Datetime, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
static ACLS: &[Acl] = &[Acl { model: "ft.doc", group: "u", read: true, write: true, create: true, delete: true }];

fn model() -> ResolvedModel {
    resolve(&DOC, &[]).unwrap()
}

#[tokio::test]
async fn float_date_datetime_roundtrip_and_filter() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let m = model();
    let su = kigumi_test::su();
    db.drop_table(&m).await.unwrap();
    db.create_table(&m).await.unwrap();

    let id = db.insert_secured(&m, &su, ACLS, &[], json!({
        "name": "a", "qty": 2.5, "due": "2026-01-15", "at": "2026-01-15T10:30:00Z"
    }).as_object().unwrap()).await.unwrap();

    // Read back: float as a number, date as ISO text, datetime as a timestamp starting with the date.
    let got = db.find_one_secured(&m, &su, ACLS, &[], id).await.unwrap().unwrap();
    assert_eq!(got["qty"], 2.5);
    assert_eq!(got["due"], "2026-01-15");
    assert!(got["at"].as_str().unwrap().starts_with("2026-01-15"), "datetime kept, got {:?}", got["at"]);

    // Domain filtering: date equality + range (placeholders cast to ::date), float comparison.
    let on_day = Domain::field("due").eq("2026-01-15");
    assert_eq!(db.find_secured(&m, &su, ACLS, &[], Some(&on_day)).await.unwrap().len(), 1);
    let before_feb = Domain::field("due").lt("2026-02-01");
    assert_eq!(db.find_secured(&m, &su, ACLS, &[], Some(&before_feb)).await.unwrap().len(), 1);
    let before_jan = Domain::field("due").lt("2026-01-01");
    assert_eq!(db.find_secured(&m, &su, ACLS, &[], Some(&before_jan)).await.unwrap().len(), 0);
    let heavy = Domain::field("qty").gt(2.0_f64);
    assert_eq!(db.find_secured(&m, &su, ACLS, &[], Some(&heavy)).await.unwrap().len(), 1);

    // Explicit-null writes to float/date/datetime columns (the ::type placeholder cast handles NULL).
    db.update_secured(&m, &su, ACLS, &[], id, json!({ "qty": null, "due": null, "at": null }).as_object().unwrap()).await.unwrap();
    let cleared = db.find_one_secured(&m, &su, ACLS, &[], id).await.unwrap().unwrap();
    assert!(cleared["qty"].is_null() && cleared["due"].is_null() && cleared["at"].is_null());

    // A malformed date is a clean BadInput (400), not an opaque SQL error.
    let bad = db.insert_secured(&m, &su, ACLS, &[], json!({ "name": "x", "due": "not-a-date" }).as_object().unwrap()).await;
    assert!(matches!(bad, Err(DbError::BadInput(_))), "malformed date → BadInput, got {bad:?}");

    db.drop_table(&m).await.unwrap();
}
