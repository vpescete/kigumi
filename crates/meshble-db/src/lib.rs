//! Postgres persistence layer.
//!
//! Closes the loop: the metamodel's generated DDL creates real tables, and a [`Domain`] is
//! compiled to a PARAMETERIZED `WHERE` whose values are BOUND (never interpolated) before
//! execution. The `*_secured` methods enforce the security engine (ACL + record rules) at the
//! database boundary: access is checked, and the user's record-rule domain is AND-ed into the
//! query — so a user can never read rows the rules forbid.

mod access_store;
mod auth_store;
mod cron;
mod custom_field;
mod migration;
mod module_store;
mod sequence;
mod settings;
mod tax;
mod view_override;
pub use access_store::{AclRow, RuleRow};
pub use auth_store::UserRow;
pub use custom_field::{is_safe_ident, CustomField};
pub use view_override::ViewOverride;
pub use cron::{registered_crons, CronFn, CronRegistration};
pub use migration::{Migration, MigrationOutcome};
pub use tax::{compute_tax_lines, TaxResult, TaxSpec};

use meshble_core::{
    action_for, check_access, check_constraints, compute_on_read, compute_stored, computed_fields,
    delegated_fields, field_accessible, field_is_readonly, has_constraints, has_read_computes, inherits_of, is_mailed,
    record_rule_domain, related_path, resolve_all_registered,
    resolve_registered, tracked_fields, Acl, ActionInput, Children, Ctx, Domain, DomainError,
    FieldDef, FieldKind, Operation, RecordRule, ResolvedModel, Value,
};
use meshble_schema::to_ddl;
use serde_json::{Map, Value as Json};
use sqlx::postgres::{PgArguments, PgPoolOptions, PgRow};
use sqlx::query::{Query, QueryScalar};
use sqlx::{PgPool, Postgres, Row};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Debug)]
pub enum DbError {
    Sql(sqlx::Error),
    Domain(DomainError),
    /// The context is not allowed to perform the operation on the model (ACL denied).
    AccessDenied { model: String, operation: &'static str },
    /// A migration problem (e.g. an unparseable version).
    Migration(String),
    /// Invalid write input (unknown/non-writable field, or a value incompatible with its kind).
    BadInput(String),
    /// A constraint conflict (unique violation, FK violation) — maps to HTTP 409.
    Conflict(String),
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for DbError {}

impl From<sqlx::Error> for DbError {
    fn from(e: sqlx::Error) -> Self {
        // Map known Postgres constraint SQLSTATEs to typed, client-safe errors (a curated message +
        // the constraint name — never the raw Postgres text). Everything else stays an opaque Sql.
        if let Some(db) = e.as_database_error() {
            let constraint = db.constraint().unwrap_or("constraint").to_string();
            match db.code().as_deref() {
                Some("23505") => return DbError::Conflict(format!("duplicate value violates unique constraint '{constraint}'")),
                Some("23503") => return DbError::Conflict(format!("foreign-key constraint '{constraint}' violated (referenced row missing or still in use)")),
                Some("23514") => return DbError::BadInput(format!("value violates check constraint '{constraint}'")),
                Some("23502") => return DbError::BadInput("a required field is missing".to_string()),
                // Malformed date/time literal or out-of-range value (e.g. an invalid Date/Datetime).
                Some("22007") | Some("22008") => return DbError::BadInput("invalid date/time value".to_string()),
                _ => {}
            }
        }
        DbError::Sql(e)
    }
}
impl From<DomainError> for DbError {
    fn from(e: DomainError) -> Self {
        DbError::Domain(e)
    }
}

/// A connection pool to a Postgres database. `Clone` shares the same pool (cheap; `PgPool` is an
/// `Arc` internally) — e.g. to hand a handle to the background cron scheduler.
#[derive(Clone)]
pub struct Db {
    pool: PgPool,
}

/// A page of list results plus the total count under the same secured domain.
pub struct ListPage {
    pub data: Vec<Json>,
    pub total: i64,
}

/// The result of a variant-generation run: which `product.product` ids were created, archived (a
/// combination no longer selected), or kept (an existing variant matched a desired combination, left
/// active or reactivated). A no-op regeneration returns all three empty except `kept`.
#[derive(Debug, Default)]
pub struct GenerateOutcome {
    pub created: Vec<i64>,
    pub archived: Vec<i64>,
    pub kept: Vec<i64>,
}

// The product-variant model graph the generator operates on. Hardcoded here (like the mail
// subsystem's table names) rather than threaded through a generic API for a single caller.
const VG_TEMPLATE: &str = "product.template";
const VG_VARIANT: &str = "product.product";
const VG_LINE: &str = "product.template.attribute.line";
const VG_PTAV: &str = "product.template.attribute.value";
const VG_ATTRIBUTE: &str = "product.attribute";
/// The junction (product.product.product_template_attribute_value_ids) linking a variant to its cells.
const VG_VARIANT_PTAV_REL: &str = "variant_ptav_rel";
/// Hard cap on variants produced by one call — a runaway cartesian product (5 attributes x 10 values
/// = 100k rows) must not explode the table in a single request.
const MAX_VARIANTS: usize = 1000;

impl Db {
    /// Connects to `url` (e.g. `postgres://user@host/db`).
    ///
    /// Pins `DateStyle = ISO, YMD` on every pooled connection: the codebase renders dates/datetimes
    /// as `::text` and assumes ISO `YYYY-MM-DD` (big-endian, so lexical order == chronological) — for
    /// activity-state derivation, tracking diffs, and the frontend's date parsing. A server/role
    /// default of `SQL`/`Postgres`/`German` would otherwise silently break those; don't inherit it.
    pub async fn connect(url: &str) -> Result<Db, DbError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    sqlx::query("SET DateStyle = 'ISO, YMD'").execute(&mut *conn).await?;
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        Ok(Db { pool })
    }

    /// Lightweight reachability check for readiness probes.
    pub async fn ping(&self) -> Result<(), DbError> {
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&self.pool).await?;
        Ok(())
    }

    /// Access to the underlying pool (e.g. for raw inserts in tests).
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Creates the model's table from the generated DDL.
    pub async fn create_table(&self, model: &ResolvedModel) -> Result<(), DbError> {
        sqlx::query(&to_ddl(model)).execute(&self.pool).await?;
        Ok(())
    }

    /// Drops the model's table (and its own Many2many junction tables) if they exist. A teardown/test
    /// helper, not a data path. NB `DROP TABLE ... CASCADE` on the model only drops the junction's FK
    /// CONSTRAINT, not the junction TABLE — so the junctions are dropped explicitly here, else their
    /// membership rows would leak across a drop/recreate (e.g. between tests sharing one database).
    pub async fn drop_table(&self, model: &ResolvedModel) -> Result<(), DbError> {
        for f in &model.fields {
            if let FieldKind::Many2many { relation, .. } = f.kind {
                sqlx::query(&format!("DROP TABLE IF EXISTS {relation} CASCADE")).execute(&self.pool).await?;
            }
        }
        let sql = format!("DROP TABLE IF EXISTS {} CASCADE", model.table);
        sqlx::query(&sql).execute(&self.pool).await?;
        Ok(())
    }

    /// Creates the junction tables for this model's Many2many fields (idempotent). Both the model's
    /// and the targets' tables must already exist (the migration runs this AFTER all model tables);
    /// each junction has FKs to both with ON DELETE CASCADE so membership rows clean up automatically.
    pub async fn create_m2m_relations(&self, model: &ResolvedModel) -> Result<(), DbError> {
        for f in &model.fields {
            if let FieldKind::Many2many { target, relation, column, target_column } = f.kind {
                let target_table = target.replace('.', "_");
                let ddl = format!(
                    "CREATE TABLE IF NOT EXISTS {rel} (\
                     {col} bigint NOT NULL REFERENCES {this}(id) ON DELETE CASCADE, \
                     {tc} bigint NOT NULL REFERENCES {tgt}(id) ON DELETE CASCADE, \
                     PRIMARY KEY ({col}, {tc}))",
                    rel = relation, col = column, this = model.table, tc = target_column, tgt = target_table
                );
                sqlx::query(&ddl).execute(&self.pool).await?;
            }
        }
        Ok(())
    }

    /// RAW count with no access control (equivalent to superuser). Apps should use
    /// [`Db::count_secured`]; this is for tests/admin where security is applied elsewhere.
    pub async fn count_where(&self, model: &ResolvedModel, domain: &Domain) -> Result<i64, DbError> {
        let filter = domain.compile(model)?;
        let sql = format!("SELECT COUNT(*) FROM {} WHERE {}", model.table, filter.where_clause);
        let q = bind_all(sqlx::query_scalar::<Postgres, i64>(&sql), &filter.params);
        Ok(q.fetch_one(&self.pool).await?)
    }

    /// Counts rows visible to `ctx` under the security policy: ACL must grant Read, and the
    /// record-rule domain is AND-ed with the optional caller `filter`.
    pub async fn count_secured(
        &self,
        model: &ResolvedModel,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        filter: Option<&Domain>,
    ) -> Result<i64, DbError> {
        let dom = self.secured_read_domain(model, ctx, acls, rules, filter)?;
        self.count_where(model, &dom).await
    }

    /// Returns the rows of `model` visible to `ctx` as JSON objects (one per row, field→value),
    /// enforcing ACL + record rules. The same secured WHERE as the count/id variants.
    pub async fn find_secured(
        &self,
        model: &ResolvedModel,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        filter: Option<&Domain>,
    ) -> Result<Vec<Json>, DbError> {
        let dom = self.secured_read_domain(model, ctx, acls, rules, filter)?;
        let f = dom.compile(model)?;
        let sql = format!(
            "SELECT {} FROM {} WHERE {} ORDER BY id",
            select_columns(model),
            model.table,
            f.where_clause
        );
        let mut q = sqlx::query(&sql);
        for p in &f.params {
            q = match p {
                Value::Str(s) => q.bind(s.clone()),
                Value::Int(n) => q.bind(*n),
                Value::Float(x) => q.bind(*x),
                Value::Decimal(d) => q.bind(*d),
                Value::Bool(b) => q.bind(*b),
                Value::Null => q.bind(Option::<String>::None),
                Value::List(_) => q,
            };
        }
        let rows = q.fetch_all(&self.pool).await?;
        rows.iter().map(|r| project_row(model, ctx, r)).collect()
    }

    /// A page of rows visible to `ctx` under the security policy, with optional `filter`, `order`
    /// (field, descending) and limit/offset — plus the total count under the SAME secured domain.
    /// Order fields are validated against the model's columns, so the ORDER BY uses only
    /// model-controlled identifiers (never user strings); limit/offset are bound parameters.
    pub async fn list_secured(
        &self,
        model: &ResolvedModel,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        filter: Option<&Domain>,
        order: &[(String, bool)],
        limit: i64,
        offset: i64,
    ) -> Result<ListPage, DbError> {
        let dom = self.secured_read_domain(model, ctx, acls, rules, filter)?;
        let total = self.count_where(model, &dom).await?;

        let mut order_sql = String::new();
        for (field, desc) in order {
            let ok = field.as_str() == "id"
                || model.fields.iter().any(|f| f.name == field.as_str() && f.has_column());
            if !ok {
                return Err(DbError::BadInput(format!("cannot order by unknown field '{field}'")));
            }
            // D6: don't let a non-member order by a field they can't read (it would leak ordering).
            if !field_accessible(model.name, field, ctx) {
                return Err(DbError::AccessDenied {
                    model: model.name.to_string(),
                    operation: "order by (restricted field)",
                });
            }
            if !order_sql.is_empty() {
                order_sql.push_str(", ");
            }
            order_sql.push_str(field);
            order_sql.push_str(if *desc { " DESC" } else { " ASC" });
        }
        if order_sql.is_empty() {
            order_sql.push_str("id");
        }

        let f = dom.compile(model)?;
        let n = f.params.len();
        let sql = format!(
            "SELECT {} FROM {} WHERE {} ORDER BY {} LIMIT ${} OFFSET ${}",
            select_columns(model),
            model.table,
            f.where_clause,
            order_sql,
            n + 1,
            n + 2,
        );
        let mut q = sqlx::query(&sql);
        for p in &f.params {
            q = bind_query(q, p);
        }
        q = q.bind(limit).bind(offset);
        let rows = q.fetch_all(&self.pool).await?;
        let data = rows.iter().map(|r| project_row(model, ctx, r)).collect::<Result<Vec<_>, _>>()?;
        Ok(ListPage { data, total })
    }

    /// Like [`Db::count_secured`] but returns the ids of the visible rows (ordered).
    pub async fn find_ids_secured(
        &self,
        model: &ResolvedModel,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        filter: Option<&Domain>,
    ) -> Result<Vec<i64>, DbError> {
        let dom = self.secured_read_domain(model, ctx, acls, rules, filter)?;
        let f = dom.compile(model)?;
        let sql = format!("SELECT id FROM {} WHERE {} ORDER BY id", model.table, f.where_clause);
        let q = bind_all(sqlx::query_scalar::<Postgres, i64>(&sql), &f.params);
        Ok(q.fetch_all(&self.pool).await?)
    }

    /// Reads one visible row by id with its One2many children INLINED. Each child set is read
    /// through the secured path (its own Read ACL + record rules), so children the caller cannot
    /// read are filtered out; a child model the caller cannot read at all is omitted rather than
    /// failing the parent read. Returns None if the row does not exist or the caller cannot read it.
    pub async fn find_one_secured(
        &self,
        model: &ResolvedModel,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        id: i64,
    ) -> Result<Option<Json>, DbError> {
        if !check_access(Operation::Read, model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: model.name.to_string(), operation: "read" });
        }
        // Parent row, restricted by the Read record rule. `id` is not a domain field, so it is bound
        // raw as $1 and the rule (if any) compiles into $2.. via compile_into.
        let mut params: Vec<Value> = vec![Value::Int(id)];
        let mut where_sql = match record_rule_domain(Operation::Read, model.name, ctx, rules) {
            Some(rule) => format!("id = $1 AND {}", rule.compile_into(model, &mut params)?),
            None => "id = $1".to_string(),
        };
        where_sql.push_str(&company_clause(model, ctx, &mut params)?);
        let sql =
            format!("SELECT {} FROM {} WHERE {}", select_columns(model), model.table, where_sql);
        let mut q = sqlx::query(&sql);
        for p in &params {
            q = bind_query(q, p);
        }
        let row = match q.fetch_optional(&self.pool).await? {
            Some(r) => r,
            None => return Ok(None),
        };
        let mut obj = match row_to_json(model, &row)? {
            Json::Object(o) => o,
            _ => return Ok(None),
        };
        strip_unreadable(model, ctx, &mut obj); // D6: drop fields the caller may not read
        // Inline each One2many field as an array of the caller's visible child rows.
        for f in &model.fields {
            if let FieldKind::One2many { target, inverse } = f.kind {
                // D6: the One2many field itself can be restricted — omit the whole relation if the
                // caller cannot read it (strip_unreadable can't catch this: the key is added here,
                // AFTER stripping, and One2many has no column so it was never in the stripped object).
                if !field_accessible(model.name, f.name, ctx) {
                    continue;
                }
                let child = match resolve_registered(target) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if !check_access(Operation::Read, child.name, ctx, acls) {
                    continue; // caller cannot read the child model → omit the relation
                }
                let cdom = Domain::field(inverse).eq(id);
                let children = self.find_secured(&child, ctx, acls, rules, Some(&cdom)).await?;
                obj.insert(f.name.to_string(), Json::Array(children));
            }
        }
        Ok(Some(Json::Object(obj)))
    }

    /// Builds the effective read domain: deny if the ACL forbids Read, else AND the caller's
    /// filter with the record-rule restriction for this context.
    fn secured_read_domain(
        &self,
        model: &ResolvedModel,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        filter: Option<&Domain>,
    ) -> Result<Domain, DbError> {
        if !check_access(Operation::Read, model.name, ctx, acls) {
            return Err(DbError::AccessDenied {
                model: model.name.to_string(),
                operation: "read",
            });
        }
        // D6: a caller-supplied filter must not reference a field the caller cannot read, else a
        // restricted field could be probed (e.g. `secret = 'x'`) even though its value is stripped.
        // Relational paths are walked hop-by-hop, so `partner_id.secret` is rejected when `secret`
        // is restricted on the TARGET model — not just the first segment.
        if let Some(f) = filter {
            for path in f.condition_paths() {
                if !filter_path_accessible(model, path, ctx) {
                    return Err(DbError::AccessDenied {
                        model: model.name.to_string(),
                        operation: "filter (restricted field)",
                    });
                }
            }
        }
        let rule = record_rule_domain(Operation::Read, model.name, ctx, rules);
        let base = match (filter, rule) {
            (Some(f), Some(r)) => f.clone().and(r),
            (Some(f), None) => f.clone(),
            (None, Some(r)) => r,
            (None, None) => Domain::True,
        };
        // Multi-company: restrict to the caller's companies for company-scoped models.
        Ok(match company_filter(model, ctx) {
            Some(cf) => base.and(cf),
            None => base,
        })
    }

    /// Inserts a row from validated `values`, enforcing ACL Create. If a Create record rule
    /// applies, the new row must satisfy it or the insert is rolled back. Returns the new id.
    pub async fn insert_secured(
        &self,
        model: &ResolvedModel,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        values: &Map<String, Json>,
    ) -> Result<i64, DbError> {
        let mut tx = self.pool.begin().await?;
        let (id, record) = self.insert_secured_in_tx(model, ctx, acls, rules, values, &mut tx).await?;
        tx.commit().await?;
        // Grandparents are a separate aggregate (single-level by design); recompute post-commit. The
        // call is idempotent (it reads current state), so a retry repairs it.
        self.recompute_parents_of(model, &record).await?;
        Ok(id)
    }

    /// The full secured create — ACL + D6, payload split, `_inherits` parent create/update, the row
    /// itself, nested One2many children, and Many2many sets — all on the caller's transaction `tx`,
    /// returning the new id and its column record. [`Db::insert_secured`] wraps this with begin/commit
    /// and the post-commit grandparent recompute; the variant generator calls it repeatedly on ONE
    /// transaction so a whole batch of variants (and their join rows) commits atomically.
    async fn insert_secured_in_tx(
        &self,
        model: &ResolvedModel,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        values: &Map<String, Json>,
        tx: &mut sqlx::Transaction<'_, Postgres>,
    ) -> Result<(i64, BTreeMap<String, Value>), DbError> {
        if !check_access(Operation::Create, model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: model.name.to_string(), operation: "create" });
        }
        check_writable_fields(model, ctx, values)?; // D6: reject fields the caller may not write
        // Split the payload: scalar columns, One2many child-create payloads, Many2many sets, and
        // (_inherits) delegated parent fields.
        let (mut scalars, nested, m2m, delegated) = split_nested(model, values)?;

        // _inherits: the required `via` FK must point at a parent carrying the delegated fields. If the
        // caller gave `via`, update that existing parent with the delegated keys; otherwise auto-create
        // the parent (with the delegated fields) FIRST and point `via` at it — all in this transaction,
        // so a child failure rolls the parent back too (no orphan template).
        if let Some((parent, via)) = inherits_of(model.name) {
            let parent_model = resolve_registered(parent).map_err(DbError::BadInput)?;
            let has_via = scalars.get(via).is_some_and(|v| !v.is_null());
            if has_via {
                if !delegated.is_empty() {
                    let pid = scalars.get(via).and_then(|v| v.as_i64()).ok_or_else(|| {
                        DbError::BadInput(format!("_inherits via '{via}' must be an integer id"))
                    })?;
                    self.update_delegated_parent(&parent_model, ctx, acls, rules, pid, &delegated, tx).await?;
                }
            } else {
                if !check_access(Operation::Create, parent_model.name, ctx, acls) {
                    return Err(DbError::AccessDenied {
                        model: parent_model.name.to_string(),
                        operation: "create (inherited parent)",
                    });
                }
                check_writable_fields(&parent_model, ctx, &delegated)?;
                let mut pscalars = delegated.clone();
                let (pid, _) =
                    self.insert_scalars_in_tx(&parent_model, ctx, rules, &mut pscalars, tx).await?;
                scalars.insert(via.to_string(), Json::from(pid));
            }
        }

        // Insert the child row itself (full secured-create treatment), in the same transaction.
        let (id, record) = self.insert_scalars_in_tx(model, ctx, rules, &mut scalars, tx).await?;

        // Nested One2many children: create-only on a brand-new parent, in the SAME transaction with
        // child ACL + record rules re-checked. Then recompute this parent's own aggregate from the
        // just-inserted children IN THIS TRANSACTION, so the row, children and aggregate commit
        // atomically. The parent is brand-new (its id is invisible to other txns) → no advisory lock.
        if !nested.is_empty() {
            self.apply_nested_in_tx(tx, ctx, acls, rules, &nested, id, false).await?;
            recompute_columns_on(tx, model, id).await?;
        }
        // Many2many sets, in the same transaction (atomic with the row + children).
        if !m2m.is_empty() {
            apply_m2m_in_tx(tx, id, &m2m).await?;
        }
        // @api.constrains: validate the new record (+ its children) in-tx; a violation rolls it back.
        // On create the whole record is new, so every constraint runs.
        if has_constraints(model.name) {
            check_constraints_in_tx(model, tx, id, None).await?;
        }
        Ok((id, record))
    }

    /// Inserts a row from a SCALAR-only payload inside `tx` with the full secured-create treatment
    /// (company-scope, defaults, required/type validation, stored computes, the create record-rule),
    /// returning the new id and its column record. No nested/Many2many handling. Shared by the normal
    /// create and the `_inherits` parent auto-create. ACL + D6 are checked by the caller.
    async fn insert_scalars_in_tx(
        &self,
        model: &ResolvedModel,
        ctx: &Ctx,
        rules: &[RecordRule],
        scalars: &mut Map<String, Json>,
        tx: &mut sqlx::Transaction<'_, Postgres>,
    ) -> Result<(i64, BTreeMap<String, Value>), DbError> {
        apply_company_scope(model, ctx, scalars, true)?;
        apply_defaults(model, scalars);
        let mut cols = validate_write_values(model, scalars, true)?;
        // Seed read-only fields' defaults (e.g. on-hand = 0) as columns — they are excluded from the
        // user-writable path above, so the framework supplies their initial value here.
        cols.extend(readonly_default_columns(model, &cols)?);
        if cols.is_empty() {
            return Err(DbError::BadInput("no values provided".to_string()));
        }
        let mut record: BTreeMap<String, Value> =
            cols.into_iter().map(|(c, v)| (c.to_string(), v)).collect();
        compute_stored(model, &mut record, &Children::new());
        let (names, vals): (Vec<&str>, Vec<Value>) =
            record.iter().map(|(k, v)| (k.as_str(), v.clone())).unzip();
        let placeholders: Vec<String> =
            names.iter().enumerate().map(|(i, c)| format!("${}::{}", i + 1, col_cast(model, c))).collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}) RETURNING id",
            model.table,
            names.join(", "),
            placeholders.join(", ")
        );
        let mut q = sqlx::query_scalar::<Postgres, i64>(&sql);
        q = bind_all(q, &vals);
        let id: i64 = q.fetch_one(&mut **tx).await?;
        if let Some(rule) = record_rule_domain(Operation::Create, model.name, ctx, rules) {
            let mut params: Vec<Value> = vec![Value::Int(id)];
            let where_sql = rule.compile_into(model, &mut params)?;
            let check = format!("SELECT 1 FROM {} WHERE id = $1 AND {}", model.table, where_sql);
            let mut cq = sqlx::query(&check);
            for v in &params {
                cq = bind_query(cq, v);
            }
            if cq.fetch_optional(&mut **tx).await?.is_none() {
                return Err(DbError::AccessDenied {
                    model: model.name.to_string(),
                    operation: "create (record rule)",
                });
            }
        }
        Ok((id, record))
    }

    /// Writes delegated `_inherits` fields onto the parent record `pid` inside `tx`: the parent's own
    /// Write ACL + D6 + company-scope + Write record-rule are enforced (writing a variant's inherited
    /// field is a write to the shared template, so it needs template write access). Recomputes the
    /// parent's stored computes if any. A 0-row update (parent missing / not permitted) is an error.
    #[allow(clippy::too_many_arguments)]
    async fn update_delegated_parent(
        &self,
        parent_model: &ResolvedModel,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        pid: i64,
        delegated: &Map<String, Json>,
        tx: &mut sqlx::Transaction<'_, Postgres>,
    ) -> Result<(), DbError> {
        if !check_access(Operation::Write, parent_model.name, ctx, acls) {
            return Err(DbError::AccessDenied {
                model: parent_model.name.to_string(),
                operation: "write (inherited parent)",
            });
        }
        check_writable_fields(parent_model, ctx, delegated)?; // D6 on parent fields
        let mut d = delegated.clone();
        apply_company_scope(parent_model, ctx, &mut d, false)?;
        let cols = validate_write_values(parent_model, &d, false)?;
        if cols.is_empty() {
            return Ok(());
        }
        let set: Vec<String> = cols
            .iter()
            .enumerate()
            .map(|(i, (c, _))| format!("{} = ${}::{}", c, i + 1, col_cast(parent_model, c)))
            .collect();
        let id_ph = cols.len() + 1;
        let mut params: Vec<Value> = cols.iter().map(|(_, v)| v.clone()).collect();
        params.push(Value::Int(pid));
        let mut where_sql = match record_rule_domain(Operation::Write, parent_model.name, ctx, rules) {
            Some(rule) => format!("id = ${id_ph} AND {}", rule.compile_into(parent_model, &mut params)?),
            None => format!("id = ${id_ph}"),
        };
        where_sql.push_str(&company_clause(parent_model, ctx, &mut params)?);
        let sql = format!("UPDATE {} SET {} WHERE {}", parent_model.table, set.join(", "), where_sql);
        let mut q = sqlx::query(&sql);
        for v in &params {
            q = bind_query(q, v);
        }
        if q.execute(&mut **tx).await?.rows_affected() == 0 {
            return Err(DbError::AccessDenied {
                model: parent_model.name.to_string(),
                operation: "write (inherited parent not found or not permitted)",
            });
        }
        // Serialize the parent's aggregate recompute per row (same advisory lock as the other write
        // paths), so concurrent variant writes to the shared template can't lose-update its computes.
        if !computed_fields(parent_model).is_empty() {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
                .bind(format!("agg:{}:{}", parent_model.table, pid))
                .execute(&mut **tx)
                .await?;
            recompute_columns_on(tx, parent_model, pid).await?;
        }
        Ok(())
    }

    /// Updates row `id` with validated `values`, enforcing ACL Write and the Write record rule
    /// (rows outside the rule are not matched → 0 affected). Returns the number of rows updated.
    pub async fn update_secured(
        &self,
        model: &ResolvedModel,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        id: i64,
        values: &Map<String, Json>,
    ) -> Result<u64, DbError> {
        if !check_access(Operation::Write, model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: model.name.to_string(), operation: "write" });
        }
        check_writable_fields(model, ctx, values)?; // D6: reject fields the caller may not write
        // Split scalar fields from One2many child commands (D4), Many2many sets, and (_inherits)
        // delegated parent fields.
        let (mut scalars, nested, m2m, delegated) = split_nested(model, values)?;
        // Multi-company: a scoped caller may not reassign a row into a foreign company or NULL.
        apply_company_scope(model, ctx, &mut scalars, false)?;
        let cols = validate_write_values(model, &scalars, false)?;
        if cols.is_empty() && nested.is_empty() && m2m.is_empty() && delegated.is_empty() {
            return Err(DbError::BadInput("no values provided".to_string()));
        }
        // _inherits: re-pointing the parent link (`via`) AND writing inherited fields in one call is
        // ambiguous (which parent receives the inherited write?) — reject it rather than silently
        // retarget the delegated write to the newly-linked parent.
        if !delegated.is_empty() {
            if let Some((_parent, via)) = inherits_of(model.name) {
                if cols.iter().any(|(c, _)| *c == via) {
                    return Err(DbError::BadInput(
                        "cannot change the inherited parent link and write inherited fields in the same update".to_string(),
                    ));
                }
            }
        }

        // Field tracking (mail): the tracked scalar columns actually present in this write. Only for
        // mailed models; computed-field tracking is deferred.
        let track_cols: Vec<&'static str> = if is_mailed(model.name) {
            let tracked = tracked_fields(model.name);
            cols.iter().map(|(c, _)| *c).filter(|c| tracked.contains(c)).collect()
        } else {
            Vec::new()
        };

        let mut tx = self.pool.begin().await?;
        // Parents this row points to BEFORE the write (re-parenting uses before + after).
        let before = self.parent_targets(model, id).await?;

        // Snapshot the OLD text of tracked columns, locking the row (FOR UPDATE) so the snapshot is
        // exactly what THIS write overwrites — no stale/lost old value under a concurrent writer. The
        // NEW values are re-read the same way (Postgres `::text`) after the UPDATE, so old/new diff in
        // ONE representation: no false positive from Date/Datetime/Float/Decimal text-format mismatch.
        let old_text = snapshot_text(&mut tx, model.table, &track_cols, id, true).await?;

        // 1) Scalar UPDATE of the provided columns (computed columns recomputed in step 3); the Write
        //    rule + company scope are enforced in the WHERE.
        let mut affected = 1u64;
        if !cols.is_empty() {
            let set: Vec<String> =
                cols.iter().enumerate().map(|(i, (c, _))| format!("{} = ${}::{}", c, i + 1, col_cast(model, c))).collect();
            let id_ph = cols.len() + 1;
            let mut params: Vec<Value> = cols.iter().map(|(_, v)| v.clone()).collect();
            params.push(Value::Int(id));
            let mut where_sql = match record_rule_domain(Operation::Write, model.name, ctx, rules) {
                Some(rule) => format!("id = ${id_ph} AND {}", rule.compile_into(model, &mut params)?),
                None => format!("id = ${id_ph}"),
            };
            where_sql.push_str(&company_clause(model, ctx, &mut params)?);
            let sql = format!("UPDATE {} SET {} WHERE {}", model.table, set.join(", "), where_sql);
            let mut q = sqlx::query(&sql);
            for v in &params {
                q = bind_query(q, v);
            }
            affected = q.execute(&mut *tx).await?.rows_affected();
            if affected == 0 {
                return Ok(0); // no such row / not permitted → tx rolls back on drop
            }
        } else {
            // Nested-only write: confirm the parent is writable by this caller before touching its
            // children.
            let mut params: Vec<Value> = vec![Value::Int(id)];
            let mut where_sql = match record_rule_domain(Operation::Write, model.name, ctx, rules) {
                Some(rule) => format!("id = $1 AND {}", rule.compile_into(model, &mut params)?),
                None => "id = $1".to_string(),
            };
            where_sql.push_str(&company_clause(model, ctx, &mut params)?);
            let check = format!("SELECT 1 FROM {} WHERE {}", model.table, where_sql);
            let mut q = sqlx::query(&check);
            for v in &params {
                q = bind_query(q, v);
            }
            if q.fetch_optional(&mut *tx).await?.is_none() {
                return Ok(0);
            }
        }

        // 2) Apply the nested child commands (create/update/delete) in the same transaction.
        if !nested.is_empty() {
            self.apply_nested_in_tx(&mut tx, ctx, acls, rules, &nested, id, true).await?;
        }
        // 2b) Apply Many2many sets (the row was confirmed writable above).
        if !m2m.is_empty() {
            apply_m2m_in_tx(&mut tx, id, &m2m).await?;
        }
        // 2c) _inherits: write delegated keys through to the parent (read this row's via FK, then
        //     UPDATE the parent). The child was confirmed writable above; the parent enforces its own
        //     ACL/rule. Same transaction, so child + parent changes commit together.
        if !delegated.is_empty() {
            if let Some((parent, via)) = inherits_of(model.name) {
                let parent_model = resolve_registered(parent).map_err(DbError::BadInput)?;
                let pid: Option<i64> =
                    sqlx::query_scalar::<Postgres, i64>(&format!("SELECT {via} FROM {} WHERE id = $1", model.table))
                        .bind(id)
                        .fetch_optional(&mut *tx)
                        .await?;
                let pid = pid.ok_or_else(|| {
                    DbError::BadInput(format!("_inherits via '{via}' is null on '{}'", model.name))
                })?;
                self.update_delegated_parent(&parent_model, ctx, acls, rules, pid, &delegated, &mut tx).await?;
            }
        }

        // 3) Recompute this row's computed columns (same-record + aggregate over its children),
        //    in-tx and serialized per row so concurrent child writes cannot lose-update the aggregate.
        if !computed_fields(model).is_empty() {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
                .bind(format!("agg:{}:{}", model.table, id))
                .execute(&mut *tx)
                .await?;
            recompute_columns_on(&mut tx, model, id).await?;
        }

        // @api.constrains: validate the updated record (+ children) in-tx, AFTER the recompute so the
        // constraint sees final computed values; a violation rolls the whole write back. Depends-scoped:
        // a constraint runs only if one of its trigger fields changed — the caller-written fields PLUS
        // the model's stored computed fields (which the recompute may have just changed and which are
        // never in the payload), so a constraint may trigger on a computed total. Scope note: only the
        // top-level written model's constraints run here — constraints on a child written via the
        // parent's nested commands, or on an _inherits parent, are not evaluated (v1).
        if has_constraints(model.name) {
            let mut changed: Vec<String> = values.keys().cloned().collect();
            changed.extend(computed_fields(model).iter().map(|c| c.to_string()));
            check_constraints_in_tx(model, &mut tx, id, Some(&changed)).await?;
        }

        // M15.1: a PTAV `price_extra` edit re-materializes `price_extra` on every variant whose combo
        // includes this cell (the Many2many aggregate is stored, not computed on read). In-tx, so the
        // refresh commits atomically with the edit.
        if model.name == VG_PTAV && affected > 0 && cols.iter().any(|(c, _)| *c == "price_extra") {
            let variant = resolve_registered(VG_VARIANT).map_err(DbError::BadInput)?;
            let ptav = resolve_registered(VG_PTAV).map_err(DbError::BadInput)?;
            let vids: Vec<i64> = sqlx::query_scalar(&format!(
                "SELECT product_id FROM {} WHERE ptav_id = $1",
                VG_VARIANT_PTAV_REL
            ))
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
            for vid in vids {
                self.set_variant_price_extra_in_tx(&variant, &ptav, &mut tx, vid).await?;
            }
        }

        // Re-read the NEW text of tracked columns on the still-locked row (same `::text` rendering as
        // the old snapshot). Tracked columns are non-computed, so the recompute above can't touch them.
        let new_text = snapshot_text(&mut tx, model.table, &track_cols, id, false).await?;
        tx.commit().await?;

        // 4) Re-parenting: if this row is itself a child whose FK moved, recompute the old + new
        //    aggregate parents (deduped).
        let after = self.parent_targets(model, id).await?;
        let mut seen: Vec<(&'static str, i64)> = Vec::new();
        for (parent, pid) in before.into_iter().chain(after) {
            if !seen.iter().any(|&(n, p)| n == parent.name && p == pid) {
                seen.push((parent.name, pid));
                self.recompute_parent(&parent, pid).await?;
            }
        }

        // 5) Field tracking: diff old vs new for tracked columns and record a chatter audit entry.
        //    Best-effort and post-commit — the write is already durable, so a missing mail schema or
        //    a tracking failure is logged, never propagated (would mislead the caller into a retry).
        if !track_cols.is_empty() {
            let mut changes: Vec<(String, Option<String>, Option<String>)> = Vec::new();
            for c in &track_cols {
                let old_t = old_text.iter().find(|(k, _)| k == c).and_then(|(_, o)| o.clone());
                let new_t = new_text.iter().find(|(k, _)| k == c).and_then(|(_, o)| o.clone());
                if old_t != new_t {
                    changes.push((c.to_string(), old_t, new_t));
                }
            }
            if let Err(e) = self.write_tracking(model.name, id, ctx.uid, &changes).await {
                eprintln!("meshble-db tracking write failed (write committed): {e:?}");
            }
        }
        Ok(affected)
    }

    /// Applies a parent's One2many child commands in `tx`. Create is always allowed; Update/Delete
    /// only when `allow_existing` (a brand-new parent has no children to edit). Each command
    /// re-checks the child model's ACL + record rules, and Update/Delete verify the child belongs to
    /// THIS parent (so nesting can't reach another parent's rows).
    #[allow(clippy::too_many_arguments)]
    async fn apply_nested_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        nested: &[NestedWrite],
        parent_id: i64,
        allow_existing: bool,
    ) -> Result<(), DbError> {
        for nw in nested {
            for cmd in &nw.commands {
                match cmd {
                    O2mCommand::Create(values) => {
                        self.create_child_in_tx(tx, nw, parent_id, ctx, acls, rules, values.clone())
                            .await?
                    }
                    O2mCommand::Update(cid, values) => {
                        if !allow_existing {
                            return Err(DbError::BadInput(
                                "a new record's children can only be created".to_string(),
                            ));
                        }
                        self.update_child_in_tx(tx, nw, parent_id, ctx, acls, rules, *cid, values.clone())
                            .await?
                    }
                    O2mCommand::Delete(cid) => {
                        if !allow_existing {
                            return Err(DbError::BadInput(
                                "a new record's children can only be created".to_string(),
                            ));
                        }
                        self.delete_child_in_tx(tx, nw, parent_id, ctx, acls, rules, *cid).await?
                    }
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_child_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        nw: &NestedWrite,
        parent_id: i64,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        mut cvals: Map<String, Json>,
    ) -> Result<(), DbError> {
        if !check_access(Operation::Create, nw.child.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: nw.child.name.to_string(), operation: "create" });
        }
        check_writable_fields(&nw.child, ctx, &cvals)?; // D6: child fields the caller may not write
        cvals.insert(nw.inverse.to_string(), Json::from(parent_id)); // parent owns the FK
        apply_company_scope(&nw.child, ctx, &mut cvals, true)?;
        apply_defaults(&nw.child, &mut cvals);
        let ccols = validate_write_values(&nw.child, &cvals, true)?;
        let mut crec: BTreeMap<String, Value> =
            ccols.into_iter().map(|(c, v)| (c.to_string(), v)).collect();
        compute_stored(&nw.child, &mut crec, &Children::new());
        let (cn, cv): (Vec<&str>, Vec<Value>) =
            crec.iter().map(|(k, v)| (k.as_str(), v.clone())).unzip();
        let cph: Vec<String> =
            cn.iter().enumerate().map(|(i, c)| format!("${}::{}", i + 1, col_cast(&nw.child, c))).collect();
        let csql = format!(
            "INSERT INTO {} ({}) VALUES ({}) RETURNING id",
            nw.child.table,
            cn.join(", "),
            cph.join(", ")
        );
        let mut cq = sqlx::query_scalar::<Postgres, i64>(&csql);
        cq = bind_all(cq, &cv);
        let child_id: i64 = cq.fetch_one(&mut **tx).await?;
        // The child's own Create record rule must hold too (nesting is not a weaker path).
        if let Some(rule) = record_rule_domain(Operation::Create, nw.child.name, ctx, rules) {
            let mut params: Vec<Value> = vec![Value::Int(child_id)];
            let where_sql = rule.compile_into(&nw.child, &mut params)?;
            let check = format!("SELECT 1 FROM {} WHERE id = $1 AND {}", nw.child.table, where_sql);
            let mut chk = sqlx::query(&check);
            for v in &params {
                chk = bind_query(chk, v);
            }
            if chk.fetch_optional(&mut **tx).await?.is_none() {
                return Err(DbError::AccessDenied {
                    model: nw.child.name.to_string(),
                    operation: "create (record rule)",
                });
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_child_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        nw: &NestedWrite,
        parent_id: i64,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        child_id: i64,
        mut cvals: Map<String, Json>,
    ) -> Result<(), DbError> {
        if !check_access(Operation::Write, nw.child.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: nw.child.name.to_string(), operation: "write" });
        }
        check_writable_fields(&nw.child, ctx, &cvals)?; // D6: child fields the caller may not write
        cvals.remove(nw.inverse); // the parent owns the link; re-parenting via nesting is not allowed
        apply_company_scope(&nw.child, ctx, &mut cvals, false)?;
        let cols = validate_write_values(&nw.child, &cvals, false)?;
        if cols.is_empty() {
            return Ok(());
        }
        // ONE generic error for not-found / wrong-parent / rule-denied, so the nested update path is
        // not an oracle for enumerating child ids the caller does not own.
        let denied = format!("cannot update line {child_id}: not found or not permitted");
        // Read the current child (verifying ownership) for the same-record recompute.
        let mut record =
            read_record_on(&mut **tx, &nw.child, child_id).await?.ok_or_else(|| DbError::BadInput(denied.clone()))?;
        if record.get(nw.inverse) != Some(&Value::Int(parent_id)) {
            return Err(DbError::BadInput(denied));
        }
        for (c, v) in &cols {
            record.insert(c.to_string(), v.clone());
        }
        let computed = computed_fields(&nw.child);
        if !computed.is_empty() {
            let gchildren = read_children_on(&mut **tx, &nw.child, child_id).await?;
            compute_stored(&nw.child, &mut record, &gchildren);
        }
        let mut set_pairs: Vec<(String, Value)> =
            cols.iter().map(|(c, v)| (c.to_string(), v.clone())).collect();
        for name in &computed {
            if let Some(v) = record.get(*name) {
                set_pairs.push((name.to_string(), v.clone()));
            }
        }
        let set: Vec<String> =
            set_pairs.iter().enumerate().map(|(i, (c, _))| format!("{} = ${}::{}", c, i + 1, col_cast(&nw.child, c))).collect();
        let id_ph = set_pairs.len() + 1;
        let mut params: Vec<Value> = set_pairs.iter().map(|(_, v)| v.clone()).collect();
        params.push(Value::Int(child_id));
        params.push(Value::Int(parent_id));
        let mut where_sql = format!("id = ${id_ph} AND {} = ${}", nw.inverse, id_ph + 1);
        if let Some(rule) = record_rule_domain(Operation::Write, nw.child.name, ctx, rules) {
            where_sql.push_str(&format!(" AND {}", rule.compile_into(&nw.child, &mut params)?));
        }
        where_sql.push_str(&company_clause(&nw.child, ctx, &mut params)?); // the child is company-scoped too
        let sql = format!("UPDATE {} SET {} WHERE {}", nw.child.table, set.join(", "), where_sql);
        let mut q = sqlx::query(&sql);
        for v in &params {
            q = bind_query(q, v);
        }
        if q.execute(&mut **tx).await?.rows_affected() == 0 {
            return Err(DbError::BadInput(denied));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn delete_child_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        nw: &NestedWrite,
        parent_id: i64,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        child_id: i64,
    ) -> Result<(), DbError> {
        if !check_access(Operation::Delete, nw.child.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: nw.child.name.to_string(), operation: "delete" });
        }
        // Ownership (child belongs to THIS parent) + the child's Delete record rule + company scope.
        let mut params: Vec<Value> = vec![Value::Int(child_id), Value::Int(parent_id)];
        let mut where_sql = format!("id = $1 AND {} = $2", nw.inverse);
        if let Some(rule) = record_rule_domain(Operation::Delete, nw.child.name, ctx, rules) {
            where_sql.push_str(&format!(" AND {}", rule.compile_into(&nw.child, &mut params)?));
        }
        where_sql.push_str(&company_clause(&nw.child, ctx, &mut params)?);
        let sql = format!("DELETE FROM {} WHERE {}", nw.child.table, where_sql);
        let mut q = sqlx::query(&sql);
        for v in &params {
            q = bind_query(q, v);
        }
        if q.execute(&mut **tx).await?.rows_affected() == 0 {
            return Err(DbError::BadInput(format!(
                "cannot delete line {child_id}: not a child of this record or not permitted"
            )));
        }
        Ok(())
    }

    /// Runs a registered state-transition action on row `id`: enforces ACL Write + the action's group
    /// guard + record-rule visibility, runs the pure action fn over the current record, resolves any
    /// sequence assignment (gapless numbering), and applies the resulting updates through the secured
    /// write path (so record rules + company scope are re-checked on the write).
    pub async fn run_action(
        &self,
        model: &ResolvedModel,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        id: i64,
        action_name: &str,
    ) -> Result<(), DbError> {
        let action = action_for(model.name, action_name).ok_or_else(|| {
            DbError::BadInput(format!("unknown action '{action_name}' on '{}'", model.name))
        })?;
        if !check_access(Operation::Write, model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: model.name.to_string(), operation: "action" });
        }
        if !action.groups.is_empty() && !ctx.is_su() && !action.groups.iter().any(|g| ctx.is_member(g)) {
            return Err(DbError::AccessDenied { model: model.name.to_string(), operation: "action (group)" });
        }
        // The row must be visible to the caller (so the action's guards read real values).
        if self.find_one_secured(model, ctx, acls, rules, id).await?.is_none() {
            return Err(DbError::BadInput("record not found or not permitted".to_string()));
        }
        let mut conn = self.pool.acquire().await?;
        let record = read_record_on(&mut conn, model, id)
            .await?
            .ok_or_else(|| DbError::BadInput("record not found".to_string()))?;
        drop(conn);

        let outcome = (action.func)(&ActionInput::new(&record)).map_err(DbError::BadInput)?;
        let mut updates: Map<String, Json> =
            outcome.set.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect();
        if let Some((field, code)) = &outcome.assign_sequence {
            updates.insert(field.clone(), Json::from(self.next_value(code).await?));
        }
        if updates.is_empty() {
            return Ok(()); // a guard-only / no-op action
        }
        if self.update_secured(model, ctx, acls, rules, id, &updates).await? == 0 {
            return Err(DbError::BadInput("record not found or not permitted".to_string()));
        }
        Ok(())
    }

    /// Generates `product.product` variants for a template as the cartesian product of its attribute
    /// lines' selected values, reconciling against the variants that already exist: a combination with
    /// a matching variant is kept (reactivated if it was archived), a missing combination is created,
    /// and an active variant whose combination is no longer selected is ARCHIVED (never deleted — it
    /// may carry stock / order history). Idempotent: a regeneration with no attribute change is a no-op.
    ///
    /// Authorization mirrors an action: WRITE on `product.template` — which the ACL already restricts
    /// to managers — plus template visibility. The actual creation then runs ELEVATED (after the gate,
    /// like the mail subsystem): the join rows are engine-owned and not user-writable, and a manager
    /// creating variants implicitly creates them. Every join row and every variant commits in ONE
    /// transaction, so a failure mid-batch leaves no partial variant set.
    pub async fn generate_variants(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        template_id: i64,
    ) -> Result<GenerateOutcome, DbError> {
        let template = resolve_registered(VG_TEMPLATE).map_err(DbError::BadInput)?;
        let variant = resolve_registered(VG_VARIANT).map_err(DbError::BadInput)?;
        let line_model = resolve_registered(VG_LINE).map_err(DbError::BadInput)?;
        let ptav = resolve_registered(VG_PTAV).map_err(DbError::BadInput)?;
        let attribute = resolve_registered(VG_ATTRIBUTE).map_err(DbError::BadInput)?;

        // Gate: WRITE on the template (manager-only via ACL) + the template must be visible to the
        // caller, so generation can't be aimed at a template the caller cannot see.
        if !check_access(Operation::Write, template.name, ctx, acls) {
            return Err(DbError::AccessDenied {
                model: template.name.to_string(),
                operation: "generate_variants",
            });
        }
        if self.find_one_secured(&template, ctx, acls, rules, template_id).await?.is_none() {
            return Err(DbError::BadInput("template not found or not permitted".to_string()));
        }

        // Past the gate, the engine's own reads/writes run elevated (the join rows are not user-writable).
        let su = ctx.sudo();

        // Read the template's attribute lines and their selected values (M2M projected as an id array).
        let lines = self
            .find_secured(&line_model, &su, acls, rules, Some(&Domain::field("product_tmpl_id").eq(template_id)))
            .await?;

        struct Line {
            id: i64,
            attribute_id: i64,
            value_ids: Vec<i64>,
        }
        let mut parsed: Vec<Line> = Vec::new();
        for l in &lines {
            let id = l["id"].as_i64().ok_or_else(|| DbError::BadInput("attribute line missing id".into()))?;
            let attribute_id = l["attribute_id"].as_i64().unwrap_or(0);
            let mut value_ids: Vec<i64> = l["value_ids"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
                .unwrap_or_default();
            value_ids.sort_unstable(); // deterministic combo order, independent of array_agg
            value_ids.dedup();
            if value_ids.is_empty() {
                continue; // a line with no selected values contributes nothing
            }
            parsed.push(Line { id, attribute_id, value_ids });
        }

        // Exclude `no_variant` attributes (informational only — they never multiply variants). Read by
        // id directly: `id` is the implicit PK, not a domain-addressable field.
        let attr_ids: Vec<i64> = parsed.iter().map(|l| l.attribute_id).collect();
        if !attr_ids.is_empty() {
            let sql = format!(
                "SELECT id FROM {} WHERE create_variant = 'no_variant' AND id = ANY($1)",
                attribute.table
            );
            let no_variant: HashSet<i64> = sqlx::query_scalar::<Postgres, i64>(&sql)
                .bind(&attr_ids)
                .fetch_all(&self.pool)
                .await?
                .into_iter()
                .collect();
            parsed.retain(|l| !no_variant.contains(&l.attribute_id));
        }

        // Bound the product before building it (saturating, so a huge product can't overflow usize).
        let mut total: usize = 1;
        for l in &parsed {
            total = total.saturating_mul(l.value_ids.len());
            if total > MAX_VARIANTS {
                return Err(DbError::BadInput(format!("variant count exceeds the cap of {MAX_VARIANTS}")));
            }
        }

        // Cartesian product → each combo is one (line_id, value_id) per line. Zero lines yields a
        // single empty combo (a template with no variant attributes still has one variant — Odoo parity).
        let mut combos: Vec<Vec<(i64, i64)>> = vec![Vec::new()];
        for l in &parsed {
            let mut next = Vec::with_capacity(combos.len() * l.value_ids.len());
            for combo in &combos {
                for &v in &l.value_ids {
                    let mut c = combo.clone();
                    c.push((l.id, v));
                    next.push(c);
                }
            }
            combos = next;
        }

        // Each desired combo is keyed by its sorted set of attribute-VALUE ids — an order-independent
        // identity that survives regeneration (so an existing variant is recognised, not duplicated).
        let mut desired_keys: HashSet<Vec<i64>> = HashSet::new();
        let desired: Vec<(Vec<i64>, &Vec<(i64, i64)>)> = combos
            .iter()
            .map(|c| {
                let mut k: Vec<i64> = c.iter().map(|&(_, v)| v).collect();
                k.sort_unstable();
                k.dedup(); // a true SET, symmetric with the existing-variant key (a degenerate config
                // could select one value on two lines; dedup keeps the keys comparable / idempotent)
                (k, c)
            })
            .collect();

        let mut tx = self.pool.begin().await?;
        // Serialize concurrent generations of the SAME template: without this, two callers could each
        // miss an existing join row in their cell lookup and both insert it (duplicate PTAV cell), and
        // their reconciliations would race. The lock releases at commit and gives this reconciliation a
        // consistent snapshot of the template's current variants.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
            .bind(format!("variants:product_template:{template_id}"))
            .execute(&mut *tx)
            .await?;

        // Snapshot the template's existing variants (active or archived) and the combo each represents,
        // so reconciliation keeps/reactivates matches and archives only the truly-stale ones.
        let mut existing: HashMap<Vec<i64>, Vec<(i64, bool)>> = HashMap::new();
        {
            let vrows = sqlx::query(&format!(
                "SELECT id, active FROM {} WHERE product_tmpl_id = $1",
                variant.table
            ))
            .bind(template_id)
            .fetch_all(&mut *tx)
            .await?;
            let mut active_of: HashMap<i64, bool> = HashMap::new();
            for r in &vrows {
                // NULL-safe: `active` is nullable at the DB level (a default, not NOT NULL), so a row
                // planted with active=null must not panic the decode — treat NULL as active.
                active_of.insert(r.get::<i64, _>("id"), r.get::<Option<bool>, _>("active").unwrap_or(true));
            }
            // Each variant's combo = the set of attribute-value ids behind its PTAV links.
            let mut vset: HashMap<i64, BTreeSet<i64>> = HashMap::new();
            let prows = sqlx::query(&format!(
                "SELECT r.{rel_col} AS vid, p.product_attribute_value_id AS val \
                 FROM {rel} r JOIN {ptav} p ON p.id = r.{rel_target} \
                 WHERE p.product_tmpl_id = $1",
                rel = VG_VARIANT_PTAV_REL,
                rel_col = "product_id",
                rel_target = "ptav_id",
                ptav = ptav.table,
            ))
            .bind(template_id)
            .fetch_all(&mut *tx)
            .await?;
            for r in &prows {
                vset.entry(r.get::<i64, _>("vid")).or_default().insert(r.get::<i64, _>("val"));
            }
            for (&id, &active) in &active_of {
                let key: Vec<i64> =
                    vset.get(&id).map(|s| s.iter().copied().collect()).unwrap_or_default();
                existing.entry(key).or_default().push((id, active));
            }
            // Deterministic survivor among any duplicate variants for one combo: keep the active row
            // with the lowest id (reactivate only when none is active). Without this, the bucket order
            // is HashMap-random and a {active, archived} pair could flip which sibling id is canonical
            // on each regeneration — churning the id that anchors a combo's stock / order history.
            for v in existing.values_mut() {
                v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            }
        }

        let mut cell_ptav: HashMap<(i64, i64), i64> = HashMap::new();
        let mut created: Vec<i64> = Vec::new();
        let mut archived: Vec<i64> = Vec::new();
        let mut kept: Vec<i64> = Vec::new();

        // Desired combos: keep/reactivate an existing variant, or create one. Any duplicate variants
        // for the same desired combo (e.g. from a pre-reconciliation create-only run) are archived so
        // the template converges to exactly one active variant per combination.
        for (key, combo) in &desired {
            desired_keys.insert(key.clone());
            match existing.get(key).filter(|v| !v.is_empty()) {
                Some(variants) => {
                    let (first_id, first_active) = variants[0];
                    if !first_active {
                        self.set_variant_active_in_tx(&variant, &mut tx, first_id, true).await?;
                    }
                    // Regeneration is a full refresh: re-materialize the kept variant's price_extra so a
                    // PTAV price change since the last run is picked up.
                    self.set_variant_price_extra_in_tx(&variant, &ptav, &mut tx, first_id).await?;
                    kept.push(first_id);
                    for &(dup_id, dup_active) in &variants[1..] {
                        if dup_active {
                            self.set_variant_active_in_tx(&variant, &mut tx, dup_id, false).await?;
                            archived.push(dup_id);
                        }
                    }
                }
                None => {
                    let mut ptav_ids: Vec<i64> = Vec::with_capacity(combo.len());
                    for &(line_id, value_id) in combo.iter() {
                        let pid = match cell_ptav.get(&(line_id, value_id)) {
                            Some(&p) => p,
                            None => {
                                let p = self
                                    .ensure_ptav_in_tx(&ptav, &su, acls, rules, &mut tx, template_id, line_id, value_id)
                                    .await?;
                                cell_ptav.insert((line_id, value_id), p);
                                p
                            }
                        };
                        ptav_ids.push(pid);
                    }
                    let payload = serde_json::json!({
                        "product_tmpl_id": template_id,
                        "product_template_attribute_value_ids": ptav_ids,
                    });
                    let (vid, _) = self
                        .insert_secured_in_tx(&variant, &su, acls, rules, payload.as_object().unwrap(), &mut tx)
                        .await?;
                    // Materialize the new variant's price_extra from its just-inserted PTAV set.
                    self.set_variant_price_extra_in_tx(&variant, &ptav, &mut tx, vid).await?;
                    created.push(vid);
                }
            }
        }

        // Stale: active variants whose combo is no longer selected are ARCHIVED, never deleted (they
        // may carry stock / order history). A later regeneration that re-selects the combo reactivates
        // them above (same id, no duplicate).
        for (key, variants) in &existing {
            if !desired_keys.contains(key) {
                for &(id, active) in variants {
                    if active {
                        self.set_variant_active_in_tx(&variant, &mut tx, id, false).await?;
                        archived.push(id);
                    }
                }
            }
        }
        tx.commit().await?;

        Ok(GenerateOutcome { created, archived, kept })
    }

    /// Returns the `product.template.attribute.value` id for (line, value), creating it elevated if
    /// absent. The caller (`generate_variants`) holds a per-template advisory lock, so the
    /// lookup-then-insert is race-free against another generation of the same template.
    #[allow(clippy::too_many_arguments)]
    async fn ensure_ptav_in_tx(
        &self,
        ptav: &ResolvedModel,
        su: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        tx: &mut sqlx::Transaction<'_, Postgres>,
        template_id: i64,
        line_id: i64,
        value_id: i64,
    ) -> Result<i64, DbError> {
        let lookup = format!(
            "SELECT id FROM {} WHERE attribute_line_id = $1 AND product_attribute_value_id = $2",
            ptav.table
        );
        if let Some(id) = sqlx::query_scalar::<Postgres, i64>(&lookup)
            .bind(line_id)
            .bind(value_id)
            .fetch_optional(&mut **tx)
            .await?
        {
            return Ok(id);
        }
        let payload = serde_json::json!({
            "product_tmpl_id": template_id,
            "attribute_line_id": line_id,
            "product_attribute_value_id": value_id,
        });
        let (id, _) =
            self.insert_secured_in_tx(ptav, su, acls, rules, payload.as_object().unwrap(), tx).await?;
        Ok(id)
    }

    /// Materializes a variant's `price_extra` = SUM of its combo PTAVs' `price_extra` — the Many2many
    /// aggregate the compute engine can't do on read. Recomputed only at the two bounded write points
    /// (generation, and a PTAV `price_extra` edit), the M2M analogue of `recompute_columns_on`. The
    /// SUM is taken in-tx so a just-inserted PTAV set is visible. Idempotent.
    async fn set_variant_price_extra_in_tx(
        &self,
        variant: &ResolvedModel,
        ptav: &ResolvedModel,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        variant_id: i64,
    ) -> Result<(), DbError> {
        let sql = format!(
            "UPDATE {v} SET price_extra = COALESCE(\
                 (SELECT SUM(p.price_extra) FROM {rel} r JOIN {ptav} p ON p.id = r.ptav_id \
                  WHERE r.product_id = $1), 0) \
             WHERE id = $1",
            v = variant.table,
            rel = VG_VARIANT_PTAV_REL,
            ptav = ptav.table,
        );
        sqlx::query(&sql).bind(variant_id).execute(&mut **tx).await?;
        Ok(())
    }

    /// Resolves the unit price of a `product.product` variant under a `product.pricelist` for a given
    /// quantity and date. Picks the MOST SPECIFIC applicable rule (variant > product > category >
    /// global), honouring `min_quantity` and the date window, and computes a fixed price or a percentage
    /// discount off the base (the variant's effective sales price `list_price + price_extra`, or its
    /// cost). Falls back to the variant's sales price when no rule matches. The category match walks the
    /// product's category ancestry (capped). Read-only; single currency (no FX — the caller enforces it).
    pub async fn resolve_price(
        &self,
        pricelist_id: i64,
        variant_id: i64,
        quantity: rust_decimal::Decimal,
        date: &str,
    ) -> Result<rust_decimal::Decimal, DbError> {
        use rust_decimal::Decimal;
        // The variant's pricing inputs: own price_extra + the delegated template list_price / cost /
        // category. lst_price = list_price + price_extra (the effective sales price, the rule base).
        let row = sqlx::query(
            "SELECT pp.product_tmpl_id AS tmpl, pp.price_extra AS extra, \
                    pt.list_price AS list, pt.standard_price AS cost, pt.categ_id AS categ \
             FROM product_product pp JOIN product_template pt ON pt.id = pp.product_tmpl_id \
             WHERE pp.id = $1",
        )
        .bind(variant_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| DbError::BadInput(format!("variant {variant_id} not found")))?;
        let tmpl: i64 = row.try_get("tmpl")?;
        let extra: Decimal = row.try_get::<Option<Decimal>, _>("extra")?.unwrap_or_default();
        let list: Decimal = row.try_get::<Option<Decimal>, _>("list")?.unwrap_or_default();
        let cost: Decimal = row.try_get::<Option<Decimal>, _>("cost")?.unwrap_or_default();
        let categ: Option<i64> = row.try_get("categ")?;
        let lst_price = list + extra;

        // The product's category ancestry (category + parents), capped to terminate a deep/cyclic tree.
        let categ_chain: Vec<i64> = match categ {
            Some(c) => sqlx::query_scalar(
                "WITH RECURSIVE anc(id, parent_id, depth) AS (\
                     SELECT id, parent_id, 0 FROM product_category WHERE id = $1 \
                     UNION ALL \
                     SELECT c.id, c.parent_id, anc.depth + 1 FROM product_category c \
                       JOIN anc ON c.id = anc.parent_id WHERE anc.depth < 16) \
                 SELECT id FROM anc",
            )
            .bind(c)
            .fetch_all(&self.pool)
            .await?,
            None => Vec::new(),
        };

        // The most-specific applicable rule. applied_on sorts '0_product_variant' < '1_product' <
        // '2_product_category' < '3_global', so ORDER BY applied_on ASC takes the narrowest scope; then
        // the highest qualifying quantity tier.
        let item = sqlx::query(
            "SELECT compute_price, fixed_price, percent_price, base FROM product_pricelist_item \
             WHERE pricelist_id = $1 AND min_quantity <= $2 \
               AND (date_start IS NULL OR date_start <= $3::date) \
               AND (date_end IS NULL OR date_end >= $3::date) \
               AND (applied_on = '3_global' \
                    OR (applied_on = '2_product_category' AND categ_id = ANY($4)) \
                    OR (applied_on = '1_product' AND product_tmpl_id = $5) \
                    OR (applied_on = '0_product_variant' AND product_id = $6)) \
             ORDER BY applied_on ASC, min_quantity DESC LIMIT 1",
        )
        .bind(pricelist_id)
        .bind(quantity)
        .bind(date)
        .bind(&categ_chain)
        .bind(tmpl)
        .bind(variant_id)
        .fetch_optional(&self.pool)
        .await?;

        let item = match item {
            Some(i) => i,
            None => return Ok(lst_price), // no rule → the variant's own sales price
        };
        let compute: String = item.try_get("compute_price")?;
        let base: String = item.try_get("base")?;
        let base_price = if base == "standard_price" { cost } else { lst_price };
        let price = if compute == "fixed" {
            item.try_get::<Option<Decimal>, _>("fixed_price")?.unwrap_or_default()
        } else {
            let pct: Decimal = item.try_get::<Option<Decimal>, _>("percent_price")?.unwrap_or_default();
            let p = base_price * (Decimal::ONE - pct / Decimal::from(100));
            if p < Decimal::ZERO { Decimal::ZERO } else { p }
        };
        Ok(price)
    }

    /// Re-prices every line of a `sale.order` from its pricelist: resolves each line's unit price (by
    /// product, quantity, today) and writes `price_unit` through the SECURED update path under the
    /// CALLER's ctx — so the line's subtotal recomputes and cascades to the order total, and the
    /// caller's line ACL/rules apply per line. Gated on order WRITE; the pricelist currency must equal
    /// the order currency (no FX in v1 — enforced here, since @api.constrains can't read a related row).
    /// Returns the number of lines re-priced.
    pub async fn apply_pricelist(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        order_id: i64,
    ) -> Result<u64, DbError> {
        let order_model = resolve_registered("sale.order").map_err(DbError::BadInput)?;
        let line_model = resolve_registered("sale.order.line").map_err(DbError::BadInput)?;
        let pricelist_model = resolve_registered("product.pricelist").map_err(DbError::BadInput)?;

        if !check_access(Operation::Write, order_model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: order_model.name.to_string(), operation: "apply_pricelist" });
        }
        let order = self
            .find_one_secured(&order_model, ctx, acls, rules, order_id)
            .await?
            .ok_or_else(|| DbError::BadInput("order not found or not permitted".to_string()))?;
        let pricelist_id = order
            .get("pricelist_id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| DbError::BadInput("the order has no pricelist".to_string()))?;
        let order_currency = order.get("currency_id").and_then(|v| v.as_i64());

        let pl = self
            .find_one_secured(&pricelist_model, ctx, acls, rules, pricelist_id)
            .await?
            .ok_or_else(|| DbError::BadInput("pricelist not found or not permitted".to_string()))?;
        if pl.get("currency_id").and_then(|v| v.as_i64()) != order_currency {
            return Err(DbError::BadInput("pricelist currency does not match the order currency".to_string()));
        }

        let today = self.today().await?;
        let lines = self
            .find_secured(&line_model, ctx, acls, rules, Some(&Domain::field("order_id").eq(order_id)))
            .await?;
        let mut priced = 0u64;
        for line in &lines {
            let (Some(lid), Some(product_id)) =
                (line.get("id").and_then(|v| v.as_i64()), line.get("product_id").and_then(|v| v.as_i64()))
            else {
                continue;
            };
            let qty: rust_decimal::Decimal = line
                .get("product_uom_qty")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or_default();
            let price = self.resolve_price(pricelist_id, product_id, qty, &today).await?;
            let payload = serde_json::json!({ "price_unit": price.to_string() });
            self.update_secured(&line_model, ctx, acls, rules, lid, payload.as_object().unwrap()).await?;
            priced += 1;
        }
        Ok(priced)
    }

    /// Derives each `sale.order` line's effective `tax_rate` from its referenced `account.tax` and writes
    /// it through the SECURED update path, so the line/order tax computes cascade (price_tax, price_total,
    /// amount_tax, amount_total). The rate is materialized on the line because a stored same-record compute
    /// cannot read the related tax row at write time — this is the tax analogue of `apply_pricelist`.
    /// v1: percentage taxes only (a fixed or inactive tax yields rate 0). Gated on order WRITE; runs under
    /// the caller's ctx (lines must be visible/permitted). Returns the number of lines processed.
    pub async fn apply_taxes(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        order_id: i64,
    ) -> Result<u64, DbError> {
        self.apply_taxes_to(
            ctx, acls, rules, order_id, "sale.order", "sale.order.line", "sale.order.line.tax",
            "sale_order_line_tax_rel", "sale_order_line_tax", "apply_taxes",
        )
        .await
    }

    /// Buy-side mirror of `apply_taxes`: materializes the per-tax breakdown on a purchase order's lines
    /// (and remaps via the order's fiscal position). The vendor bill rolls it up per group.
    pub async fn apply_purchase_taxes(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        order_id: i64,
    ) -> Result<u64, DbError> {
        self.apply_taxes_to(
            ctx, acls, rules, order_id, "purchase.order", "purchase.order.line", "purchase.order.line.tax",
            "purchase_order_line_tax_rel", "purchase_order_line_tax", "apply_purchase_taxes",
        )
        .await
    }

    /// The tax-engine core shared by the sale and purchase sides. Resolves each line's tax set (Many2many,
    /// else legacy tax_id, else the product's default taxes), remaps it through the order's fiscal
    /// position, runs `compute_tax_lines`, and replaces the line's breakdown rows + a blended back-compat
    /// `tax_rate`. The model / table names differ only by side; the SQL fragments are fixed literals.
    #[allow(clippy::too_many_arguments)]
    async fn apply_taxes_to(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        order_id: i64,
        order_model_name: &str,
        line_model_name: &str,
        breakdown_model_name: &str,
        m2m_rel: &str,
        breakdown_table: &str,
        op_name: &'static str,
    ) -> Result<u64, DbError> {
        use rust_decimal::Decimal;
        let order_model = resolve_registered(order_model_name).map_err(DbError::BadInput)?;
        let line_model = resolve_registered(line_model_name).map_err(DbError::BadInput)?;
        let breakdown_model = resolve_registered(breakdown_model_name).map_err(DbError::BadInput)?;

        if !check_access(Operation::Write, order_model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: order_model.name.to_string(), operation: op_name });
        }
        let order = self
            .find_one_secured(&order_model, ctx, acls, rules, order_id)
            .await?
            .ok_or_else(|| DbError::BadInput("order not found or not permitted".to_string()))?;

        // Round per-tax amounts to the order currency's decimal places (default 2).
        let dp: u32 = match order.get("currency_id").and_then(|v| v.as_i64()) {
            Some(cur) => sqlx::query_scalar::<_, Option<i64>>("SELECT decimal_places FROM res_currency WHERE id = $1")
                .bind(cur)
                .fetch_optional(&self.pool)
                .await?
                .flatten()
                .unwrap_or(2) as u32,
            None => 2,
        };
        // Fiscal-position rewrite map (src tax -> Some(dest) / None = drop), if the order has one.
        let fmap = match order.get("fiscal_position_id").and_then(|v| v.as_i64()) {
            Some(pid) => self.fiscal_map_for(pid).await?,
            None => BTreeMap::new(),
        };

        let lines = self
            .find_secured(&line_model, ctx, acls, rules, Some(&Domain::field("order_id").eq(order_id)))
            .await?;
        let mut applied = 0u64;
        for line in &lines {
            let Some(lid) = line.get("id").and_then(|v| v.as_i64()) else { continue };
            let dec = |k: &str| -> Decimal {
                line.get(k).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or_default()
            };
            let qty = dec("product_uom_qty");
            let line_net = qty * dec("price_unit") * (Decimal::ONE - dec("discount") / Decimal::from(100));

            // The tax set, in resolution order: the line's Many2many membership, else the legacy single
            // tax_id, else the product's default taxes (Odoo's product.taxes_id flowing to the line).
            let mut tax_ids: Vec<i64> = sqlx::query_scalar(&format!("SELECT tax_id FROM {m2m_rel} WHERE line_id = $1"))
                .bind(lid)
                .fetch_all(&self.pool)
                .await?;
            if tax_ids.is_empty() {
                if let Some(t) = line.get("tax_id").and_then(|v| v.as_i64()) {
                    tax_ids.push(t);
                }
            }
            if tax_ids.is_empty() {
                if let Some(pid) = line.get("product_id").and_then(|v| v.as_i64()) {
                    tax_ids = sqlx::query_scalar(
                        "SELECT r.tax_id FROM product_template_tax_rel r \
                         JOIN product_product p ON p.product_tmpl_id = r.product_id WHERE p.id = $1",
                    )
                    .bind(pid)
                    .fetch_all(&self.pool)
                    .await?;
                }
            }
            // Remap through the fiscal position (NULL dest drops the tax); dedup, preserving order, so a
            // tax never applies twice. tax_ids (the user's original selection) is left untouched.
            let mut mapped: Vec<i64> = Vec::new();
            for t in &tax_ids {
                let dest = match fmap.get(t) {
                    Some(Some(d)) => Some(*d),
                    Some(None) => None,
                    None => Some(*t),
                };
                if let Some(d) = dest {
                    if !mapped.contains(&d) {
                        mapped.push(d);
                    }
                }
            }
            let specs = self.resolve_tax_specs(&mapped).await?;
            let (subtotal, results) = tax::compute_tax_lines(line_net, qty, &specs, dp);

            // Replace the line's breakdown rows (idempotent: a re-run re-derives from tax_ids).
            sqlx::query(&format!("DELETE FROM {breakdown_table} WHERE line_id = $1"))
                .bind(lid)
                .execute(&self.pool)
                .await?;
            for r in &results {
                let payload = serde_json::json!({
                    "line_id": lid, "sequence": r.sequence, "tax_id": r.tax_id, "tax_group_id": r.group_id,
                    "base_amount": r.base.to_string(), "tax_amount": r.tax_amount.to_string(),
                    "is_price_include": r.is_price_include
                });
                self.insert_secured(&breakdown_model, ctx, acls, rules, payload.as_object().unwrap()).await?;
            }
            // Back-compat blended rate (only consulted by the line's fallback compute when the breakdown is
            // empty, which it now is not). Updating the line also rolls the new tax up into the order totals.
            let total_tax: Decimal = results.iter().map(|r| r.tax_amount).sum();
            let blended = if subtotal != Decimal::ZERO {
                (total_tax / subtotal * Decimal::from(100)).round_dp(4)
            } else {
                Decimal::ZERO
            };
            let payload = serde_json::json!({ "tax_rate": blended.to_string() });
            self.update_secured(&line_model, ctx, acls, rules, lid, payload.as_object().unwrap()).await?;
            applied += 1;
        }
        Ok(applied)
    }

    /// Per-group tax totals (order currency) from a line's materialized breakdown, plus a single fallback
    /// bucket for any tax NOT in the breakdown (lines still on the tax_rate fallback), so the GL tax always
    /// sums to the order's amount_tax. Ordered by group sequence (NULL group last). `breakdown_table` /
    /// `line_table` are fixed literals (the sale or purchase pair). Returns (group name, amount) buckets.
    async fn tax_group_buckets(
        &self,
        order_id: i64,
        breakdown_table: &str,
        line_table: &str,
        total_tax: rust_decimal::Decimal,
    ) -> Result<Vec<(String, rust_decimal::Decimal)>, DbError> {
        use rust_decimal::Decimal;
        let sql = format!(
            "SELECT t.tax_group_id, g.name AS gname, SUM(t.tax_amount) AS amt \
             FROM {breakdown_table} t JOIN {line_table} l ON l.id = t.line_id \
             LEFT JOIN account_tax_group g ON g.id = t.tax_group_id \
             WHERE l.order_id = $1 \
             GROUP BY t.tax_group_id, g.name, g.sequence \
             ORDER BY COALESCE(g.sequence, 1000), t.tax_group_id"
        );
        let rows = sqlx::query(&sql).bind(order_id).fetch_all(&self.pool).await?;
        let mut buckets: Vec<(String, Decimal)> = Vec::new();
        let mut breakdown_total = Decimal::ZERO;
        for r in &rows {
            let amt: Decimal = r.try_get::<Option<Decimal>, _>("amt")?.unwrap_or_default();
            breakdown_total += amt;
            if amt != Decimal::ZERO {
                let name = r.try_get::<Option<String>, _>("gname").ok().flatten().unwrap_or_else(|| "Taxes".to_string());
                buckets.push((name, amt));
            }
        }
        let fallback = total_tax - breakdown_total;
        if fallback != Decimal::ZERO {
            buckets.push(("Taxes".to_string(), fallback));
        }
        Ok(buckets)
    }

    /// Resolves account.tax ids into engine specs, preserving the given order and dropping inactive taxes.
    async fn resolve_tax_specs(&self, tax_ids: &[i64]) -> Result<Vec<tax::TaxSpec>, DbError> {
        use rust_decimal::Decimal;
        let mut specs = Vec::new();
        for &tid in tax_ids {
            let row = sqlx::query(
                "SELECT amount_type, amount, price_include, include_base_amount, sequence, tax_group_id, active \
                 FROM account_tax WHERE id = $1",
            )
            .bind(tid)
            .fetch_optional(&self.pool)
            .await?;
            let Some(row) = row else { continue };
            if !row.try_get::<Option<bool>, _>("active").ok().flatten().unwrap_or(true) {
                continue;
            }
            specs.push(tax::TaxSpec {
                tax_id: tid,
                group_id: row.try_get::<Option<i64>, _>("tax_group_id").ok().flatten(),
                amount_type: row.try_get::<Option<String>, _>("amount_type").ok().flatten().unwrap_or_else(|| "percent".to_string()),
                amount: row.try_get::<Option<Decimal>, _>("amount").ok().flatten().unwrap_or_default(),
                price_include: row.try_get::<Option<bool>, _>("price_include").ok().flatten().unwrap_or(false),
                include_base_amount: row.try_get::<Option<bool>, _>("include_base_amount").ok().flatten().unwrap_or(false),
                sequence: row.try_get::<Option<i64>, _>("sequence").ok().flatten().unwrap_or(10),
            });
        }
        Ok(specs)
    }

    /// The fiscal position's source-to-destination tax rewrite map (NULL dest = drop the source tax).
    async fn fiscal_map_for(&self, position_id: i64) -> Result<BTreeMap<i64, Option<i64>>, DbError> {
        let rows = sqlx::query("SELECT tax_src_id, tax_dest_id FROM account_fiscal_position_tax WHERE position_id = $1")
            .bind(position_id)
            .fetch_all(&self.pool)
            .await?;
        let mut map = BTreeMap::new();
        for r in &rows {
            let src: i64 = r.try_get("tax_src_id")?;
            let dest: Option<i64> = r.try_get("tax_dest_id")?;
            map.insert(src, dest);
        }
        Ok(map)
    }

    /// The values an order/invoice line should default when its product is set: the product's name, its
    /// effective unit price (template list_price + variant price_extra), quantity 1, and unit of measure.
    /// A SECURED read of the product (delegated template fields included), so a caller who cannot read it
    /// gets a clean error rather than leaked data. Returns a JSON object the frontend merges into the line.
    pub async fn product_onchange_values(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        product_id: i64,
    ) -> Result<serde_json::Value, DbError> {
        use rust_decimal::Decimal;
        let product_model = resolve_registered("product.product").map_err(DbError::BadInput)?;
        let p = self
            .find_one_secured(&product_model, ctx, acls, rules, product_id)
            .await?
            .ok_or_else(|| DbError::BadInput("product not found or not permitted".to_string()))?;
        let dec = |k: &str| -> Decimal {
            p.get(k).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or_default()
        };
        let price = dec("list_price") + dec("price_extra");
        let mut values = serde_json::Map::new();
        if let Some(name) = p.get("name").filter(|v| !v.is_null()) {
            values.insert("name".to_string(), name.clone());
        }
        values.insert("price_unit".to_string(), serde_json::json!(price.to_string()));
        values.insert("product_uom_qty".to_string(), serde_json::json!("1"));
        if let Some(uom) = p.get("uom_id").filter(|v| !v.is_null()) {
            values.insert("uom_id".to_string(), uom.clone());
        }
        Ok(serde_json::Value::Object(values))
    }

    /// Applies the `sale.order.discount` wizard: writes its `discount` percent onto every line of its
    /// target order (the line/order compute cascade then re-rolls subtotals/totals). Gated on the REAL
    /// effect — WRITE on `sale.order` — since the wizard row itself is only read. Runs under the caller
    /// ctx (lines must be visible/permitted). Per-line, not atomic across lines (documented).
    pub async fn apply_sale_order_discount(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        wizard_id: i64,
    ) -> Result<u64, DbError> {
        let wizard_model = resolve_registered("sale.order.discount").map_err(DbError::BadInput)?;
        let order_model = resolve_registered("sale.order").map_err(DbError::BadInput)?;
        let line_model = resolve_registered("sale.order.line").map_err(DbError::BadInput)?;

        if !check_access(Operation::Write, order_model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: order_model.name.to_string(), operation: "apply_discount" });
        }
        let wizard = self
            .find_one_secured(&wizard_model, ctx, acls, rules, wizard_id)
            .await?
            .ok_or_else(|| DbError::BadInput("discount wizard not found or not permitted".to_string()))?;
        let order_id = wizard
            .get("order_id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| DbError::BadInput("the discount wizard has no order".to_string()))?;
        let discount: rust_decimal::Decimal = wizard
            .get("discount")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or_default();
        // Validate at the boundary: a percent must be in [0, 100] (the line net factor is 1 - d/100).
        if discount < rust_decimal::Decimal::ZERO || discount > rust_decimal::Decimal::from(100) {
            return Err(DbError::BadInput("discount must be a percentage between 0 and 100".to_string()));
        }

        // The order must be visible/permitted to the caller (mirrors apply_pricelist).
        self.find_one_secured(&order_model, ctx, acls, rules, order_id)
            .await?
            .ok_or_else(|| DbError::BadInput("order not found or not permitted".to_string()))?;

        let lines = self
            .find_secured(&line_model, ctx, acls, rules, Some(&Domain::field("order_id").eq(order_id)))
            .await?;
        let mut applied = 0u64;
        for line in &lines {
            let Some(lid) = line.get("id").and_then(|v| v.as_i64()) else { continue };
            let payload = serde_json::json!({ "discount": discount.to_string() });
            self.update_secured(&line_model, ctx, acls, rules, lid, payload.as_object().unwrap()).await?;
            applied += 1;
        }
        Ok(applied)
    }

    /// The exchange rate for `currency` effective on or before `as_of` (units of the currency per 1 base
    /// unit): the latest `res.currency.rate` row. A currency with NO rate rows is the base currency (1.0);
    /// a currency that HAS rates but none on or before `as_of` is an unknown historical rate → error
    /// (never silently 1.0 for a foreign currency).
    async fn currency_rate(&self, currency: i64, as_of: &str) -> Result<rust_decimal::Decimal, DbError> {
        use rust_decimal::Decimal;
        let latest: Option<Decimal> = sqlx::query_scalar(
            "SELECT rate FROM res_currency_rate WHERE currency_id = $1 AND name <= $2::date ORDER BY name DESC LIMIT 1",
        )
        .bind(currency)
        .bind(as_of)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(r) = latest {
            return Ok(r);
        }
        let has_any: Option<i64> = sqlx::query_scalar("SELECT 1 FROM res_currency_rate WHERE currency_id = $1 LIMIT 1")
            .bind(currency)
            .fetch_optional(&self.pool)
            .await?;
        if has_any.is_some() {
            Err(DbError::BadInput(format!("no exchange rate for currency {currency} on or before {as_of}")))
        } else {
            Ok(Decimal::ONE)
        }
    }

    /// Converts `amount` from `from_currency` to `to_currency` at the rates effective on `as_of`, rounded
    /// to the to-currency's decimal places. Two-hop through the base currency (Odoo's `_convert`).
    pub async fn convert_amount(
        &self,
        amount: rust_decimal::Decimal,
        from_currency: i64,
        to_currency: i64,
        as_of: &str,
    ) -> Result<rust_decimal::Decimal, DbError> {
        if from_currency == to_currency {
            return Ok(amount);
        }
        let from_rate = self.currency_rate(from_currency, as_of).await?;
        let to_rate = self.currency_rate(to_currency, as_of).await?;
        if from_rate.is_zero() {
            return Err(DbError::BadInput("source currency rate is zero".to_string()));
        }
        let dp: Option<i64> = sqlx::query_scalar::<_, Option<i64>>("SELECT decimal_places FROM res_currency WHERE id = $1")
            .bind(to_currency)
            .fetch_optional(&self.pool)
            .await?
            .flatten();
        Ok((amount * to_rate / from_rate).round_dp(dp.unwrap_or(2).max(0) as u32))
    }

    /// The company's fiscal lock date as an ISO `YYYY-MM-DD` string, or None if it has none. Read with a
    /// direct query (the lock is a posting guard, not user-scoped data).
    async fn company_lock_date(&self, company_id: i64) -> Result<Option<String>, DbError> {
        Ok(sqlx::query_scalar::<_, Option<String>>("SELECT fiscalyear_lock_date::text FROM res_company WHERE id = $1")
            .bind(company_id)
            .fetch_optional(&self.pool)
            .await?
            .flatten())
    }

    /// Posts an `account.move` (draft → posted): re-checks the balanced-entry invariant, numbers the
    /// entry from its journal's sequence (`sequence_code`, falling back to the journal `code`), and
    /// flips state to posted. Cross-record (reads the journal), so a service method — not a pure action.
    /// Runs under the caller ctx; returns the assigned entry number.
    pub async fn post_move(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        move_id: i64,
    ) -> Result<String, DbError> {
        let move_model = resolve_registered("account.move").map_err(DbError::BadInput)?;
        let journal_model = resolve_registered("account.journal").map_err(DbError::BadInput)?;
        let line_model = resolve_registered("account.move.line").map_err(DbError::BadInput)?;

        if !check_access(Operation::Write, move_model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: move_model.name.to_string(), operation: "post" });
        }
        let mv = self
            .find_one_secured(&move_model, ctx, acls, rules, move_id)
            .await?
            .ok_or_else(|| DbError::BadInput("move not found or not permitted".to_string()))?;
        let state = mv.get("state").and_then(|v| v.as_str()).unwrap_or("");
        if state != "draft" {
            return Err(DbError::BadInput(format!("only a draft entry can be posted (state is '{state}')")));
        }

        // Fiscal lock: an entry dated on or before its company's lock date cannot be posted. ISO date
        // strings compare lexically. A move with no date or no company, or a company with no lock, is free.
        if let (Some(md), Some(cid)) =
            (mv.get("date").and_then(|v| v.as_str()), mv.get("company_id").and_then(|v| v.as_i64()))
        {
            if let Some(lock) = self.company_lock_date(cid).await? {
                if md <= lock.as_str() {
                    return Err(DbError::BadInput(format!(
                        "cannot post an entry dated {md}: on or before the fiscal lock date {lock}"
                    )));
                }
            }
        }

        // Re-check the balance at post time (defense in depth — create already enforced it).
        let lines = self
            .find_secured(&line_model, ctx, acls, rules, Some(&Domain::field("move_id").eq(move_id)))
            .await?;
        if lines.is_empty() {
            return Err(DbError::BadInput("cannot post an entry with no lines".to_string()));
        }
        let (mut debit, mut credit) = (rust_decimal::Decimal::ZERO, rust_decimal::Decimal::ZERO);
        let parse = |l: &Json, f: &str| -> rust_decimal::Decimal {
            l.get(f).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or_default()
        };
        for l in &lines {
            debit += parse(l, "debit");
            credit += parse(l, "credit");
        }
        if debit != credit {
            return Err(DbError::BadInput(format!("cannot post an unbalanced entry: debit {debit} != credit {credit}")));
        }

        // Number the entry from its journal's sequence (sequence_code, else the journal code).
        let journal_id = mv
            .get("journal_id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| DbError::BadInput("the move has no journal".to_string()))?;
        let journal = self
            .find_one_secured(&journal_model, ctx, acls, rules, journal_id)
            .await?
            .ok_or_else(|| DbError::BadInput("journal not found or not permitted".to_string()))?;
        let sc = journal.get("sequence_code").and_then(|v| v.as_str()).unwrap_or("");
        let code = journal.get("code").and_then(|v| v.as_str()).unwrap_or("");
        let seq = if !sc.is_empty() {
            sc
        } else if !code.is_empty() {
            code
        } else {
            return Err(DbError::BadInput("the journal has no sequence code".to_string()));
        };
        self.ensure_sequence(seq, &format!("{seq}/"), "", 5).await?;
        let number = self.next_value(seq).await?;

        let payload = serde_json::json!({ "state": "posted", "name": number });
        self.update_secured(&move_model, ctx, acls, rules, move_id, payload.as_object().unwrap()).await?;
        Ok(number)
    }

    /// First ACTIVE id of `model` whose `field` equals `value` and whose company matches the invoicing
    /// company exactly — `company_id = c` when a company is given, else `company_id IS NULL` (shared).
    /// Used to resolve the receivable / income / tax account and the sale journal. Pinning the company
    /// (rather than "any company") keeps the lookup company-deterministic even though it runs elevated,
    /// so a shared order can only book to shared accounts — never another company's ledger.
    async fn first_match(
        &self,
        model: &ResolvedModel,
        ctx: &Ctx,
        field: &str,
        value: &str,
        company: Option<i64>,
    ) -> Result<Option<i64>, DbError> {
        let mut dom = Domain::field(field).eq(value).and(Domain::field("active").eq(true));
        dom = match company {
            Some(c) => dom.and(Domain::field("company_id").eq(c)),
            None => dom.and(Domain::field("company_id").is_null()),
        };
        Ok(self.find_ids_secured(model, ctx, &[], &[], Some(&dom)).await?.into_iter().next())
    }

    /// Generates a posted customer invoice (`account.move`, out_invoice) from a confirmed sale order:
    /// one income credit (untaxed), one tax credit (if any) and a receivable debit (total) — a balanced
    /// entry, posted (numbered + frozen) — then flips the order's `invoice_status` to invoiced. Gated on
    /// the caller's WRITE of the order; the GL posting runs elevated, so a salesperson need not also hold
    /// account groups. Returns the new move id. (v1: a single income line for the untaxed total, not one
    /// per order line; the vendor-bill mirror for purchase.order is a follow-up.)
    ///
    /// Ordering: the order is CLAIMED first (the `to_invoice → invoiced` flip under the caller, which
    /// enforces the order's WRITE ACL **and** WRITE record rule + company, requiring exactly one row),
    /// and only then is the move created and posted. So the elevated GL effect never runs unless the
    /// caller is actually authorized to write the order, and a denied claim leaves no orphan move.
    /// KNOWN LIMITATION (deferred): the claim and the move are not one transaction, and the flip is by
    /// id (not a compare-and-set on the status), so a DB failure between the claim and the posted move,
    /// or two simultaneous claims racing before either commits, can still desync (an order invoiced
    /// without a move, recoverable by reset). A fully transactional / FOR UPDATE invoicing path closes it.
    pub async fn create_sale_invoice(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        order_id: i64,
    ) -> Result<i64, DbError> {
        let order_model = resolve_registered("sale.order").map_err(DbError::BadInput)?;
        let account_model = resolve_registered("account.account")
            .map_err(|_| DbError::BadInput("install the account module to invoice".to_string()))?;
        let journal_model = resolve_registered("account.journal").map_err(DbError::BadInput)?;
        let move_model = resolve_registered("account.move").map_err(DbError::BadInput)?;

        if !check_access(Operation::Write, order_model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: order_model.name.to_string(), operation: "create_invoice" });
        }
        let order = self
            .find_one_secured(&order_model, ctx, acls, rules, order_id)
            .await?
            .ok_or_else(|| DbError::BadInput("order not found or not permitted".to_string()))?;
        let status = order.get("invoice_status").and_then(|v| v.as_str()).unwrap_or("");
        if status != "to_invoice" {
            return Err(DbError::BadInput(format!("order is not ready to invoice (invoice status '{status}')")));
        }
        let partner = order.get("partner_id").and_then(|v| v.as_i64());
        let currency = order.get("currency_id").and_then(|v| v.as_i64());
        // Pin the invoicing company: the order's, else the caller's active company. The chart lookup is
        // then company-deterministic (never another company's ledger), and it is stamped on the move.
        let company = order.get("company_id").and_then(|v| v.as_i64()).or(ctx.company_id);
        let amount = |k: &str| -> rust_decimal::Decimal {
            order.get(k).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or_default()
        };
        let (untaxed, tax, total) = (amount("amount_untaxed"), amount("amount_tax"), amount("amount_total"));
        // No degenerate invoices: a non-positive total is a credit-note / data-error case, not an invoice.
        if total <= rust_decimal::Decimal::ZERO {
            return Err(DbError::BadInput("cannot invoice an order with a non-positive total".to_string()));
        }

        // Resolve the chart BEFORE claiming the order, so a misconfiguration fails before any side effect.
        let elevated = ctx.sudo();
        let receivable = self
            .first_match(&account_model, &elevated, "account_type", "receivable", company)
            .await?
            .ok_or_else(|| DbError::BadInput("no receivable account configured".to_string()))?;
        let income = self
            .first_match(&account_model, &elevated, "account_type", "income", company)
            .await?
            .ok_or_else(|| DbError::BadInput("no income account configured".to_string()))?;
        let journal = self
            .first_match(&journal_model, &elevated, "journal_type", "sale", company)
            .await?
            .ok_or_else(|| DbError::BadInput("no sale journal configured".to_string()))?;
        // The tax credit is written whenever there is any tax (incl. a negative one) so the move always
        // balances; `> 0` would silently drop a negative tax and unbalance the entry.
        let tax_account = if tax != rust_decimal::Decimal::ZERO {
            Some(
                self.first_match(&account_model, &elevated, "account_type", "tax", company)
                    .await?
                    .ok_or_else(|| DbError::BadInput("no tax account configured".to_string()))?,
            )
        } else {
            None
        };

        // CLAIM the order under the caller — this enforces the WRITE record rule + company on the order,
        // and exactly-one-row means we are authorized; abort (no GL effect) otherwise.
        let claim = serde_json::json!({ "invoice_status": "invoiced" });
        if self.update_secured(&order_model, ctx, acls, rules, order_id, claim.as_object().unwrap()).await? != 1 {
            return Err(DbError::AccessDenied { model: order_model.name.to_string(), operation: "create_invoice" });
        }

        // Accounting date is today; the due date is today + the order's payment term (days), if any.
        let today = self.today().await?;
        let due_date = match order.get("payment_term_id").and_then(|v| v.as_i64()) {
            Some(term_id) => sqlx::query_scalar::<_, Option<String>>(
                "SELECT ($1::date + days::int)::text FROM account_payment_term WHERE id = $2 AND active",
            )
            .bind(&today)
            .bind(term_id)
            .fetch_optional(&self.pool)
            .await?
            .flatten()
            .unwrap_or_else(|| today.clone()),
            None => today.clone(),
        };
        // Resolve the company currency once. FX applies only when it differs from the invoice currency;
        // same / absent currency is identity, so the single-currency path is byte-for-byte unchanged.
        let co_cur: Option<i64> = match company {
            Some(co) => sqlx::query_scalar::<_, Option<i64>>("SELECT currency_id FROM res_company WHERE id = $1")
                .bind(co)
                .fetch_optional(&self.pool)
                .await?
                .flatten(),
            None => None,
        };
        let fx = match (currency, co_cur) {
            (Some(c), Some(cc)) if c != cc => Some((c, cc)),
            _ => None,
        };
        use rust_decimal::Decimal;
        // Company-currency value at today's rate (identity when the order currency == the company's).
        let untaxed_co = match fx {
            Some((c, cc)) => self.convert_amount(untaxed, c, cc, &today).await?,
            None => untaxed,
        };

        // Per-group tax totals (order currency) from the materialized breakdown + a fallback bucket for any
        // tax not covered by a breakdown row, so the GL tax always sums to amount_tax and the move balances.
        let buckets = self.tax_group_buckets(order_id, "sale_order_line_tax", "sale_order_line", tax).await?;
        // A net-zero-across-groups tax set still needs the tax account; resolve it on demand.
        let tax_account = match tax_account {
            Some(a) => Some(a),
            None if !buckets.is_empty() => Some(
                self.first_match(&account_model, &elevated, "account_type", "tax", company)
                    .await?
                    .ok_or_else(|| DbError::BadInput("no tax account configured".to_string()))?,
            ),
            None => None,
        };

        // Balanced invoice: income credit (untaxed) + one tax credit per group + receivable debit, all in
        // the company currency. `amount_currency` carries the signed invoice-currency amount (+ debit, −
        // credit) as the FX memo. The receivable is the SUM of the already-rounded company-currency parts
        // (untaxed_co + Σ tax_g_co), NEVER an independent convert(total), so check_balanced cannot trip.
        let mut lines = vec![serde_json::json!({
            "account_id": income, "name": "Untaxed Amount", "debit": "0", "credit": untaxed_co.to_string(),
            "amount_currency": (-untaxed).to_string(), "partner_id": partner, "company_id": company
        })];
        let mut tax_co_total = Decimal::ZERO;
        for (name, amt) in &buckets {
            let amt_co = match fx {
                Some((c, cc)) => self.convert_amount(*amt, c, cc, &today).await?,
                None => *amt,
            };
            tax_co_total += amt_co;
            lines.push(serde_json::json!({
                "account_id": tax_account, "name": name, "debit": "0", "credit": amt_co.to_string(),
                "amount_currency": (-*amt).to_string(), "partner_id": partner, "company_id": company
            }));
        }
        let receivable_co = untaxed_co + tax_co_total;
        lines.push(serde_json::json!({
            "account_id": receivable, "name": "Receivable", "debit": receivable_co.to_string(), "credit": "0",
            "amount_currency": total.to_string(), "partner_id": partner, "company_id": company
        }));

        let move_payload = serde_json::json!({
            "move_type": "out_invoice", "journal_id": journal, "partner_id": partner,
            "currency_id": currency, "company_id": company, "line_ids": lines,
            "date": today, "invoice_date_due": due_date,
            // Residual stays in INVOICE currency (the foreign amount owed); the company-currency total is
            // the receivable's company debit. register_payment draws the residual down.
            "amount_residual": total.to_string(), "amount_total_company": receivable_co.to_string()
        });
        let move_id =
            self.insert_secured(&move_model, &elevated, &[], &[], move_payload.as_object().unwrap()).await?;
        self.post_move(&elevated, &[], &[], move_id).await?;
        Ok(move_id)
    }

    /// Generates a posted vendor bill (`account.move`, in_invoice) from a confirmed purchase order: one
    /// expense debit (untaxed), one tax debit (if any) and a payable credit (total) — the buy-side mirror
    /// of `create_sale_invoice`, then flips the order's `invoice_status` to invoiced. Multi-currency safe
    /// (the payable is the SUM of the already-rounded company-currency parts, so the move balances to the
    /// cent). Gated on the caller's WRITE of the order; the GL posting runs elevated. Returns the move id.
    /// Same documented non-atomicity as create_sale_invoice (claim then post). v1: a single tax line from
    /// the order's amount_tax (purchase lines carry no per-group breakdown); per-group tax + fiscal
    /// positions on the buy side are a follow-up. The bill is payable via `register_payment` (in_invoice).
    pub async fn create_vendor_bill(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        order_id: i64,
    ) -> Result<i64, DbError> {
        use rust_decimal::Decimal;
        let order_model = resolve_registered("purchase.order").map_err(DbError::BadInput)?;
        let account_model = resolve_registered("account.account")
            .map_err(|_| DbError::BadInput("install the account module to bill".to_string()))?;
        let journal_model = resolve_registered("account.journal").map_err(DbError::BadInput)?;
        let move_model = resolve_registered("account.move").map_err(DbError::BadInput)?;

        if !check_access(Operation::Write, order_model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: order_model.name.to_string(), operation: "create_vendor_bill" });
        }
        let order = self
            .find_one_secured(&order_model, ctx, acls, rules, order_id)
            .await?
            .ok_or_else(|| DbError::BadInput("order not found or not permitted".to_string()))?;
        let status = order.get("invoice_status").and_then(|v| v.as_str()).unwrap_or("");
        if status != "to_invoice" {
            return Err(DbError::BadInput(format!("order is not ready to bill (billing status '{status}')")));
        }
        let partner = order.get("partner_id").and_then(|v| v.as_i64());
        let currency = order.get("currency_id").and_then(|v| v.as_i64());
        let company = order.get("company_id").and_then(|v| v.as_i64()).or(ctx.company_id);
        let amount = |k: &str| -> Decimal {
            order.get(k).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or_default()
        };
        let (untaxed, tax, total) = (amount("amount_untaxed"), amount("amount_tax"), amount("amount_total"));
        if total <= Decimal::ZERO {
            return Err(DbError::BadInput("cannot bill an order with a non-positive total".to_string()));
        }

        // Resolve the chart BEFORE claiming the order, so a misconfiguration fails before any side effect.
        let elevated = ctx.sudo();
        let payable = self
            .first_match(&account_model, &elevated, "account_type", "payable", company)
            .await?
            .ok_or_else(|| DbError::BadInput("no payable account configured".to_string()))?;
        let expense = self
            .first_match(&account_model, &elevated, "account_type", "expense", company)
            .await?
            .ok_or_else(|| DbError::BadInput("no expense account configured".to_string()))?;
        let journal = self
            .first_match(&journal_model, &elevated, "journal_type", "purchase", company)
            .await?
            .ok_or_else(|| DbError::BadInput("no purchase journal configured".to_string()))?;
        let tax_account = if tax != Decimal::ZERO {
            Some(
                self.first_match(&account_model, &elevated, "account_type", "tax", company)
                    .await?
                    .ok_or_else(|| DbError::BadInput("no tax account configured".to_string()))?,
            )
        } else {
            None
        };

        // CLAIM the order under the caller (enforces the order WRITE ACL + record rule + company); abort
        // (no GL effect) otherwise.
        let claim = serde_json::json!({ "invoice_status": "invoiced" });
        if self.update_secured(&order_model, ctx, acls, rules, order_id, claim.as_object().unwrap()).await? != 1 {
            return Err(DbError::AccessDenied { model: order_model.name.to_string(), operation: "create_vendor_bill" });
        }

        let today = self.today().await?;
        // Company-currency conversion at today's rate (identity when same / absent company currency).
        let co_cur: Option<i64> = match company {
            Some(co) => sqlx::query_scalar::<_, Option<i64>>("SELECT currency_id FROM res_company WHERE id = $1")
                .bind(co)
                .fetch_optional(&self.pool)
                .await?
                .flatten(),
            None => None,
        };
        let fx = match (currency, co_cur) {
            (Some(c), Some(cc)) if c != cc => Some((c, cc)),
            _ => None,
        };
        let untaxed_co = match fx {
            Some((c, cc)) => self.convert_amount(untaxed, c, cc, &today).await?,
            None => untaxed,
        };
        // Per-group tax totals (order currency) from the breakdown + a fallback bucket; the GL tax sums to
        // amount_tax and the bill balances.
        let buckets = self.tax_group_buckets(order_id, "purchase_order_line_tax", "purchase_order_line", tax).await?;
        // A net-zero-across-groups tax set still needs the tax account; resolve it on demand.
        let tax_account = match tax_account {
            Some(a) => Some(a),
            None if !buckets.is_empty() => Some(
                self.first_match(&account_model, &elevated, "account_type", "tax", company)
                    .await?
                    .ok_or_else(|| DbError::BadInput("no tax account configured".to_string()))?,
            ),
            None => None,
        };

        // Balanced bill: expense debit (untaxed) + one tax debit per group + payable credit (total), in
        // company currency. amount_currency carries the signed invoice-currency amount (+ debit, − credit).
        // The payable is the SUM of the rounded parts (untaxed_co + Σ tax_g_co), never convert(total).
        let mut lines = vec![serde_json::json!({
            "account_id": expense, "name": "Untaxed Amount", "debit": untaxed_co.to_string(), "credit": "0",
            "amount_currency": untaxed.to_string(), "partner_id": partner, "company_id": company
        })];
        let mut tax_co_total = Decimal::ZERO;
        for (name, amt) in &buckets {
            let amt_co = match fx {
                Some((c, cc)) => self.convert_amount(*amt, c, cc, &today).await?,
                None => *amt,
            };
            tax_co_total += amt_co;
            lines.push(serde_json::json!({
                "account_id": tax_account, "name": name, "debit": amt_co.to_string(), "credit": "0",
                "amount_currency": amt.to_string(), "partner_id": partner, "company_id": company
            }));
        }
        let payable_co = untaxed_co + tax_co_total;
        lines.push(serde_json::json!({
            "account_id": payable, "name": "Payable", "debit": "0", "credit": payable_co.to_string(),
            "amount_currency": (-total).to_string(), "partner_id": partner, "company_id": company
        }));

        let move_payload = serde_json::json!({
            "move_type": "in_invoice", "journal_id": journal, "partner_id": partner,
            "currency_id": currency, "company_id": company, "line_ids": lines,
            "date": today, "invoice_date_due": today,
            // Settlement starts fully open; register_payment draws this down (in_invoice → payable).
            "amount_residual": total.to_string(), "amount_total_company": payable_co.to_string()
        });
        let move_id =
            self.insert_secured(&move_model, &elevated, &[], &[], move_payload.as_object().unwrap()).await?;
        self.post_move(&elevated, &[], &[], move_id).await?;
        Ok(move_id)
    }

    /// Registers a (full or partial) payment against a posted customer invoice / vendor bill: atomically
    /// draws down the invoice's open `amount_residual` (a guarded SQL decrement that both validates — no
    /// over-payment, no concurrent double-spend — and records `payment_state`/`reconciled`), then books a
    /// balanced 2-line payment entry through the given bank/cash journal, posted elevated. Gated on the
    /// caller's WRITE of account.move. Returns the posted payment move id.
    /// KNOWN LIMITATION (deferred, mirrors create_sale_invoice): the residual draw-down and the payment
    /// move are not one transaction, so a failure between them can desync (residual drawn, no move).
    pub async fn register_payment(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        invoice_id: i64,
        amount: rust_decimal::Decimal,
        journal_id: i64,
    ) -> Result<i64, DbError> {
        use rust_decimal::Decimal;
        let move_model = resolve_registered("account.move").map_err(DbError::BadInput)?;
        let journal_model = resolve_registered("account.journal").map_err(DbError::BadInput)?;
        let account_model = resolve_registered("account.account").map_err(DbError::BadInput)?;

        if !check_access(Operation::Write, move_model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: move_model.name.to_string(), operation: "register_payment" });
        }
        if amount <= Decimal::ZERO {
            return Err(DbError::BadInput("payment amount must be positive".to_string()));
        }
        let inv = self
            .find_one_secured(&move_model, ctx, acls, rules, invoice_id)
            .await?
            .ok_or_else(|| DbError::BadInput("invoice not found or not permitted".to_string()))?;
        if inv.get("state").and_then(|v| v.as_str()) != Some("posted") {
            return Err(DbError::BadInput("only a posted invoice can be paid".to_string()));
        }
        let is_customer = match inv.get("move_type").and_then(|v| v.as_str()).unwrap_or("") {
            "out_invoice" => true,
            "in_invoice" => false,
            _ => return Err(DbError::BadInput("payments apply to customer invoices or vendor bills".to_string())),
        };
        let partner = inv.get("partner_id").and_then(|v| v.as_i64());
        let currency = inv.get("currency_id").and_then(|v| v.as_i64());
        let company = inv.get("company_id").and_then(|v| v.as_i64());

        // Money account = the bank/cash journal's default account; counterpart = the receivable
        // (customer) / payable (vendor) the invoice settles, resolved company-deterministically.
        let elevated = ctx.sudo();
        let journal = self
            .find_one_secured(&journal_model, &elevated, &[], &[], journal_id)
            .await?
            .ok_or_else(|| DbError::BadInput("journal not found".to_string()))?;
        match journal.get("journal_type").and_then(|v| v.as_str()).unwrap_or("") {
            "bank" | "cash" => {}
            _ => return Err(DbError::BadInput("a payment needs a bank or cash journal".to_string())),
        }
        let money = journal
            .get("default_account_id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| DbError::BadInput("the payment journal has no default account".to_string()))?;
        let counter_type = if is_customer { "receivable" } else { "payable" };
        let counterpart = self
            .first_match(&account_model, &elevated, "account_type", counter_type, company)
            .await?
            .ok_or_else(|| DbError::BadInput(format!("no {counter_type} account configured")))?;

        // Atomically draw down the open residual — validates AND records the new settlement state in one
        // guarded statement (no posted-line write; `amount_residual` is the move's own field). The CASE
        // reads the OLD residual (Postgres evaluates SET RHS against the pre-update row).
        let row = sqlx::query(
            "UPDATE account_move \
             SET amount_residual = amount_residual - $2, \
                 payment_state = CASE WHEN amount_residual - $2 <= 0 THEN 'paid' ELSE 'partial' END, \
                 reconciled = (amount_residual - $2 <= 0) \
             WHERE id = $1 AND amount_residual >= $2 \
             RETURNING id",
        )
        .bind(invoice_id)
        .bind(amount)
        .fetch_optional(&self.pool)
        .await?;
        if row.is_none() {
            return Err(DbError::BadInput("payment exceeds the invoice's open balance".to_string()));
        }

        // Book the balanced payment entry: customer pays in → debit bank / credit receivable; vendor bill
        // paid → credit bank / debit payable. Multi-currency: the bank movement is valued at TODAY's rate
        // while the counterpart is relieved at the invoice-date rate (what sits on the books); the
        // difference is the realized FX gain/loss, booked to a 3rd line so the company-currency entry
        // balances. Same / absent company currency ⇒ both equal `amount`, no FX line — the exact
        // single-currency 2-liner (plus the additive amount_currency memo).
        let today = self.today().await?;
        let invoice_date = inv.get("date").and_then(|v| v.as_str()).unwrap_or(today.as_str()).to_string();
        let co_cur: Option<i64> = match company {
            Some(co) => sqlx::query_scalar::<_, Option<i64>>("SELECT currency_id FROM res_company WHERE id = $1")
                .bind(co)
                .fetch_optional(&self.pool)
                .await?
                .flatten(),
            None => None,
        };
        let fx = match (currency, co_cur) {
            (Some(c), Some(cc)) if c != cc => Some((c, cc)),
            _ => None,
        };
        let (money_company, counter_company) = match fx {
            Some((c, cc)) => (
                self.convert_amount(amount, c, cc, &today).await?,
                self.convert_amount(amount, c, cc, &invoice_date).await?,
            ),
            None => (amount, amount),
        };

        // amount_currency: + on the debit side, − on the credit side (the FX-memo sign convention).
        let (bank_d, bank_c, bank_cur, ctr_d, ctr_c, ctr_cur) = if is_customer {
            (money_company, Decimal::ZERO, amount, Decimal::ZERO, counter_company, -amount)
        } else {
            (Decimal::ZERO, money_company, -amount, counter_company, Decimal::ZERO, amount)
        };
        let mut lines = vec![
            serde_json::json!({ "account_id": money, "name": "Payment", "debit": bank_d.to_string(), "credit": bank_c.to_string(), "amount_currency": bank_cur.to_string(), "partner_id": partner, "company_id": company }),
            serde_json::json!({ "account_id": counterpart, "name": "Payment", "debit": ctr_d.to_string(), "credit": ctr_c.to_string(), "amount_currency": ctr_cur.to_string(), "partner_id": partner, "company_id": company }),
        ];
        // FX plug: the line that keeps Σdebit == Σcredit in company currency (the realized gain/loss).
        // v1 books both gain and loss to the income account; dedicated gain/loss accounts are a follow-up.
        let imbalance = (bank_d + ctr_d) - (bank_c + ctr_c);
        if imbalance != Decimal::ZERO {
            let fx_account = self
                .first_match(&account_model, &elevated, "account_type", "income", company)
                .await?
                .ok_or_else(|| DbError::BadInput("no income account configured for FX gain/loss".to_string()))?;
            let (fx_d, fx_c) = if imbalance > Decimal::ZERO {
                (Decimal::ZERO, imbalance)
            } else {
                (-imbalance, Decimal::ZERO)
            };
            lines.push(serde_json::json!({ "account_id": fx_account, "name": "Exchange difference", "debit": fx_d.to_string(), "credit": fx_c.to_string(), "amount_currency": "0", "partner_id": partner, "company_id": company }));
        }
        let lines = serde_json::Value::Array(lines);
        let pay_payload = serde_json::json!({
            "move_type": "entry", "journal_id": journal_id, "partner_id": partner,
            "currency_id": currency, "company_id": company, "line_ids": lines
        });
        let pay_id =
            self.insert_secured(&move_model, &elevated, &[], &[], pay_payload.as_object().unwrap()).await?;
        self.post_move(&elevated, &[], &[], pay_id).await?;
        Ok(pay_id)
    }

    /// Trial balance: per-account totals (debit, credit, balance) over POSTED journal entries — the
    /// foundation report the frontend folds into a P&L and a balance sheet. Only accounts with posted
    /// activity appear, ordered by code. Gated on account.account READ. v1: all companies (multi-company
    /// scoping is a follow-up). Returns one JSON object per account.
    pub async fn trial_balance(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        _rules: &[RecordRule],
    ) -> Result<Vec<serde_json::Value>, DbError> {
        let account_model = resolve_registered("account.account").map_err(DbError::BadInput)?;
        if !check_access(Operation::Read, account_model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: account_model.name.to_string(), operation: "trial_balance" });
        }
        let rows = sqlx::query(
            "SELECT a.id, a.code, a.name, a.account_type, \
                    COALESCE(SUM(l.debit), 0) AS debit, COALESCE(SUM(l.credit), 0) AS credit \
             FROM account_account a \
             JOIN account_move_line l ON l.account_id = a.id \
             JOIN account_move m ON m.id = l.move_id AND m.state = 'posted' \
             GROUP BY a.id, a.code, a.name, a.account_type \
             ORDER BY a.code",
        )
        .fetch_all(&self.pool)
        .await?;
        use rust_decimal::Decimal;
        Ok(rows
            .iter()
            .map(|r| {
                let debit: Decimal = r.try_get("debit").unwrap_or_default();
                let credit: Decimal = r.try_get("credit").unwrap_or_default();
                serde_json::json!({
                    "account_id": r.try_get::<i64, _>("id").unwrap_or_default(),
                    "code": r.try_get::<Option<String>, _>("code").ok().flatten(),
                    "name": r.try_get::<Option<String>, _>("name").ok().flatten(),
                    "account_type": r.try_get::<Option<String>, _>("account_type").ok().flatten(),
                    "debit": debit.to_string(),
                    "credit": credit.to_string(),
                    "balance": (debit - credit).to_string(),
                })
            })
            .collect())
    }

    /// Aged receivable / payable: open posted invoices (`amount_residual > 0`) of `move_type`
    /// ("out_invoice" = receivable, "in_invoice" = payable), grouped by partner and bucketed by days
    /// past their due date (current / 1-30 / 31-60 / 61-90 / 90+). A null due date ages as current.
    /// Read-gated on account.move. v1: ages against today, single currency.
    pub async fn aged_balance(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        _rules: &[RecordRule],
        move_type: &str,
    ) -> Result<Vec<serde_json::Value>, DbError> {
        let move_model = resolve_registered("account.move").map_err(DbError::BadInput)?;
        if !check_access(Operation::Read, move_model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: move_model.name.to_string(), operation: "aged_balance" });
        }
        let rows = sqlx::query(
            "SELECT m.partner_id, p.name AS partner_name, \
                    COALESCE(SUM(m.amount_residual) FILTER (WHERE m.invoice_date_due IS NULL OR m.invoice_date_due >= current_date), 0) AS bucket_current, \
                    COALESCE(SUM(m.amount_residual) FILTER (WHERE current_date - m.invoice_date_due BETWEEN 1 AND 30), 0) AS b1_30, \
                    COALESCE(SUM(m.amount_residual) FILTER (WHERE current_date - m.invoice_date_due BETWEEN 31 AND 60), 0) AS b31_60, \
                    COALESCE(SUM(m.amount_residual) FILTER (WHERE current_date - m.invoice_date_due BETWEEN 61 AND 90), 0) AS b61_90, \
                    COALESCE(SUM(m.amount_residual) FILTER (WHERE current_date - m.invoice_date_due > 90), 0) AS b90_plus, \
                    COALESCE(SUM(m.amount_residual), 0) AS total \
             FROM account_move m \
             LEFT JOIN res_partner p ON p.id = m.partner_id \
             WHERE m.state = 'posted' AND m.move_type = $1 AND m.amount_residual > 0 \
             GROUP BY m.partner_id, p.name \
             ORDER BY p.name",
        )
        .bind(move_type)
        .fetch_all(&self.pool)
        .await?;
        use rust_decimal::Decimal;
        let d = |r: &sqlx::postgres::PgRow, c: &str| -> String {
            r.try_get::<Decimal, _>(c).unwrap_or_default().to_string()
        };
        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "partner_id": r.try_get::<Option<i64>, _>("partner_id").ok().flatten(),
                    "partner_name": r.try_get::<Option<String>, _>("partner_name").ok().flatten(),
                    "current": d(r, "bucket_current"),
                    "b1_30": d(r, "b1_30"),
                    "b31_60": d(r, "b31_60"),
                    "b61_90": d(r, "b61_90"),
                    "b90_plus": d(r, "b90_plus"),
                    "total": d(r, "total"),
                })
            })
            .collect())
    }

    /// Validates a `stock.picking` (draft → done): in ONE transaction, atomically moves each line's
    /// quantity from its source to its destination quant (`ON CONFLICT (product_id, location_id)`
    /// upsert), marks the moves done, numbers the transfer from a per-type sequence (IN/OUT/INT), and
    /// re-materializes `product.product.qty_available` (Σ internal quants) for the moved products. A
    /// `FOR UPDATE` lock on the picking makes it a true compare-and-set, so two concurrent validations
    /// cannot double-apply. v1 allows negative stock (no reservation). Gated on the caller's WRITE of
    /// the picking; the quant mutations are a system effect run inside the transaction. Returns the
    /// assigned transfer number.
    /// Reserves stock for a draft transfer's internal-source moves: for each move still short of its
    /// demand, claims up to the available (unreserved) on-hand at its source, recording the claim on both
    /// the source quant (`reserved_quantity`) and the move (`reserved_qty`). The quant row is locked
    /// `FOR UPDATE` — the serialization point that stops two concurrent reservations of the same quant
    /// from over-committing. Idempotent: a re-run only tops up the still-unreserved delta of each move.
    /// Returns the number of moves that gained some reservation. Gated on the caller's picking WRITE.
    /// The conversion factor of a unit of measure to its product's REFERENCE unit, defined as reference
    /// units per one of this unit (dozen -> 12, cm with m reference -> 0.01, the reference itself -> 1).
    /// Absent / unreadable / non-positive factor => 1 (pass-through, so a move without a uom or a legacy
    /// row is unconverted). The f64 column is snapshotted to a Decimal rounded to 6 dp to bound float dust
    /// so the quant math (qty_ref = move_qty * factor) stays clean.
    async fn uom_factor(&self, uom_id: Option<i64>) -> Result<rust_decimal::Decimal, DbError> {
        use rust_decimal::Decimal;
        let Some(uid) = uom_id else { return Ok(Decimal::ONE) };
        let f: Option<f64> = sqlx::query_scalar("SELECT factor FROM uom_uom WHERE id = $1")
            .bind(uid)
            .fetch_optional(&self.pool)
            .await?
            .flatten();
        match f.and_then(Decimal::from_f64_retain) {
            Some(d) if d > Decimal::ZERO => Ok(d.round_dp(6)),
            _ => Ok(Decimal::ONE),
        }
    }

    pub async fn reserve_picking(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        picking_id: i64,
    ) -> Result<i64, DbError> {
        use rust_decimal::Decimal;
        let picking_model = resolve_registered("stock.picking").map_err(DbError::BadInput)?;
        if !check_access(Operation::Write, picking_model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: picking_model.name.to_string(), operation: "reserve" });
        }
        let picking = self
            .find_one_secured(&picking_model, ctx, acls, rules, picking_id)
            .await?
            .ok_or_else(|| DbError::BadInput("transfer not found or not permitted".to_string()))?;
        let state = picking.get("state").and_then(|v| v.as_str()).unwrap_or("");
        if state != "draft" {
            return Err(DbError::BadInput(format!("only a draft transfer can be reserved (state is '{state}')")));
        }

        let mut tx = self.pool.begin().await?;
        let moves = sqlx::query("SELECT id, product_id, product_uom_qty, product_uom_id, lot_id, reserved_qty, location_id FROM stock_move WHERE picking_id = $1 AND state = 'draft'")
            .bind(picking_id)
            .fetch_all(&mut *tx)
            .await?;
        let mut reserved_moves = 0i64;
        for m in &moves {
            let move_id: i64 = m.try_get("id")?;
            let product_id: i64 = m.try_get("product_id")?;
            let ordered: Decimal = m.try_get("product_uom_qty")?;
            let already: Decimal = m.try_get("reserved_qty")?;
            let lot_id: Option<i64> = m.try_get("lot_id")?;
            let src: i64 = m.try_get("location_id")?;
            // Convert the demand to the product reference unit — the quant + reserved_qty are reference.
            let factor = self.uom_factor(m.try_get::<Option<i64>, _>("product_uom_id")?).await?;
            let ordered_ref = (ordered * factor).round_dp(6);
            // Only internal sources hold reservable stock (a supplier/customer source has none).
            let src_usage: Option<String> = sqlx::query_scalar("SELECT usage FROM stock_location WHERE id = $1")
                .bind(src)
                .fetch_optional(&mut *tx)
                .await?;
            if src_usage.as_deref() != Some("internal") {
                continue;
            }
            let want = ordered_ref - already;
            if want <= Decimal::ZERO {
                continue;
            }
            // Lock the quant row for this (product, location, lot): a concurrent reserve of the same quant
            // blocks here. COALESCE(lot_id, 0) matches the untracked-bulk quant when the move has no lot.
            let row = sqlx::query("SELECT quantity, COALESCE(reserved_quantity, 0) AS reserved_quantity FROM stock_quant WHERE product_id = $1 AND location_id = $2 AND COALESCE(lot_id, 0) = COALESCE($3, 0) FOR UPDATE")
                .bind(product_id)
                .bind(src)
                .bind(lot_id)
                .fetch_optional(&mut *tx)
                .await?;
            let Some(row) = row else { continue }; // no quant row → nothing on hand → reserve nothing
            let on_hand: Decimal = row.try_get("quantity")?;
            let reserved: Decimal = row.try_get("reserved_quantity")?;
            let free = on_hand - reserved;
            let grant = if want < free { want } else { free };
            if grant <= Decimal::ZERO {
                continue;
            }
            sqlx::query("UPDATE stock_quant SET reserved_quantity = reserved_quantity + $4 WHERE product_id = $1 AND location_id = $2 AND COALESCE(lot_id, 0) = COALESCE($3, 0)")
                .bind(product_id)
                .bind(src)
                .bind(lot_id)
                .bind(grant)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE stock_move SET reserved_qty = reserved_qty + $2 WHERE id = $1")
                .bind(move_id)
                .bind(grant)
                .execute(&mut *tx)
                .await?;
            reserved_moves += 1;
        }
        tx.commit().await?;
        Ok(reserved_moves)
    }

    pub async fn validate_picking(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        picking_id: i64,
    ) -> Result<String, DbError> {
        let picking_model = resolve_registered("stock.picking").map_err(DbError::BadInput)?;

        if !check_access(Operation::Write, picking_model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: picking_model.name.to_string(), operation: "validate" });
        }
        let picking = self
            .find_one_secured(&picking_model, ctx, acls, rules, picking_id)
            .await?
            .ok_or_else(|| DbError::BadInput("transfer not found or not permitted".to_string()))?;
        let state = picking.get("state").and_then(|v| v.as_str()).unwrap_or("");
        if state != "draft" {
            return Err(DbError::BadInput(format!("only a draft transfer can be validated (state is '{state}')")));
        }
        let seq = match picking.get("picking_type").and_then(|v| v.as_str()).unwrap_or("internal") {
            "receipt" => "IN",
            "delivery" => "OUT",
            _ => "INT",
        };
        self.ensure_sequence(seq, &format!("{seq}/"), "", 5).await?;
        let number = self.next_value(seq).await?;

        let mut tx = self.pool.begin().await?;
        // Compare-and-set: lock the row and re-assert draft, so concurrent validations can't double-apply.
        let live: Option<String> =
            sqlx::query_scalar("SELECT state FROM stock_picking WHERE id = $1 FOR UPDATE").bind(picking_id).fetch_optional(&mut *tx).await?;
        if live.as_deref() != Some("draft") {
            return Err(DbError::Conflict("the transfer was already validated".to_string()));
        }

        let moves = sqlx::query("SELECT id, product_id, product_uom_qty, product_uom_id, lot_id, quantity_done, reserved_qty, location_id, location_dest_id FROM stock_move WHERE picking_id = $1 AND state = 'draft'")
            .bind(picking_id)
            .fetch_all(&mut *tx)
            .await?;
        // An empty transfer has nothing to move — validating it would flip it to done with no effect
        // (a phantom transfer that burns a number and locks itself). Reject it (the tx rolls back).
        if moves.is_empty() {
            return Err(DbError::BadInput("cannot validate a transfer with no moves".to_string()));
        }
        use rust_decimal::Decimal;
        let mut products: Vec<i64> = Vec::new();
        // (product, remainder in move unit, src, dst, move uom, lot) for each move not fully processed →
        // a backorder after commit.
        let mut backorders: Vec<(i64, Decimal, i64, i64, Option<i64>, Option<i64>)> = Vec::new();
        for m in &moves {
            let move_id: i64 = m.try_get("id")?;
            let product_id: i64 = m.try_get("product_id")?;
            let ordered: Decimal = m.try_get("product_uom_qty")?;
            let done_field: Decimal = m.try_get("quantity_done")?;
            let move_reserved: Decimal = m.try_get("reserved_qty")?;
            let uom_id: Option<i64> = m.try_get("product_uom_id")?;
            let lot_id: Option<i64> = m.try_get("lot_id")?;
            let src: i64 = m.try_get("location_id")?;
            let dst: i64 = m.try_get("location_dest_id")?;
            // quantity_done == 0 (the default) means "do the full ordered quantity" (all-or-nothing);
            // a positive value validates exactly that much and backorders the rest. All quant math is in
            // the product REFERENCE unit: done_ref = done(move unit) * factor.
            let factor = self.uom_factor(uom_id).await?;
            let done = if done_field > Decimal::ZERO { done_field } else { ordered };
            let mut done_ref = (done * factor).round_dp(6);
            // Serial-tracked product: a serial is exactly one unit and must carry its lot. Enforce both
            // before touching a quant, so a serial can never be split or move anonymously.
            let tracking: Option<String> = sqlx::query_scalar::<_, Option<String>>(
                "SELECT t.tracking FROM product_product p JOIN product_template t ON p.product_tmpl_id = t.id WHERE p.id = $1",
            )
            .bind(product_id)
            .fetch_optional(&mut *tx)
            .await?
            .flatten();
            if tracking.as_deref() == Some("serial") {
                if lot_id.is_none() {
                    return Err(DbError::BadInput("a serial-tracked move requires a serial number (lot_id)".to_string()));
                }
                if done_ref != Decimal::ONE {
                    return Err(DbError::BadInput("a serial number is exactly one unit; the move quantity must be 1".to_string()));
                }
            }
            // Over-delivery guard: a move OUT of an INTERNAL location can take only what is AVAILABLE
            // there — on-hand minus what OTHER moves have reserved, plus back this move's own
            // reservation (so a pre-reserved move can still deliver what it claimed). Stock never goes
            // negative. An external source (supplier/customer) is not clamped.
            let src_usage: Option<String> = sqlx::query_scalar("SELECT usage FROM stock_location WHERE id = $1")
                .bind(src)
                .fetch_optional(&mut *tx)
                .await?;
            if src_usage.as_deref() == Some("internal") {
                let row = sqlx::query("SELECT quantity, COALESCE(reserved_quantity, 0) AS reserved_quantity FROM stock_quant WHERE product_id = $1 AND location_id = $2 AND COALESCE(lot_id, 0) = COALESCE($3, 0)")
                    .bind(product_id)
                    .bind(src)
                    .bind(lot_id)
                    .fetch_optional(&mut *tx)
                    .await?;
                let (on_hand, reserved) = match row {
                    Some(r) => (r.try_get::<Decimal, _>("quantity")?, r.try_get::<Decimal, _>("reserved_quantity")?),
                    None => (Decimal::ZERO, Decimal::ZERO),
                };
                let available = on_hand - reserved + move_reserved;
                if done_ref > available {
                    done_ref = available;
                }
                if done_ref < Decimal::ZERO {
                    done_ref = Decimal::ZERO;
                }
            }
            // Back-convert the (possibly clamped) reference quantity to the move unit for storage + the
            // backorder remainder. factor is always > 0 (uom_factor floors at 1).
            let done = (done_ref / factor).round_dp(6);
            // Source loses `done_ref` and frees this move's reservation in full (the move is now done and
            // will never deliver again — any unused reservation must not leak; the backorder re-reserves
            // from scratch). Destination gains `done_ref`. The quant is keyed (product, location, lot);
            // GREATEST floors reserved at 0.
            if done_ref > Decimal::ZERO {
                sqlx::query(
                    "INSERT INTO stock_quant (product_id, location_id, quantity, reserved_quantity, lot_id) VALUES ($1, $2, $3, 0, $5) \
                     ON CONFLICT (product_id, location_id, COALESCE(lot_id, 0)) DO UPDATE SET \
                       quantity = stock_quant.quantity + $3, \
                       reserved_quantity = GREATEST(0, stock_quant.reserved_quantity - $4)",
                )
                .bind(product_id)
                .bind(src)
                .bind(-done_ref)
                .bind(move_reserved)
                .bind(lot_id)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "INSERT INTO stock_quant (product_id, location_id, quantity, reserved_quantity, lot_id) VALUES ($1, $2, $3, 0, $4) \
                     ON CONFLICT (product_id, location_id, COALESCE(lot_id, 0)) DO UPDATE SET quantity = stock_quant.quantity + $3",
                )
                .bind(product_id)
                .bind(dst)
                .bind(done_ref)
                .bind(lot_id)
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query("UPDATE stock_move SET state = 'done', quantity_done = $2 WHERE id = $1")
                .bind(move_id)
                .bind(done)
                .execute(&mut *tx)
                .await?;
            let remainder = ordered - done;
            if remainder > Decimal::ZERO {
                backorders.push((product_id, remainder, src, dst, uom_id, lot_id));
            }
            products.push(product_id);
        }
        // De-duplicate once (a product may appear on several moves) for the on-hand recompute below.
        products.sort_unstable();
        products.dedup();

        sqlx::query("UPDATE stock_picking SET state = 'done', name = $2 WHERE id = $1")
            .bind(picking_id)
            .bind(&number)
            .execute(&mut *tx)
            .await?;

        // Re-materialize on-hand for the moved products from the (just-updated) internal quants.
        if !products.is_empty() {
            sqlx::query(
                "UPDATE product_product p SET qty_available = COALESCE( \
                   (SELECT SUM(q.quantity) FROM stock_quant q JOIN stock_location l ON l.id = q.location_id \
                    WHERE q.product_id = p.id AND l.usage = 'internal'), 0) \
                 WHERE p.id = ANY($1)",
            )
            .bind(&products)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        // Spill any unfulfilled remainder into a new DRAFT backorder transfer, created via the ORM (so it
        // gets its defaults + nested moves). Mirrors create_sale_invoice's documented non-atomicity: if
        // this fails the original transfer is still validated, the remainder just is not backordered.
        if !backorders.is_empty() {
            let elevated = ctx.sudo();
            let ptype = picking.get("picking_type").and_then(|v| v.as_str()).unwrap_or("internal");
            let ploc = picking.get("location_id").and_then(|v| v.as_i64());
            let pdest = picking.get("location_dest_id").and_then(|v| v.as_i64());
            let bo_moves: Vec<serde_json::Value> = backorders
                .iter()
                .map(|(product_id, remainder, src, dst, uom_id, lot_id)| {
                    serde_json::json!({
                        "product_id": product_id, "product_uom_qty": remainder.to_string(),
                        "product_uom_id": uom_id, "lot_id": lot_id, "location_id": src, "location_dest_id": dst
                    })
                })
                .collect();
            let bo_payload = serde_json::json!({
                "picking_type": ptype, "location_id": ploc, "location_dest_id": pdest,
                "backorder_id": picking_id, "move_ids": bo_moves
            });
            self.insert_secured(&picking_model, &elevated, &[], &[], bo_payload.as_object().unwrap()).await?;
        }

        Ok(number)
    }

    /// Creates a DRAFT stock transfer from a confirmed order: one `stock.move` per goods line, between
    /// two locations resolved by usage and pinned to the order's company (company-deterministic, like
    /// the invoice account lookup). Gated on the caller's order WRITE; the picking + moves are created
    /// elevated, so a salesperson/buyer need not also hold stock-create rights. The transfer is left in
    /// draft for the warehouse to `validate` — this method never moves stock itself. Returns the new
    /// picking id. (v1: no delivered/received-qty tracking, so calling it twice makes two drafts.)
    async fn create_order_picking(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        order_model_name: &str,
        order_id: i64,
        operation: &'static str,
        required_state: &str,
        picking_type: &str,
        src_usage: &str,
        dst_usage: &str,
    ) -> Result<i64, DbError> {
        let order_model = resolve_registered(order_model_name).map_err(DbError::BadInput)?;
        let line_model = resolve_registered(&format!("{order_model_name}.line")).map_err(DbError::BadInput)?;
        let location_model = resolve_registered("stock.location")
            .map_err(|_| DbError::BadInput("install the stock module to create transfers".to_string()))?;
        let picking_model = resolve_registered("stock.picking").map_err(DbError::BadInput)?;
        let move_model = resolve_registered("stock.move").map_err(DbError::BadInput)?;

        if !check_access(Operation::Write, order_model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: order_model.name.to_string(), operation });
        }
        let order = self
            .find_one_secured(&order_model, ctx, acls, rules, order_id)
            .await?
            .ok_or_else(|| DbError::BadInput("order not found or not permitted".to_string()))?;
        let state = order.get("state").and_then(|v| v.as_str()).unwrap_or("");
        if state != required_state {
            return Err(DbError::BadInput(format!("order is not ready (state '{state}', expected '{required_state}')")));
        }
        let partner = order.get("partner_id").and_then(|v| v.as_i64());
        // Pin the order's company (else the caller's active company), so the endpoints resolve under
        // exactly that company and a shared order only ever uses shared locations.
        let company = order.get("company_id").and_then(|v| v.as_i64()).or(ctx.company_id);

        // Resolving the endpoints and creating the transfer are a system effect (like the GL posting in
        // create_sale_invoice), authorized by the caller's order WRITE above — so they run elevated.
        let elevated = ctx.sudo();
        let src = self
            .first_match(&location_model, &elevated, "usage", src_usage, company)
            .await?
            .ok_or_else(|| DbError::BadInput(format!("no '{src_usage}' location is configured for this company")))?;
        let dst = self
            .first_match(&location_model, &elevated, "usage", dst_usage, company)
            .await?
            .ok_or_else(|| DbError::BadInput(format!("no '{dst_usage}' location is configured for this company")))?;

        // One move per line carrying a product and a positive quantity.
        let lines = self
            .find_secured(&line_model, ctx, acls, rules, Some(&Domain::field("order_id").eq(order_id)))
            .await?;
        let mut moves: Vec<(i64, rust_decimal::Decimal)> = Vec::new();
        for l in &lines {
            let qty: rust_decimal::Decimal =
                l.get("product_uom_qty").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or_default();
            if let Some(pid) = l.get("product_id").and_then(|v| v.as_i64()) {
                if qty > rust_decimal::Decimal::ZERO {
                    moves.push((pid, qty));
                }
            }
        }
        if moves.is_empty() {
            return Err(DbError::BadInput("the order has no goods lines to transfer".to_string()));
        }

        let mut payload = serde_json::json!({
            "picking_type": picking_type, "location_id": src, "location_dest_id": dst, "state": "draft",
        });
        if let Some(p) = partner {
            payload["partner_id"] = p.into();
        }
        if let Some(c) = company {
            payload["company_id"] = c.into();
        }
        let picking_id =
            self.insert_secured(&picking_model, &elevated, &[], &[], payload.as_object().unwrap()).await?;
        for (pid, qty) in moves {
            let mp = serde_json::json!({
                "picking_id": picking_id, "product_id": pid, "product_uom_qty": qty.to_string(),
                "location_id": src, "location_dest_id": dst, "state": "draft",
            });
            self.insert_secured(&move_model, &elevated, &[], &[], mp.as_object().unwrap()).await?;
        }
        Ok(picking_id)
    }

    /// Creates a draft delivery (Stock → Customers) for a confirmed sale order. See
    /// [`Db::create_order_picking`].
    pub async fn create_delivery(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        order_id: i64,
    ) -> Result<i64, DbError> {
        self.create_order_picking(ctx, acls, rules, "sale.order", order_id, "create_delivery", "sale", "delivery", "internal", "customer")
            .await
    }

    /// Creates a draft receipt (Vendors → Stock) for a confirmed purchase order. See
    /// [`Db::create_order_picking`].
    pub async fn create_receipt(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        order_id: i64,
    ) -> Result<i64, DbError> {
        self.create_order_picking(ctx, acls, rules, "purchase.order", order_id, "create_receipt", "purchase", "receipt", "supplier", "internal")
            .await
    }

    /// Sets a variant's own `active` flag inside `tx` (archive / reactivate during reconciliation). A
    /// direct UPDATE on the variant's own column — never the delegated template field — so it touches
    /// only this variant, not the shared template or its siblings.
    async fn set_variant_active_in_tx(
        &self,
        variant: &ResolvedModel,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        id: i64,
        active: bool,
    ) -> Result<(), DbError> {
        sqlx::query(&format!("UPDATE {} SET active = $1 WHERE id = $2", variant.table))
            .bind(active)
            .bind(id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// Recomputes `parent`'s aggregate computed columns from its current children (a direct UPDATE,
    /// so it never re-enters the secured write path / re-triggers). Serialized per parent with an
    /// advisory lock so concurrent child writes can't lose-update the aggregate. All reads and the
    /// write run on the SAME locked connection, so holding the lock never contends for a second
    /// pool connection.
    async fn recompute_parent(&self, parent: &ResolvedModel, parent_id: i64) -> Result<(), DbError> {
        // Multi-level cascade: recompute this aggregate parent, then propagate to ITS aggregate
        // parents (line → order → customer rollups). Iterative with a depth cap + a visited set so a
        // cyclic or deep model graph can never loop forever.
        let mut work: Vec<(ResolvedModel, i64, usize)> = vec![(parent.clone(), parent_id, 0)];
        let mut seen: Vec<(&'static str, i64)> = Vec::new();
        while let Some((m, pid, depth)) = work.pop() {
            if depth > MAX_CASCADE_DEPTH || seen.iter().any(|&(n, i)| n == m.name && i == pid) {
                continue;
            }
            seen.push((m.name, pid));
            if computed_fields(&m).is_empty() {
                continue;
            }
            // Per-row advisory lock across read+recompute+write, so concurrent recomputes of the
            // same row serialize and the final aggregate is correct.
            let mut lock = self.pool.begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
                .bind(format!("agg:{}:{}", m.table, pid))
                .execute(&mut *lock)
                .await?;
            recompute_columns_on(&mut lock, &m, pid).await?;
            lock.commit().await?;
            // Propagate upward: if m is itself a child of a higher aggregate, enqueue that grandparent.
            for (gp, inverse) in parents_of(m.name) {
                if let Some(gpid) = self.read_fk(m.table, inverse, pid).await? {
                    work.push((gp, gpid, depth + 1));
                }
            }
        }
        Ok(())
    }
    // ponytail: aggregate recompute on a child UPDATE/DELETE still runs post-commit (advisory-locked
    // and idempotent); a process crash in that window leaves a stale parent total until the next
    // write. Fold into the write tx like insert_secured if that window ever matters.

    /// For a freshly inserted child, recompute the parent it points to (FK from the child values).
    async fn recompute_parents_of(
        &self,
        child: &ResolvedModel,
        child_values: &BTreeMap<String, Value>,
    ) -> Result<(), DbError> {
        for (parent, inverse) in parents_of(child.name) {
            if let Some(Value::Int(pid)) = child_values.get(inverse) {
                self.recompute_parent(&parent, *pid).await?;
            }
        }
        Ok(())
    }

    /// The (parent, parent_id) pairs a child currently points to (used before a delete and to
    /// capture the old/new parents of an updated child).
    async fn parent_targets(
        &self,
        child: &ResolvedModel,
        child_id: i64,
    ) -> Result<Vec<(ResolvedModel, i64)>, DbError> {
        let mut out = Vec::new();
        for (parent, inverse) in parents_of(child.name) {
            if let Some(pid) = self.read_fk(child.table, inverse, child_id).await? {
                out.push((parent, pid));
            }
        }
        Ok(out)
    }

    /// Reads a single FK (the inverse) column value for a child row.
    async fn read_fk(&self, table: &str, field: &str, id: i64) -> Result<Option<i64>, DbError> {
        let sql = format!("SELECT {field} FROM {table} WHERE id = $1");
        let v: Option<Option<i64>> = sqlx::query_scalar(&sql).bind(id).fetch_optional(&self.pool).await?;
        Ok(v.flatten())
    }

    /// Deletes row `id`, enforcing ACL Delete and the Delete record rule. Returns rows deleted.
    pub async fn delete_secured(
        &self,
        model: &ResolvedModel,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        id: i64,
    ) -> Result<u64, DbError> {
        if !check_access(Operation::Delete, model.name, ctx, acls) {
            return Err(DbError::AccessDenied {
                model: model.name.to_string(),
                operation: "delete",
            });
        }
        // Capture the parents this child points to before it is gone, to recompute them after.
        let parents = self.parent_targets(model, id).await?;
        let mut params: Vec<Value> = vec![Value::Int(id)];
        let mut where_sql = match record_rule_domain(Operation::Delete, model.name, ctx, rules) {
            Some(rule) => format!("id = $1 AND {}", rule.compile_into(model, &mut params)?),
            None => "id = $1".to_string(),
        };
        where_sql.push_str(&company_clause(model, ctx, &mut params)?);
        let sql = format!("DELETE FROM {} WHERE {}", model.table, where_sql);
        let mut q = sqlx::query(&sql);
        for v in &params {
            q = bind_query(q, v);
        }
        let affected = q.execute(&self.pool).await?.rows_affected();
        // Polymorphic-integrity fix: a record's attachments and (if mailed) its thread are linked by
        // (res_model, res_id), which the metamodel can't express as an FK. Clean them on this — Meshble's
        // ONLY delete path (unlike Odoo, where bulk SQL orphans them).
        if affected > 0 {
            self.cleanup_attachments(model.name, id).await?;
            if is_mailed(model.name) {
                self.cleanup_thread(model.name, id).await?;
            }
        }
        for (parent, pid) in parents {
            self.recompute_parent(&parent, pid).await?;
        }
        Ok(affected)
    }

    /// The database clock as an ISO-8601 string, for stamping mail messages/tracking/activities.
    /// One clock (the DB) for every mail row, so ordering is consistent regardless of caller.
    pub async fn now(&self) -> Result<String, DbError> {
        Ok(sqlx::query_scalar::<_, String>("SELECT now()::text").fetch_one(&self.pool).await?)
    }

    /// The database's current date as `YYYY-MM-DD`, for deriving an activity's state (overdue/today/
    /// planned) by lexical comparison against its ISO `date_deadline`. One clock = one source of truth.
    pub async fn today(&self) -> Result<String, DbError> {
        Ok(sqlx::query_scalar::<_, String>("SELECT current_date::text").fetch_one(&self.pool).await?)
    }

    /// Deletes a record's mail thread across all thread tables. Tolerates a thread table not being
    /// migrated yet (the mail module may be linked but not installed) — nothing to clean, not an error.
    async fn cleanup_thread(&self, model_name: &str, id: i64) -> Result<(), DbError> {
        // mail_tracking is keyed by message_id (not res_model/res_id): remove the record's tracking
        // rows via its messages first, then the (res_model, res_id)-keyed thread tables.
        self.exec_tolerant(
            "DELETE FROM mail_tracking WHERE message_id IN (SELECT id FROM mail_message WHERE res_model = $1 AND res_id = $2)",
            model_name, id,
        ).await?;
        for table in THREAD_TABLES {
            let sql = format!("DELETE FROM {table} WHERE res_model = $1 AND res_id = $2");
            self.exec_tolerant(&sql, model_name, id).await?;
        }
        Ok(())
    }

    /// Removes a deleted record's attachment rows (polymorphic `(res_model, res_id)`). The blobs are
    /// NOT reclaimed here: a content-addressed blob can be shared across records by dedup, so freeing it
    /// is a separate mark-sweep GC (deferred to the operations milestone). Tolerates the attachment
    /// table not being migrated. Runs on EVERY delete (any record may carry attachments), not just mailed.
    async fn cleanup_attachments(&self, model_name: &str, id: i64) -> Result<(), DbError> {
        self.exec_tolerant(
            "DELETE FROM meshble_attachment WHERE res_model = $1 AND res_id = $2",
            model_name,
            id,
        )
        .await
    }

    /// Runs a `(res_model, res_id)`-parameterized DELETE, tolerating a missing table (42P01 →
    /// nothing to clean). Any other error propagates.
    async fn exec_tolerant(&self, sql: &str, model_name: &str, id: i64) -> Result<(), DbError> {
        match sqlx::query(sql).bind(model_name).bind(id).execute(&self.pool).await {
            Ok(_) => Ok(()),
            Err(e) if is_undefined_table(&e) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Records a field-change audit entry in the chatter: one `notification` message on
    /// `(model_name, res_id)` plus a typed `mail.tracking` row per change. Best-effort and run
    /// AFTER the business write commits, so a missing mail schema or a tracking failure never aborts
    /// or rolls back the user's write (see the call site in `update_secured`). Atomic in itself.
    async fn write_tracking(
        &self,
        model_name: &str,
        res_id: i64,
        author: i64,
        changes: &[(String, Option<String>, Option<String>)],
    ) -> Result<(), DbError> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        let mid: i64 = match sqlx::query_scalar(
            "INSERT INTO mail_message (res_model, res_id, author_id, message_type, date) \
             VALUES ($1, $2, $3, 'notification', now()) RETURNING id",
        )
        .bind(model_name)
        .bind(res_id)
        .bind(author)
        .fetch_one(&mut *tx)
        .await
        {
            Ok(v) => v,
            // mail schema not migrated → skip the audit entry entirely (nothing partial written).
            Err(e) if is_undefined_table(&e) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        for (field, old, new) in changes {
            sqlx::query("INSERT INTO mail_tracking (message_id, field, old_value, new_value) VALUES ($1, $2, $3, $4)")
                .bind(mid)
                .bind(field)
                .bind(old)
                .bind(new)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The tracking rows for the given message ids, as JSON `{message_id, field, old_value,
    /// new_value}` (ordered). Tolerates an unmigrated `mail_tracking`. For embedding audit entries
    /// into a thread read.
    pub async fn tracking_for(&self, message_ids: &[i64]) -> Result<Vec<Json>, DbError> {
        if message_ids.is_empty() {
            return Ok(vec![]);
        }
        let rows = match sqlx::query(
            "SELECT message_id, field, old_value, new_value FROM mail_tracking WHERE message_id = ANY($1) ORDER BY id",
        )
        .bind(message_ids)
        .fetch_all(&self.pool)
        .await
        {
            Ok(r) => r,
            Err(e) if is_undefined_table(&e) => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "message_id": r.get::<i64, _>("message_id"),
                    "field": r.get::<String, _>("field"),
                    "old_value": r.get::<Option<String>, _>("old_value"),
                    "new_value": r.get::<Option<String>, _>("new_value"),
                })
            })
            .collect())
    }

    /// Creates the mail subsystem's lookup indexes (idempotent, tolerant of unmigrated tables): the
    /// polymorphic `mail_message(res_model, res_id)` thread lookup and `mail_tracking(message_id)`.
    /// The metamodel can't express indexes yet and the mail tables are a known framework concern, so
    /// the framework ensures them directly — like the sequence schema. Run during migrate.
    pub async fn ensure_mail_indexes(&self) -> Result<(), DbError> {
        for sql in [
            "CREATE INDEX IF NOT EXISTS mail_message_res_idx ON mail_message (res_model, res_id)",
            "CREATE INDEX IF NOT EXISTS mail_tracking_message_idx ON mail_tracking (message_id)",
            // Composite UNIQUE makes following idempotent (one subscription per user per record) and
            // indexes the follower lookup. The metamodel can't express composite uniqueness yet.
            "CREATE UNIQUE INDEX IF NOT EXISTS mail_follower_uniq ON mail_follower (res_model, res_id, user_id)",
            // ir.attachment shares the polymorphic shape; index its host lookup (list + delete-cleanup).
            "CREATE INDEX IF NOT EXISTS meshble_attachment_res_idx ON meshble_attachment (res_model, res_id)",
        ] {
            match sqlx::query(sql).execute(&self.pool).await {
                Ok(_) => {}
                Err(e) if is_undefined_table(&e) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Stock indexes: one quant per (product, location, lot) — a composite UNIQUE the metamodel can't
    /// express, and the anchor for the `ON CONFLICT (product_id, location_id, COALESCE(lot_id, 0))` upsert
    /// the move-done mechanism uses. `COALESCE(lot_id, 0)` collapses untracked bulk (NULL lot) into a
    /// single row per (product, location), so legacy/untracked stock stays one quant. The new index is
    /// created BEFORE the old single-key one is dropped, so a quant is always uniquely constrained.
    /// Tolerates an unmigrated `stock_quant` (the stock module isn't installed). Run during migrate.
    pub async fn ensure_stock_indexes(&self) -> Result<(), DbError> {
        for sql in [
            "CREATE UNIQUE INDEX IF NOT EXISTS stock_quant_product_location_lot ON stock_quant (product_id, location_id, COALESCE(lot_id, 0))",
            "DROP INDEX IF EXISTS stock_quant_product_location",
        ] {
            match sqlx::query(sql).execute(&self.pool).await {
                Ok(_) => {}
                Err(e) if is_undefined_table(&e) => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

/// True iff the error is Postgres `undefined_table` (42P01) — a mail thread table not yet migrated.
fn is_undefined_table(e: &sqlx::Error) -> bool {
    e.as_database_error().and_then(|d| d.code()).as_deref() == Some("42P01")
}

/// Reads the given columns of one row as Postgres `::text` (None for NULL / absent row). Used for the
/// tracking diff: rendering old and new through the SAME `::text` cast makes the comparison
/// representation-independent (no Date/Datetime/Float text-format mismatch). `lock` adds FOR UPDATE so
/// the old snapshot is exactly what the subsequent UPDATE overwrites.
async fn snapshot_text(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    table: &str,
    cols: &[&'static str],
    id: i64,
    lock: bool,
) -> Result<Vec<(&'static str, Option<String>)>, DbError> {
    if cols.is_empty() {
        return Ok(Vec::new());
    }
    let list = cols.iter().map(|c| format!("{c}::text AS {c}")).collect::<Vec<_>>().join(", ");
    let sql = format!("SELECT {list} FROM {table} WHERE id = $1{}", if lock { " FOR UPDATE" } else { "" });
    let mut out = Vec::new();
    if let Some(row) = sqlx::query(&sql).bind(id).fetch_optional(&mut **tx).await? {
        for c in cols {
            out.push((*c, row.try_get::<Option<String>, _>(*c).ok().flatten()));
        }
    }
    Ok(out)
}

/// Mail subsystem thread tables, all keyed by the polymorphic `(res_model, res_id)` link. Cleaned up
/// when a mailed record is deleted (see [`Db::cleanup_thread`]). Listed here so the integrity hook
/// covers every thread table even though the models land in separate mail slices.
const THREAD_TABLES: &[&str] = &["mail_message", "mail_activity", "mail_follower"];

/// Validates a write payload against the model: every key must be a writable stored column, every
/// value must match its field kind, and `null` is rejected for required fields. With
/// `require_all` (create), every required column must be present. This is the input-validation
/// boundary for writes — required/option enforcement happens here (clean BadInput), not as an
/// opaque Postgres constraint error.
fn validate_write_values(
    model: &ResolvedModel,
    values: &Map<String, Json>,
    require_all: bool,
) -> Result<Vec<(&'static str, Value)>, DbError> {
    let mut out = Vec::new();
    for (key, jv) in values {
        let field = model
            .fields
            .iter()
            .find(|f| f.name == key)
            .ok_or_else(|| DbError::BadInput(format!("unknown field '{key}'")))?;
        if !field.has_column() {
            return Err(DbError::BadInput(format!("field '{key}' is not a stored column")));
        }
        if field.is_computed() {
            return Err(DbError::BadInput(format!("field '{key}' is computed and not writable")));
        }
        if field_is_readonly(model.name, field.name) {
            return Err(DbError::BadInput(format!("field '{key}' is read-only and not writable")));
        }
        if jv.is_null() && field.required {
            return Err(DbError::BadInput(format!("field '{key}' is required and cannot be null")));
        }
        out.push((field.name, json_to_value(field, jv)?));
    }
    if require_all {
        for f in &model.fields {
            if f.has_column() && !f.is_computed() && f.required && !values.contains_key(f.name) {
                return Err(DbError::BadInput(format!("field '{}' is required", f.name)));
            }
        }
    }
    Ok(out)
}

/// Hard cap on child commands per One2many field in one write — bounds the work a single request
/// can do inside one transaction (and the time the per-parent advisory lock is held).
const MAX_O2M_COMMANDS: usize = 1000;

/// Maximum aggregate-cascade depth (line → order → customer …), guarding deep/cyclic model graphs.
const MAX_CASCADE_DEPTH: usize = 8;

/// One x2many child command — typed objects (decision D4), not Odoo's positional tuples.
enum O2mCommand {
    Create(Map<String, Json>),
    Update(i64, Map<String, Json>),
    Delete(i64),
}

/// A One2many field's extracted child commands.
struct NestedWrite {
    child: ResolvedModel,
    inverse: &'static str,
    commands: Vec<O2mCommand>,
}

/// A Many2many SET: the target ids the relation should contain after the write (replaces membership).
struct M2mSet {
    relation: &'static str,
    column: &'static str,
    target_column: &'static str,
    ids: Vec<i64>,
}

/// Splits a write payload into scalar columns, One2many child commands, and Many2many sets. Each
/// One2many value is an array of typed commands `{op:'create',values}` / `{op:'update',id,values}` /
/// `{op:'delete',id}` (a bare object = create); each Many2many value is an array of target ids (SET
/// semantics — it replaces the membership). A null Many2many is ignored (no change).
fn split_nested(
    model: &ResolvedModel,
    values: &Map<String, Json>,
) -> Result<(Map<String, Json>, Vec<NestedWrite>, Vec<M2mSet>, Map<String, Json>), DbError> {
    let mut scalars = Map::new();
    let mut nested = Vec::new();
    let mut m2m = Vec::new();
    let mut delegated = Map::new();
    // Names this model delegates to its `_inherits` parent (empty for a non-inheriting model).
    let deleg_names: Vec<&'static str> =
        delegated_fields(model.name).map_err(DbError::BadInput)?.iter().map(|d| d.def.name).collect();
    for (key, jv) in values {
        match model.fields.iter().find(|f| f.name == *key).map(|f| f.kind) {
            Some(FieldKind::Many2many { relation, column, target_column, .. }) => {
                if jv.is_null() {
                    continue; // null = leave the relation unchanged
                }
                let arr = jv
                    .as_array()
                    .ok_or_else(|| DbError::BadInput(format!("'{key}' must be an array of ids")))?;
                let mut ids = Vec::with_capacity(arr.len());
                for v in arr {
                    ids.push(
                        v.as_i64().ok_or_else(|| DbError::BadInput(format!("'{key}': ids must be integers")))?,
                    );
                }
                m2m.push(M2mSet { relation, column, target_column, ids });
            }
            Some(FieldKind::One2many { target, inverse }) => {
                let arr = jv.as_array().ok_or_else(|| {
                    DbError::BadInput(format!("'{key}' must be an array of child commands"))
                })?;
                if arr.len() > MAX_O2M_COMMANDS {
                    return Err(DbError::BadInput(format!(
                        "'{key}': too many child commands ({}, max {MAX_O2M_COMMANDS})",
                        arr.len()
                    )));
                }
                let commands: Vec<O2mCommand> =
                    arr.iter().map(|item| parse_o2m_command(key, item)).collect::<Result<_, _>>()?;
                if !commands.is_empty() {
                    let child = resolve_registered(target).map_err(|e| {
                        DbError::BadInput(format!("unknown child model '{target}': {e}"))
                    })?;
                    nested.push(NestedWrite { child, inverse, commands });
                }
            }
            // Delegated (_inherits) parent field: routed to the parent, not the child's columns.
            _ if deleg_names.contains(&key.as_str()) => {
                delegated.insert(key.clone(), jv.clone());
            }
            // Scalar (or unknown) field: validate_write_values will accept or reject it.
            _ => {
                scalars.insert(key.clone(), jv.clone());
            }
        }
    }
    Ok((scalars, nested, m2m, delegated))
}

/// Applies Many2many SETs in `tx`: for each, clear the record's membership then insert the given
/// target ids (a non-existent target id raises a clean FK error; duplicate ids are coalesced).
async fn apply_m2m_in_tx(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    record_id: i64,
    sets: &[M2mSet],
) -> Result<(), DbError> {
    for s in sets {
        sqlx::query(&format!("DELETE FROM {} WHERE {} = $1", s.relation, s.column))
            .bind(record_id)
            .execute(&mut **tx)
            .await?;
        for &tid in &s.ids {
            sqlx::query(&format!(
                "INSERT INTO {} ({}, {}) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                s.relation, s.column, s.target_column
            ))
            .bind(record_id)
            .bind(tid)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

fn parse_o2m_command(field: &str, item: &Json) -> Result<O2mCommand, DbError> {
    let obj = item
        .as_object()
        .ok_or_else(|| DbError::BadInput(format!("each '{field}' item must be an object")))?;
    let values = || {
        obj.get("values")
            .and_then(|v| v.as_object())
            .cloned()
            .ok_or_else(|| DbError::BadInput(format!("'{field}' command needs a 'values' object")))
    };
    let id = || {
        obj.get("id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| DbError::BadInput(format!("'{field}' command needs an integer 'id'")))
    };
    Ok(match obj.get("op").and_then(|v| v.as_str()) {
        None => {
            if obj.contains_key("id") {
                return Err(DbError::BadInput(format!(
                    "'{field}': an item with 'id' must use op 'update' or 'delete'"
                )));
            }
            O2mCommand::Create(obj.clone())
        }
        Some("create") => O2mCommand::Create(values()?),
        Some("update") => O2mCommand::Update(id()?, values()?),
        Some("delete") => O2mCommand::Delete(id()?),
        Some(other) => return Err(DbError::BadInput(format!("'{field}': unknown op '{other}'"))),
    })
}

/// The multi-company restriction for a company-scoped model, or None when the caller is sudo
/// (unrestricted) or the model has no `company_id`. A non-superuser is ALWAYS restricted (M7
/// default-deny): an allowed set yields `company_id IN (allowed) OR IS NULL`; an EMPTY set yields
/// `company_id IS NULL` only — an unassigned user sees only shared records, never everything. A NULL
/// company_id is a SHARED record, visible to every company (matching Odoo's multi-company semantics).
fn company_filter(model: &ResolvedModel, ctx: &Ctx) -> Option<Domain> {
    if !ctx.company_scoped() {
        return None;
    }
    let scoped = model
        .fields
        .iter()
        .any(|f| f.name == "company_id" && matches!(f.kind, FieldKind::Many2one { .. }));
    if !scoped {
        return None;
    }
    let shared = Domain::field("company_id").is_null();
    Some(if ctx.allowed_company_ids.is_empty() {
        shared // default-deny: no assignment → only shared (NULL-company) rows
    } else {
        Domain::field("company_id").in_(ctx.allowed_company_ids.clone()).or(shared)
    })
}

/// SQL fragment ` AND (<company filter>)` for the id-based write / read-one paths, appending its
/// bound params (placeholder numbering continues after `params`). Empty when the caller is
/// unrestricted, so existing single-company behavior is unchanged.
fn company_clause(model: &ResolvedModel, ctx: &Ctx, params: &mut Vec<Value>) -> Result<String, DbError> {
    match company_filter(model, ctx) {
        Some(d) => Ok(format!(" AND ({})", d.compile_into(model, params)?)),
        None => Ok(String::new()),
    }
}

/// Enforces multi-company scoping on a WRITE payload for a company-scoped model (a Many2one named
/// `company_id`). The single chokepoint reused by parent create, nested child create, and update.
/// For a restricted caller (any non-sudo user, M7 default-deny): an explicit `company_id` must be a
/// non-null id WITHIN the allowed set (no writing a row into a foreign company; no NULL, which would
/// publish a row as shared/visible to everyone); an unset `company_id` on CREATE is defaulted to the
/// caller's active company, and a caller with no active company cannot create a company-scoped row.
/// Only sudo is unaffected (create still defaults to the active company when one is set).
fn apply_company_scope(
    model: &ResolvedModel,
    ctx: &Ctx,
    payload: &mut Map<String, Json>,
    is_create: bool,
) -> Result<(), DbError> {
    let scoped_model = model
        .fields
        .iter()
        .any(|f| f.name == "company_id" && matches!(f.kind, FieldKind::Many2one { .. }));
    if !scoped_model {
        return Ok(());
    }
    // company_scoped() is now "any non-sudo caller" (M7 default-deny), so only sudo falls through to
    // permissive behavior (create still defaults to an active company when one is set).
    let restricted = ctx.company_scoped();
    let denied =
        |op: &'static str| DbError::AccessDenied { model: model.name.to_string(), operation: op };

    match payload.get("company_id") {
        // Explicit NULL = "shared, visible to all companies" — privileged; a scoped caller can't.
        Some(v) if v.is_null() => {
            if restricted {
                return Err(denied("write (null company)"));
            }
        }
        // Explicit id must be one the caller is allowed to act for.
        Some(v) => {
            let cid =
                v.as_i64().ok_or_else(|| DbError::BadInput("company_id must be an integer".to_string()))?;
            if restricted && !ctx.allowed_company_ids.contains(&cid) {
                return Err(denied("write (foreign company)"));
            }
        }
        // Unset on create → default to the active company (or the sole allowed one).
        None if is_create => {
            let active = ctx.company_id.or_else(|| {
                (ctx.allowed_company_ids.len() == 1).then(|| ctx.allowed_company_ids[0])
            });
            match active {
                Some(cid) => {
                    payload.insert("company_id".to_string(), Json::from(cid));
                }
                None if restricted => {
                    return Err(DbError::BadInput(
                        "company_id is required (no single active company in scope)".to_string(),
                    ))
                }
                None => {} // sudo only, no active company → leave NULL (shared)
            }
        }
        None => {}
    }
    Ok(())
}

/// Converts a field's static `default` string into a JSON value shaped for its kind, so a create
/// that omits the field receives the default (then validated like any user-supplied value).
fn default_json(field: &FieldDef) -> Option<Json> {
    let d = field.default?;
    Some(match field.kind {
        FieldKind::Bool => Json::Bool(matches!(d, "true" | "1" | "t" | "yes")),
        FieldKind::Integer | FieldKind::Many2one { .. } | FieldKind::Image => Json::from(d.parse::<i64>().ok()?),
        FieldKind::Float => Json::from(d.parse::<f64>().ok()?),
        // Decimals + dates travel as strings → parsed/validated by json_to_value.
        FieldKind::Decimal { .. } | FieldKind::Text | FieldKind::Html | FieldKind::Selection(_) | FieldKind::Date | FieldKind::Datetime => {
            Json::from(d.to_string())
        }
        FieldKind::One2many { .. } | FieldKind::Many2many { .. } => return None,
    })
}

/// Serde JSON for a typed Value — feeds an action's typed outputs back through the secured write path.
fn value_to_json(v: &Value) -> Json {
    match v {
        Value::Str(s) => Json::from(s.clone()),
        Value::Int(n) => Json::from(*n),
        Value::Float(f) => serde_json::json!(f),
        Value::Decimal(d) => Json::from(d.to_string()),
        Value::Bool(b) => Json::from(*b),
        Value::Null => Json::Null,
        Value::List(_) => Json::Null,
    }
}

/// Fills any unset stored field that declares a `default` (applied on create, before required check).
fn apply_defaults(model: &ResolvedModel, payload: &mut Map<String, Json>) {
    for f in &model.fields {
        // Read-only fields are not user-writable, so their default is NOT injected here (that would be
        // rejected by the writability guard); the create path seeds it as a column after validation.
        if f.has_column() && !f.is_computed() && !field_is_readonly(model.name, f.name) && !payload.contains_key(f.name) {
            if let Some(v) = default_json(f) {
                payload.insert(f.name.to_string(), v);
            }
        }
    }
}

/// Columns for a read-only field's declared default, to seed on create AFTER `validate_write_values`
/// (which would reject the field as not user-writable). Only fields the user did not supply — a
/// user-supplied read-only value stays in the payload and is correctly rejected by the guard.
fn readonly_default_columns(
    model: &ResolvedModel,
    written: &[(&'static str, Value)],
) -> Result<Vec<(&'static str, Value)>, DbError> {
    let mut out = Vec::new();
    for f in &model.fields {
        if f.has_column()
            && field_is_readonly(model.name, f.name)
            && !written.iter().any(|(n, _)| *n == f.name)
        {
            if let Some(jv) = default_json(f) {
                out.push((f.name, json_to_value(f, &jv)?));
            }
        }
    }
    Ok(out)
}

fn json_to_value(field: &FieldDef, jv: &Json) -> Result<Value, DbError> {
    let bad = || {
        DbError::BadInput(format!(
            "value for '{}' is not compatible with field kind {:?}",
            field.name, field.kind
        ))
    };
    Ok(match (&field.kind, jv) {
        (_, Json::Null) => Value::Null,
        (FieldKind::Text, Json::String(s)) => Value::Str(s.clone()),
        // Rich text: sanitize on write with a strict allowlist (ammonia) so stored XSS can never land
        // — <script>, event-handler attributes and javascript: URLs are stripped before the value is
        // ever stored. The stored value is already safe, so any reader/renderer can trust it.
        (FieldKind::Html, Json::String(s)) => Value::Str(ammonia::clean(s)),
        (FieldKind::Selection(opts), Json::String(s)) => {
            if !opts.iter().any(|(k, _)| k == s) {
                return Err(DbError::BadInput(format!(
                    "'{s}' is not a valid option for '{}'",
                    field.name
                )));
            }
            Value::Str(s.clone())
        }
        (FieldKind::Integer | FieldKind::Many2one { .. } | FieldKind::Image, Json::Number(n)) => {
            Value::Int(n.as_i64().ok_or_else(bad)?)
        }
        // Accept a numeric STRING for an integer/relation/image field (HTML number inputs and some
        // clients serialize ids as strings) — coerce at the boundary rather than fail with a type error.
        (FieldKind::Integer | FieldKind::Many2one { .. } | FieldKind::Image, Json::String(s)) => {
            Value::Int(s.trim().parse().map_err(|_| bad())?)
        }
        // Exact decimal: parse from the number's canonical STRING (not f64) so 0.01 etc. are exact;
        // also accept a JSON string (the canonical money representation).
        (FieldKind::Decimal { .. }, Json::Number(n)) => {
            Value::Decimal(n.to_string().parse().map_err(|_| bad())?)
        }
        (FieldKind::Decimal { .. }, Json::String(s)) => {
            Value::Decimal(s.parse().map_err(|_| bad())?)
        }
        (FieldKind::Float, Json::Number(n)) => Value::Float(n.as_f64().ok_or_else(bad)?),
        // Accept a numeric string for a float field too (HTML number inputs).
        (FieldKind::Float, Json::String(s)) => Value::Float(s.trim().parse().map_err(|_| bad())?),
        // Date/Datetime travel as ISO strings; the `::date`/`::timestamptz` placeholder cast parses +
        // validates them in Postgres (a malformed value surfaces as a clean 400 via the SQLSTATE map).
        (FieldKind::Date | FieldKind::Datetime, Json::String(s)) => Value::Str(s.clone()),
        (FieldKind::Bool, Json::Bool(b)) => Value::Bool(*b),
        _ => return Err(bad()),
    })
}

/// The Postgres type a column's INSERT/UPDATE placeholder is cast to. A bound NULL would otherwise
/// be typed `text` by the driver and rejected when assigned to a `bigint`/`numeric`/`boolean` column
/// (e.g. setting a Many2one back to null); casting `$n::<type>` makes an explicit NULL the right type,
/// and is a no-op for non-null values already of that type.
fn pg_cast(kind: &FieldKind) -> &'static str {
    match kind {
        FieldKind::Text | FieldKind::Html | FieldKind::Selection(_) => "text",
        FieldKind::Integer | FieldKind::Many2one { .. } | FieldKind::Image => "bigint",
        FieldKind::Float => "double precision",
        FieldKind::Decimal { .. } => "numeric",
        FieldKind::Bool => "boolean",
        FieldKind::Date => "date",
        FieldKind::Datetime => "timestamptz",
        // No column; never reached in a SET/VALUES clause.
        FieldKind::One2many { .. } | FieldKind::Many2many { .. } => "text",
    }
}

/// The cast type for a model column (defaults to `text` if the column is unknown — unreachable, since
/// columns come from validated field names).
fn col_cast(model: &ResolvedModel, col: &str) -> &'static str {
    model.fields.iter().find(|f| f.name == col).map(|f| pg_cast(&f.kind)).unwrap_or("text")
}

/// Binds a domain parameter into a non-scalar query (used for UPDATE/DELETE and the create check).
fn bind_query<'q>(q: Query<'q, Postgres, PgArguments>, v: &Value) -> Query<'q, Postgres, PgArguments> {
    match v {
        Value::Str(s) => q.bind(s.clone()),
        Value::Int(n) => q.bind(*n),
        Value::Float(f) => q.bind(*f),
        Value::Decimal(d) => q.bind(*d),
        Value::Bool(b) => q.bind(*b),
        Value::Null => q.bind(Option::<String>::None),
        Value::List(_) => q,
    }
}

/// Reads a row's stored field values into a typed map, on a caller-provided connection (a pooled
/// one, or the very tx that wrote the row — so recompute can see uncommitted children).
async fn read_record_on(
    conn: &mut sqlx::PgConnection,
    model: &ResolvedModel,
    id: i64,
) -> Result<Option<BTreeMap<String, Value>>, DbError> {
    let sql = format!("SELECT {} FROM {} WHERE id = $1", select_columns(model), model.table);
    let row = sqlx::query(&sql).bind(id).fetch_optional(&mut *conn).await?;
    Ok(row.map(|r| record_to_values(model, &r)))
}

/// Loads the One2many children of `parent_id` (one entry per o2m field), on `conn`.
async fn read_children_on(
    conn: &mut sqlx::PgConnection,
    parent: &ResolvedModel,
    parent_id: i64,
) -> Result<Children, DbError> {
    let mut children = Children::new();
    for f in &parent.fields {
        if let FieldKind::One2many { target, inverse } = f.kind {
            let child = match resolve_registered(target) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let sql =
                format!("SELECT {} FROM {} WHERE {inverse} = $1", select_columns(&child), child.table);
            let rows = sqlx::query(&sql).bind(parent_id).fetch_all(&mut *conn).await?;
            children
                .insert(f.name.to_string(), rows.iter().map(|r| record_to_values(&child, r)).collect());
        }
    }
    Ok(children)
}

/// Runs the model's `@api.constrains` constraints over the just-written record `id` on `conn`,
/// re-reading the record + its One2many children, and maps a violation to a typed `BadInput` (which
/// rolls back the surrounding transaction). `changed` = the written field names, or None on create
/// (every constraint runs). Caller gates on `has_constraints` so the extra read happens only when needed.
async fn check_constraints_in_tx(
    model: &ResolvedModel,
    conn: &mut sqlx::PgConnection,
    id: i64,
    changed: Option<&[String]>,
) -> Result<(), DbError> {
    // The row was just written on THIS connection, so it must be visible. If it isn't, fail closed —
    // never commit a record whose constraints went unchecked (a validation gate, not best-effort).
    let record = read_record_on(&mut *conn, model, id).await?.ok_or_else(|| {
        DbError::BadInput(format!("constraint check: '{}' row {id} vanished inside its own write", model.name))
    })?;
    let children = read_children_on(&mut *conn, model, id).await?;
    check_constraints(model.name, changed, &record, &children).map_err(DbError::BadInput)
}

/// Recomputes `parent`'s stored computed columns from its current children and writes them, all on
/// `conn`. Works on a pooled connection, an advisory-locked tx, or the tx that just inserted the
/// children (making the aggregate atomic with them). No-op when the model has no computed columns.
async fn recompute_columns_on(
    conn: &mut sqlx::PgConnection,
    parent: &ResolvedModel,
    parent_id: i64,
) -> Result<(), DbError> {
    let computed = computed_fields(parent);
    if computed.is_empty() {
        return Ok(());
    }
    let mut record = match read_record_on(&mut *conn, parent, parent_id).await? {
        Some(r) => r,
        None => return Ok(()),
    };
    let children = read_children_on(&mut *conn, parent, parent_id).await?;
    compute_stored(parent, &mut record, &children);
    let set: Vec<String> =
        computed.iter().enumerate().map(|(i, name)| format!("{} = ${}::{}", name, i + 1, col_cast(parent, name))).collect();
    let sql = format!("UPDATE {} SET {} WHERE id = ${}", parent.table, set.join(", "), computed.len() + 1);
    let mut q = sqlx::query(&sql);
    for name in &computed {
        q = bind_query(q, record.get(*name).unwrap_or(&Value::Null));
    }
    q = bind_query(q, &Value::Int(parent_id));
    q.execute(&mut *conn).await?;
    Ok(())
}

/// Builds the SELECT column list for a model. NUMERIC columns are cast to float8 so they decode
/// into `f64` without a decimal dependency. Identifiers come from the model, never user input.
/// Registered models that have a One2many field targeting `child_model_name`, paired with the
/// inverse FK column — i.e. the aggregate parents to recompute when such a child changes.
fn parents_of(child_model_name: &str) -> Vec<(ResolvedModel, &'static str)> {
    let mut out = Vec::new();
    if let Ok(models) = resolve_all_registered() {
        for m in models {
            for f in &m.fields {
                if let FieldKind::One2many { target, inverse } = f.kind {
                    if target == child_model_name {
                        out.push((m.clone(), inverse));
                    }
                }
            }
        }
    }
    out
}

fn select_columns(model: &ResolvedModel) -> String {
    let mut cols = vec!["id".to_string()];
    for f in &model.fields {
        if f.has_column() {
            match f.kind {
                // Read date/timestamp columns as ISO text so they decode into String without a chrono
                // decoder; Decimal stays NUMERIC (rust_decimal), Float stays float8.
                FieldKind::Date | FieldKind::Datetime => cols.push(format!("{0}::text AS {0}", f.name)),
                _ => cols.push(f.name.to_string()),
            }
        } else if let Some(path) = related_path(model.name, f.name) {
            // Related field: resolve its value with a correlated subquery, aliased to the field name.
            if let Ok(sq) = related_subquery(model, path) {
                cols.push(format!("{sq} AS {}", f.name));
            }
        } else if let FieldKind::Many2many { relation, column, target_column, .. } = f.kind {
            // Many2many: an aggregated array of the target ids from the junction table.
            cols.push(format!(
                "(SELECT COALESCE(array_agg({tc} ORDER BY {tc}), '{{}}'::bigint[]) FROM {rel} WHERE {col} = {tbl}.id) AS {name}",
                tc = target_column, rel = relation, col = column, tbl = model.table, name = f.name
            ));
        }
    }
    // Delegated (_inherits) fields: read from the parent through the child's `via` FK, one correlated
    // subquery each (same shape as a related field). Not in `model.fields`, so appended here.
    for d in delegated_fields(model.name).unwrap_or_default() {
        let read = match d.def.kind {
            FieldKind::Date | FieldKind::Datetime => format!("{}::text", d.def.name),
            _ => d.def.name.to_string(),
        };
        cols.push(format!(
            "(SELECT {read} FROM {ptable} WHERE id = {ctable}.{via}) AS {name}",
            ptable = d.parent_table, ctable = model.table, via = d.via, name = d.def.name
        ));
    }
    cols.join(", ")
}

/// Builds the correlated subquery resolving a related field's `path` (e.g. "order_id.currency_id")
/// for the current row: leading segments are Many2one hops, the last is the mirrored field (cast to
/// ::text for Date/Datetime, like a normal date column read). Read-only — it returns one value.
fn related_subquery(model: &ResolvedModel, path: &str) -> Result<String, DbError> {
    let segs: Vec<&str> = path.split('.').collect();
    if segs.len() < 2 {
        return Err(DbError::BadInput(format!("related path '{path}' must traverse a relation")));
    }
    let first = model
        .fields
        .iter()
        .find(|f| f.name == segs[0])
        .ok_or_else(|| DbError::BadInput(format!("related path: unknown field '{}'", segs[0])))?;
    let target = match &first.kind {
        FieldKind::Many2one { target } => target,
        _ => return Err(DbError::BadInput(format!("related path: '{}' is not a Many2one", segs[0]))),
    };
    let mut cur = resolve_registered(target).map_err(DbError::BadInput)?;
    let mut id_expr = format!("{}.{}", model.table, segs[0]);
    for (i, seg) in segs.iter().enumerate().skip(1) {
        let f = cur
            .fields
            .iter()
            .find(|f| f.name == *seg)
            .ok_or_else(|| DbError::BadInput(format!("related path: unknown field '{seg}' on '{}'", cur.name)))?;
        if i == segs.len() - 1 {
            let read = match f.kind {
                FieldKind::Date | FieldKind::Datetime => format!("{seg}::text"),
                _ => seg.to_string(),
            };
            return Ok(format!("(SELECT {read} FROM {} WHERE id = {id_expr})", cur.table));
        }
        let next = match &f.kind {
            FieldKind::Many2one { target } => target,
            _ => return Err(DbError::BadInput(format!("related path: '{seg}' is not a Many2one"))),
        };
        id_expr = format!("(SELECT {seg} FROM {} WHERE id = {id_expr})", cur.table);
        cur = resolve_registered(next).map_err(DbError::BadInput)?;
    }
    unreachable!("the loop returns on the final segment")
}

/// Decodes one selected column (`name`, aliased to the field name) into a typed [`Value`] per its
/// kind. NULL → `Value::Null`. Shared by own-field and delegated-field decoding.
fn decode_value(row: &PgRow, name: &str, kind: &FieldKind) -> Value {
    match kind {
        FieldKind::Text | FieldKind::Html | FieldKind::Selection(_) => {
            row.try_get::<Option<String>, _>(name).ok().flatten().map(Value::Str).unwrap_or(Value::Null)
        }
        FieldKind::Integer | FieldKind::Many2one { .. } | FieldKind::Image => {
            row.try_get::<Option<i64>, _>(name).ok().flatten().map(Value::Int).unwrap_or(Value::Null)
        }
        FieldKind::Float => {
            row.try_get::<Option<f64>, _>(name).ok().flatten().map(Value::Float).unwrap_or(Value::Null)
        }
        FieldKind::Decimal { .. } => row
            .try_get::<Option<rust_decimal::Decimal>, _>(name)
            .ok()
            .flatten()
            .map(Value::Decimal)
            .unwrap_or(Value::Null),
        FieldKind::Bool => {
            row.try_get::<Option<bool>, _>(name).ok().flatten().map(Value::Bool).unwrap_or(Value::Null)
        }
        // Date/Datetime are selected as ::text → read as a String.
        FieldKind::Date | FieldKind::Datetime => {
            row.try_get::<Option<String>, _>(name).ok().flatten().map(Value::Str).unwrap_or(Value::Null)
        }
        // Many2many is selected as an int array (array_agg of target ids) → a list of Ints.
        FieldKind::Many2many { .. } => Value::List(
            row.try_get::<Option<Vec<i64>>, _>(name)
                .ok()
                .flatten()
                .unwrap_or_default()
                .into_iter()
                .map(Value::Int)
                .collect(),
        ),
        FieldKind::One2many { .. } => Value::Null, // not selected; never reached
    }
}

/// Converts a database row into a typed `Value` map keyed by field name (for the compute engine).
fn record_to_values(model: &ResolvedModel, row: &PgRow) -> BTreeMap<String, Value> {
    let mut m = BTreeMap::new();
    for f in &model.fields {
        // Related fields have no column but ARE selected (as a subquery alias) — read them too.
        if !f.has_column() && related_path(model.name, f.name).is_none() && !matches!(f.kind, FieldKind::Many2many { .. }) {
            continue;
        }
        m.insert(f.name.to_string(), decode_value(row, f.name, &f.kind));
    }
    // Delegated (_inherits) fields are selected as subqueries — decode them by the parent's kind.
    for d in delegated_fields(model.name).unwrap_or_default() {
        m.insert(d.def.name.to_string(), decode_value(row, d.def.name, &d.def.kind));
    }
    m
}

/// Converts a database row into a JSON object keyed by field name, decoding each column per its
/// field kind (NULL → JSON null).
/// Decodes one selected column (`name`) into JSON per its kind. NULL → `Json::Null`; exact Decimal →
/// a JSON string (preserves precision). Shared by own-field and delegated-field projection.
fn decode_json(row: &PgRow, name: &str, kind: &FieldKind) -> Result<Json, DbError> {
    Ok(match kind {
        FieldKind::Text | FieldKind::Html | FieldKind::Selection(_) => {
            row.try_get::<Option<String>, _>(name)?.map(Json::from).unwrap_or(Json::Null)
        }
        FieldKind::Integer | FieldKind::Many2one { .. } | FieldKind::Image => {
            row.try_get::<Option<i64>, _>(name)?.map(Json::from).unwrap_or(Json::Null)
        }
        FieldKind::Float => row.try_get::<Option<f64>, _>(name)?.map(Json::from).unwrap_or(Json::Null),
        FieldKind::Decimal { .. } => match row.try_get::<Option<rust_decimal::Decimal>, _>(name)? {
            Some(d) => Json::from(d.to_string()),
            None => Json::Null,
        },
        FieldKind::Bool => row.try_get::<Option<bool>, _>(name)?.map(Json::from).unwrap_or(Json::Null),
        FieldKind::Date | FieldKind::Datetime => {
            row.try_get::<Option<String>, _>(name)?.map(Json::from).unwrap_or(Json::Null)
        }
        FieldKind::Many2many { .. } => {
            let ids: Vec<i64> = row.try_get::<Option<Vec<i64>>, _>(name)?.unwrap_or_default();
            Json::Array(ids.into_iter().map(Json::from).collect())
        }
        FieldKind::One2many { .. } => Json::Null, // not selected; never reached
    })
}

fn row_to_json(model: &ResolvedModel, row: &PgRow) -> Result<Json, DbError> {
    let mut obj = Map::new();
    let id: i64 = row.try_get("id")?;
    obj.insert("id".to_string(), Json::from(id));
    for f in &model.fields {
        // Related fields have no column but ARE selected (as a subquery alias) — project them too.
        if !f.has_column() && related_path(model.name, f.name).is_none() && !matches!(f.kind, FieldKind::Many2many { .. }) {
            continue;
        }
        obj.insert(f.name.to_string(), decode_json(row, f.name, &f.kind)?);
    }
    // Delegated (_inherits) fields are selected as subqueries — project them by the parent's kind.
    for d in delegated_fields(model.name).unwrap_or_default() {
        obj.insert(d.def.name.to_string(), decode_json(row, d.def.name, &d.def.kind)?);
    }
    // On-read (non-stored) computed fields: evaluate the registered fn over this row's decoded values
    // (own + related + delegated) and inject the result. Output-only — they have no column.
    if has_read_computes(model) {
        let values = record_to_values(model, row);
        for (name, val) in compute_on_read(model, &values) {
            obj.insert(name.to_string(), value_to_json(&val));
        }
    }
    Ok(Json::Object(obj))
}

/// Builds a row's JSON and removes the fields the caller may not READ (D6 field-level security).
fn project_row(model: &ResolvedModel, ctx: &Ctx, row: &PgRow) -> Result<Json, DbError> {
    let mut j = row_to_json(model, row)?;
    if let Json::Object(ref mut o) = j {
        strip_unreadable(model, ctx, o);
    }
    Ok(j)
}

/// Removes from `obj` the fields whose `#[field(groups = ...)]` the caller is not a member of. The
/// `id` key (and any field with no restriction) is always kept; superuser keeps everything.
fn strip_unreadable(model: &ResolvedModel, ctx: &Ctx, obj: &mut Map<String, Json>) {
    if ctx.is_su() {
        return;
    }
    obj.retain(|k, _| field_accessible(model.name, k, ctx));
}

/// Whether the caller may read every field a (possibly dotted) filter path traverses (D6). Walks the
/// relation chain: each segment must be field-accessible on the model it sits on, and a non-final
/// segment must be a relation whose target model is resolved before checking the next segment. So
/// `partner_id.secret` is rejected when `secret` is restricted on the partner model, blocking probing
/// of a restricted field through a relation. Superuser is always allowed.
fn filter_path_accessible(model: &ResolvedModel, path: &str, ctx: &Ctx) -> bool {
    if ctx.is_su() {
        return true;
    }
    let mut cur: ResolvedModel = model.clone();
    let mut segs = path.split('.').peekable();
    while let Some(seg) = segs.next() {
        if !field_accessible(cur.name, seg, ctx) {
            return false;
        }
        if segs.peek().is_none() {
            break; // last segment — nothing more to traverse
        }
        // Non-final segment must be a relation; resolve its target to keep walking.
        match cur.fields.iter().find(|f| f.name == seg).map(|f| &f.kind) {
            Some(FieldKind::Many2one { target }) | Some(FieldKind::One2many { target, .. }) => {
                match resolve_registered(target) {
                    Ok(m) => cur = m,
                    // Unresolvable target → the domain compile will reject it anyway; nothing to probe.
                    Err(_) => return true,
                }
            }
            // Dotted into a non-relation (invalid path) → let the domain compiler surface the error.
            _ => return true,
        }
    }
    true
}

/// Rejects a write whose payload touches a field the caller may not WRITE (D6). One2many keys are
/// not restricted here — their child writes are checked recursively when the nested commands apply.
fn check_writable_fields(
    model: &ResolvedModel,
    ctx: &Ctx,
    payload: &Map<String, Json>,
) -> Result<(), DbError> {
    if ctx.is_su() {
        return Ok(());
    }
    for k in payload.keys() {
        if !field_accessible(model.name, k, ctx) {
            return Err(DbError::AccessDenied {
                model: model.name.to_string(),
                operation: "write (restricted field)",
            });
        }
    }
    Ok(())
}

/// Binds the compiled domain's parameters in order. Owned binds (clone/copy) so the bound
/// query never borrows the params slice.
fn bind_all<'q>(
    mut q: QueryScalar<'q, Postgres, i64, PgArguments>,
    params: &[Value],
) -> QueryScalar<'q, Postgres, i64, PgArguments> {
    for p in params {
        q = match p {
            Value::Str(s) => q.bind(s.clone()),
            Value::Int(n) => q.bind(*n),
            Value::Float(f) => q.bind(*f),
            Value::Decimal(d) => q.bind(*d),
            Value::Bool(b) => q.bind(*b),
            Value::Null => q.bind(Option::<String>::None),
            // Lists are pre-expanded into scalar params by the compiler; this is unreachable.
            Value::List(_) => q,
        };
    }
    q
}

#[cfg(test)]
mod tests {
    use super::*;
    use meshble_core::{resolve, ModelDescriptor};
    use serde_json::json;

    static M: ModelDescriptor = ModelDescriptor {
        name: "w",
        table: "w",
        fields: &[
            FieldDef {
                name: "name", label: "Name", kind: FieldKind::Text,
                required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
            FieldDef {
                name: "note", label: "Note", kind: FieldKind::Text,
                required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
            FieldDef {
                name: "total", label: "Total", kind: FieldKind::Decimal { currency_field: None },
                required: false, stored: true, compute: Some("c"), depends: &[], default: None, unique: false, check: None },
            FieldDef {
                name: "ref_id", label: "Ref", kind: FieldKind::Many2one { target: "w" },
                required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        ],
    };

    fn obj(v: serde_json::Value) -> Map<String, Json> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn rejects_null_on_required_field() {
        let m = resolve(&M, &[]).unwrap();
        assert!(matches!(
            validate_write_values(&m, &obj(json!({ "name": null })), false),
            Err(DbError::BadInput(_))
        ));
    }

    #[test]
    fn rejects_missing_required_on_create() {
        let m = resolve(&M, &[]).unwrap();
        assert!(matches!(
            validate_write_values(&m, &obj(json!({ "note": "x" })), true),
            Err(DbError::BadInput(_))
        ));
    }

    #[test]
    fn allows_partial_update_of_optional_field() {
        let m = resolve(&M, &[]).unwrap();
        assert!(validate_write_values(&m, &obj(json!({ "note": "x" })), false).is_ok());
    }

    #[test]
    fn rejects_unknown_and_computed_fields() {
        let m = resolve(&M, &[]).unwrap();
        assert!(matches!(
            validate_write_values(&m, &obj(json!({ "nope": 1 })), false),
            Err(DbError::BadInput(_))
        ));
        assert!(matches!(
            validate_write_values(&m, &obj(json!({ "total": 1.0 })), false),
            Err(DbError::BadInput(_))
        ));
    }

    #[test]
    fn accepts_valid_create_payload() {
        let m = resolve(&M, &[]).unwrap();
        assert!(validate_write_values(&m, &obj(json!({ "name": "x", "note": "y" })), true).is_ok());
    }

    #[test]
    fn coerces_numeric_string_to_int_for_relation() {
        // HTML number inputs / some clients serialize ids as strings; the boundary coerces "7" → 7.
        let m = resolve(&M, &[]).unwrap();
        let out = validate_write_values(&m, &obj(json!({ "ref_id": "7" })), false).unwrap();
        assert_eq!(out, vec![("ref_id", Value::Int(7))]);
        // A non-numeric string is still rejected (clean BadInput, not a Postgres type error).
        assert!(matches!(
            validate_write_values(&m, &obj(json!({ "ref_id": "abc" })), false),
            Err(DbError::BadInput(_))
        ));
    }
}
