//! `delete_secured` is ONE transaction: the row, its polymorphic cleanups and the delete event
//! commit together or not at all. Before this, the DELETE was autocommit and the cleanups followed
//! on separate connections — a failure after it left the record gone and its attachments orphaned.
//! Live PG.

use kigumi_core::{resolve, Acl, FieldDef, FieldKind, ModelDescriptor, ResolvedModel};
use serde_json::json;

static HOST: ModelDescriptor = ModelDescriptor {
    name: "del.host",
    table: "del_host",
    fields: &[FieldDef {
        name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true,
        compute: None, depends: &[], default: None, unique: false, check: None,
    }],
};

static ACLS: &[Acl] =
    &[Acl { model: "del.host", group: "u", read: true, write: true, create: true, delete: true }];

async fn exists(pool: &sqlx::PgPool, id: i64) -> bool {
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM del_host WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    n == 1
}

#[tokio::test]
async fn a_failing_cleanup_rolls_the_delete_back_instead_of_orphaning_it() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;
    let m: ResolvedModel = resolve(&HOST, &[]).unwrap();
    let su = kigumi_test::su();
    db.drop_table(&m).await.unwrap();
    db.create_table(&m).await.unwrap();

    let id = db
        .insert_secured(&m, &su, &[], &[], json!({ "name": "doomed" }).as_object().unwrap())
        .await
        .unwrap();

    // A kigumi_attachment whose res_id is TEXT: the cleanup's `res_id = $2` binds an int8, so the
    // statement fails with 42883 (operator does not exist) — a real error, NOT the 42P01 the
    // tolerance is allowed to swallow. It stands in for any cleanup that fails mid-delete.
    sqlx::query("DROP TABLE IF EXISTS kigumi_attachment").execute(db.pool()).await.unwrap();
    sqlx::query("CREATE TABLE kigumi_attachment (id bigserial PRIMARY KEY, res_model text NOT NULL, res_id text NOT NULL)")
        .execute(db.pool())
        .await
        .unwrap();

    let err = db.delete_secured(&m, &su, ACLS, &[], id).await;
    assert!(err.is_err(), "a failing cleanup must fail the delete: {err:?}");
    assert!(
        exists(db.pool(), id).await,
        "the record must still be there — the whole transaction rolled back"
    );

    // Remove the hostile table and the same delete now succeeds, end to end.
    sqlx::query("DROP TABLE kigumi_attachment").execute(db.pool()).await.unwrap();
    let n = db.delete_secured(&m, &su, ACLS, &[], id).await.unwrap();
    assert_eq!(n, 1);
    assert!(!exists(db.pool(), id).await, "and now it is gone");

    // The delete event committed with it, on the same transaction.
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event_outbox WHERE model = 'del.host' AND record_id = $1 AND event_type = 'model.deleted'",
    )
    .bind(id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(events, 1, "exactly one delete event, and only for the delete that committed");

    db.drop_table(&m).await.unwrap();
}
