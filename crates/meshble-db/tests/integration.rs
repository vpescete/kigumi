//! End-to-end persistence test against a live Postgres.
//! Set `DATABASE_URL` (e.g. `postgres://user@127.0.0.1/meshble_test`) to run it; skipped otherwise.

use meshble_core::{resolve, FieldDef, FieldKind, ModelDescriptor, ResolvedModel};
use meshble_db::Db;
use meshble_core::Domain;

static MODEL: ModelDescriptor = ModelDescriptor {
    name: "widget",
    table: "widget_test",
    fields: &[
        FieldDef {
            name: "name", label: "Name", kind: FieldKind::Text,
            required: true, stored: true, compute: None, depends: &[],
        },
        FieldDef {
            name: "qty", label: "Qty", kind: FieldKind::Integer,
            required: true, stored: true, compute: None, depends: &[],
        },
        FieldDef {
            name: "active", label: "Active", kind: FieldKind::Bool,
            required: false, stored: true, compute: None, depends: &[],
        },
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
