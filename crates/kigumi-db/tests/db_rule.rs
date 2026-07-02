//! D12 part 2: runtime DB-backed record rules. A rule stored as a JSON domain loads into a
//! RuleDomain::Owned and flows through the SAME engine as the compiled-in rules, so DB rules add to
//! the static baseline (global rules AND together). Superuser bypasses all rules. Live Postgres.

use kigumi_core::{
    resolve, Acl, Ctx, Domain, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, Operation,
    RecordRule, RuleDomain,
};
use kigumi_db::Db;
use serde_json::{json, Value};

static DOC: ModelDescriptor = ModelDescriptor {
    name: "rr.doc",
    table: "rr_doc",
    fields: &[
        FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "state", label: "State", kind: FieldKind::Text, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
fn doc_desc() -> &'static ModelDescriptor {
    &DOC
}
kigumi_core::inventory::submit! { ModelRegistration { name: "rr.doc", module: "test", descriptor: doc_desc } }

fn not_secret() -> Domain {
    Domain::field("name").ne("secret")
}
static ACLS: &[Acl] = &[Acl { model: "rr.doc", group: "u", read: true, write: true, create: true, delete: true }];
// Compiled-in baseline: nobody (global) reads the row named "secret".
static STATIC_RULES: &[RecordRule] =
    &[RecordRule { model: "rr.doc", groups: &[], ops: &[Operation::Read], domain: RuleDomain::Static(not_secret) }];

fn names(rows: &[Value]) -> Vec<String> {
    let mut v: Vec<String> = rows.iter().map(|r| r["name"].as_str().unwrap().to_string()).collect();
    v.sort();
    v
}

#[tokio::test]
async fn db_rules_apply_additively_with_the_static_baseline() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let m = resolve(&DOC, &[]).unwrap();
    let su = Ctx::new(0, vec![]).sudo();
    let clerk = Ctx::new(1, vec!["u".to_string()]);

    db.ensure_access_schema().await.unwrap();
    // Clean any rr.doc rules left by a previous run.
    for r in db.list_db_rules().await.unwrap() {
        if r.model == "rr.doc" {
            db.remove_db_rule(r.id).await.unwrap();
        }
    }
    db.drop_table(&m).await.unwrap();
    db.create_table(&m).await.unwrap();
    for (n, s) in [("a", "open"), ("b", "archived"), ("secret", "open")] {
        db.insert_secured(&m, &su, ACLS, &[], json!({ "name": n, "state": s }).as_object().unwrap()).await.unwrap();
    }

    // Only the static rule applies → "secret" hidden; the clerk sees a, b.
    let merged: Vec<RecordRule> = STATIC_RULES.to_vec();
    assert_eq!(names(&db.find_secured(&m, &clerk, ACLS, &merged, None).await.unwrap()), vec!["a", "b"]);

    // Add a runtime DB rule (global): hide "archived" rows. It ANDs with the static rule.
    let id = db.set_db_rule("rr.doc", "", "r", &Domain::field("state").ne("archived").to_json()).await.unwrap();
    let mut merged = STATIC_RULES.to_vec();
    merged.extend(db.load_rules_static().await.unwrap());
    assert_eq!(
        names(&db.find_secured(&m, &clerk, ACLS, &merged, None).await.unwrap()),
        vec!["a"],
        "static (not secret) AND db (not archived) → only 'a'"
    );
    // Superuser bypasses every rule.
    assert_eq!(db.find_secured(&m, &su, ACLS, &merged, None).await.unwrap().len(), 3, "sudo sees all");

    // A malformed domain is rejected at write time.
    assert!(db.set_db_rule("rr.doc", "", "r", "{not json").await.is_err(), "invalid domain rejected on write");

    // Removing the DB rule restores the static-only behavior.
    db.remove_db_rule(id).await.unwrap();
    let mut after = STATIC_RULES.to_vec();
    after.extend(db.load_rules_static().await.unwrap());
    assert_eq!(names(&db.find_secured(&m, &clerk, ACLS, &after, None).await.unwrap()), vec!["a", "b"]);

    db.drop_table(&m).await.unwrap();
}
