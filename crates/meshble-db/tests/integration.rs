//! End-to-end persistence test against a live Postgres.
//! Set `DATABASE_URL` (e.g. `postgres://user@127.0.0.1/meshble_test`) to run it; skipped otherwise.

use meshble_core::{
    resolve, Acl, Ctx, Domain, FieldDef, FieldKind, ModelDescriptor, Operation, RecordRule,
    ResolvedModel,
};
use meshble_db::{Db, DbError};

static MODEL: ModelDescriptor = ModelDescriptor {
    name: "widget",
    table: "widget_test",
    fields: &[
        FieldDef {
            name: "name", label: "Name", kind: FieldKind::Text,
            required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef {
            name: "qty", label: "Qty", kind: FieldKind::Integer,
            required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef {
            name: "active", label: "Active", kind: FieldKind::Bool,
            required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};

fn model() -> ResolvedModel {
    resolve(&MODEL, &[]).unwrap()
}

#[tokio::test]
async fn parameterized_domain_query_runs_against_postgres() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.expect("connect to postgres");
    let m = model();

    // Metamodel -> real table.
    db.drop_table(&m).await.unwrap();
    db.create_table(&m).await.unwrap();

    for (n, q, a) in [("alpha", 5_i64, true), ("beta", 50_i64, true), ("gamma", 5_i64, false)] {
        sqlx::query("INSERT INTO widget_test (name, qty, active) VALUES ($1, $2, $3)")
            .bind(n)
            .bind(q)
            .bind(a)
            .execute(db.pool())
            .await
            .unwrap();
    }

    // Domain -> parameterized WHERE: active = true AND qty < 10 -> only "alpha".
    let d = Domain::field("active").eq(true).and(Domain::field("qty").lt(10_i64));
    assert_eq!(db.count_where(&m, &d).await.unwrap(), 1);

    // Injection attempt as a VALUE is treated as data: matches nothing, and the table survives.
    let evil = "alpha'; DROP TABLE widget_test; --";
    let d2 = Domain::field("name").eq(evil);
    assert_eq!(db.count_where(&m, &d2).await.unwrap(), 0);

    // Table intact -> the DROP inside the value was never executed.
    assert_eq!(db.count_where(&m, &Domain::True).await.unwrap(), 3);

    db.drop_table(&m).await.unwrap();
}

// Distinct table so this test never races the one above when run in parallel.
static SEC_MODEL: ModelDescriptor = ModelDescriptor {
    name: "widget",
    table: "widget_sec_test",
    fields: &[
        FieldDef {
            name: "name", label: "Name", kind: FieldKind::Text,
            required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef {
            name: "active", label: "Active", kind: FieldKind::Bool,
            required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};

fn active_only() -> Domain {
    Domain::field("active").eq(true)
}

static ACLS: &[Acl] = &[Acl {
    model: "widget", group: "u", read: true, write: false, create: false, delete: false,
}];
static RULES: &[RecordRule] = &[RecordRule {
    model: "widget", groups: &["u"], ops: &[Operation::Read], domain: active_only,
}];

#[tokio::test]
async fn security_is_enforced_on_reads() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.expect("connect");
    let m = resolve(&SEC_MODEL, &[]).unwrap();
    db.drop_table(&m).await.unwrap();
    db.create_table(&m).await.unwrap();
    for (n, a) in [("alpha", true), ("beta", true), ("gamma", false)] {
        sqlx::query("INSERT INTO widget_sec_test (name, active) VALUES ($1, $2)")
            .bind(n)
            .bind(a)
            .execute(db.pool())
            .await
            .unwrap();
    }

    let u = Ctx::new(1, vec!["u".to_string()]);
    // The record rule restricts group "u" to active rows → only alpha, beta.
    assert_eq!(db.count_secured(&m, &u, ACLS, RULES, None).await.unwrap(), 2);
    assert_eq!(db.find_ids_secured(&m, &u, ACLS, RULES, None).await.unwrap().len(), 2);

    // A user without a granting ACL group is denied outright.
    let other = Ctx::new(2, vec!["x".to_string()]);
    assert!(matches!(
        db.count_secured(&m, &other, ACLS, RULES, None).await,
        Err(DbError::AccessDenied { .. })
    ));

    // sudo bypasses ACL and record rules → all 3 rows.
    assert_eq!(db.count_secured(&m, &u.sudo(), ACLS, RULES, None).await.unwrap(), 3);

    db.drop_table(&m).await.unwrap();
}
