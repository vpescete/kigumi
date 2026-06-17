//! Relation traversal end-to-end: a record rule that filters via a Many2one relation
//! (`company_id.name = 'Acme'`) compiles to a subquery and restricts the rows a user sees.
//! Requires `DATABASE_URL`; skipped otherwise.

use meshble_core::{
    resolve, Acl, Ctx, Domain, FieldDef, FieldKind, ModelDescriptor, ModelRegistration, Operation,
    RecordRule, RuleDomain, ResolvedModel,
};
use meshble_db::Db;

static COMPANY: ModelDescriptor = ModelDescriptor {
    name: "rel.company",
    table: "rel_company",
    fields: &[FieldDef {
        name: "name", label: "Name", kind: FieldKind::Text,
        required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
};
static DOC: ModelDescriptor = ModelDescriptor {
    name: "rel.doc",
    table: "rel_doc",
    fields: &[
        FieldDef { name: "title", label: "Title", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        FieldDef { name: "company_id", label: "Company", kind: FieldKind::Many2one { target: "rel.company" }, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};

fn company_desc() -> &'static ModelDescriptor {
    &COMPANY
}
fn doc_desc() -> &'static ModelDescriptor {
    &DOC
}
meshble_core::inventory::submit! { ModelRegistration { name: "rel.company", module: "test", descriptor: company_desc } }
meshble_core::inventory::submit! { ModelRegistration { name: "rel.doc", module: "test", descriptor: doc_desc } }

fn acme_only() -> Domain {
    Domain::field("company_id.name").eq("Acme")
}
static ACLS: &[Acl] = &[Acl {
    model: "rel.doc", group: "u", read: true, write: false, create: false, delete: false,
}];
static RULES: &[RecordRule] = &[RecordRule {
    model: "rel.doc", groups: &["u"], ops: &[Operation::Read], domain: RuleDomain::Static(acme_only),
}];

fn doc_model() -> ResolvedModel {
    resolve(&DOC, &[]).unwrap()
}

#[tokio::test]
async fn record_rule_filters_through_a_relation() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let company = resolve(&COMPANY, &[]).unwrap();
    let doc = doc_model();

    db.drop_table(&doc).await.unwrap(); // child first (FK)
    db.drop_table(&company).await.unwrap();
    db.create_table(&company).await.unwrap();
    db.create_table(&doc).await.unwrap();

    sqlx::query("INSERT INTO rel_company (name) VALUES ('Acme'), ('Globex')").execute(db.pool()).await.unwrap();
    let acme: i64 = sqlx::query_scalar("SELECT id FROM rel_company WHERE name = 'Acme'").fetch_one(db.pool()).await.unwrap();
    let globex: i64 = sqlx::query_scalar("SELECT id FROM rel_company WHERE name = 'Globex'").fetch_one(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO rel_doc (title, company_id) VALUES ('acme-doc', $1), ('globex-doc', $2)")
        .bind(acme).bind(globex).execute(db.pool()).await.unwrap();

    // Caller is in both companies (so the M7 company filter admits both docs); the relation record
    // rule then restricts to docs whose company is Acme.
    let ctx = Ctx::new(1, vec!["u".to_string()]).in_companies(acme, vec![acme, globex]);
    let rows = db.find_secured(&doc, &ctx, ACLS, RULES, None).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["title"], "acme-doc");

    db.drop_table(&doc).await.unwrap();
    db.drop_table(&company).await.unwrap();
}

// A nullable Many2one to test that negated relation rules include rows whose FK is NULL.
static P_PARTNER: ModelDescriptor = ModelDescriptor {
    name: "p.partner",
    table: "p_partner",
    fields: &[FieldDef {
        name: "code", label: "Code", kind: FieldKind::Text,
        required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
};
static P_DOC: ModelDescriptor = ModelDescriptor {
    name: "p.doc",
    table: "p_doc",
    fields: &[
        FieldDef { name: "title", label: "Title", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        // Nullable (required: false) — a doc may have no partner.
        FieldDef { name: "partner_id", label: "Partner", kind: FieldKind::Many2one { target: "p.partner" }, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
    ],
};
fn p_partner_desc() -> &'static ModelDescriptor {
    &P_PARTNER
}
fn p_doc_desc() -> &'static ModelDescriptor {
    &P_DOC
}
meshble_core::inventory::submit! { ModelRegistration { name: "p.partner", module: "test", descriptor: p_partner_desc } }
meshble_core::inventory::submit! { ModelRegistration { name: "p.doc", module: "test", descriptor: p_doc_desc } }

fn not_blocked() -> Domain {
    Domain::field("partner_id.code").eq("blocked").not()
}
static P_ACLS: &[Acl] = &[Acl {
    model: "p.doc", group: "u", read: true, write: false, create: false, delete: false,
}];
static P_RULES: &[RecordRule] = &[RecordRule {
    model: "p.doc", groups: &["u"], ops: &[Operation::Read], domain: RuleDomain::Static(not_blocked),
}];

#[tokio::test]
async fn negated_relation_rule_includes_null_fk_rows() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let partner = resolve(&P_PARTNER, &[]).unwrap();
    let doc = resolve(&P_DOC, &[]).unwrap();

    db.drop_table(&doc).await.unwrap();
    db.drop_table(&partner).await.unwrap();
    db.create_table(&partner).await.unwrap();
    db.create_table(&doc).await.unwrap();

    sqlx::query("INSERT INTO p_partner (code) VALUES ('blocked')").execute(db.pool()).await.unwrap();
    let blocked: i64 = sqlx::query_scalar("SELECT id FROM p_partner WHERE code = 'blocked'").fetch_one(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO p_doc (title, partner_id) VALUES ('has-blocked', $1)").bind(blocked).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO p_doc (title) VALUES ('no-partner')").execute(db.pool()).await.unwrap(); // partner_id NULL

    // Rule = NOT(partner.code = 'blocked'): the no-partner doc must be visible; the blocked one not.
    let ctx = Ctx::new(1, vec!["u".to_string()]);
    let rows = db.find_secured(&doc, &ctx, P_ACLS, P_RULES, None).await.unwrap();
    let titles: Vec<&str> = rows.iter().map(|r| r["title"].as_str().unwrap()).collect();
    assert!(titles.contains(&"no-partner"), "null-FK row must be included under a negated rule");
    assert!(!titles.contains(&"has-blocked"), "blocked partner's doc must be excluded");

    db.drop_table(&doc).await.unwrap();
    db.drop_table(&partner).await.unwrap();
}
