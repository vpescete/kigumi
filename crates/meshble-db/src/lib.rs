//! Postgres persistence layer.
//!
//! Closes the loop: the metamodel's generated DDL creates real tables, and a [`Domain`] is
//! compiled to a PARAMETERIZED `WHERE` whose values are BOUND (never interpolated) before
//! execution. The `*_secured` methods enforce the security engine (ACL + record rules) at the
//! database boundary: access is checked, and the user's record-rule domain is AND-ed into the
//! query — so a user can never read rows the rules forbid.

mod access_store;
mod auth_store;
mod migration;
mod sequence;
mod settings;
pub use access_store::{AclRow, RuleRow};
pub use auth_store::UserRow;
pub use migration::{Migration, MigrationOutcome};

use meshble_core::{
    action_for, check_access, compute_stored, computed_fields, field_accessible, record_rule_domain,
    resolve_all_registered, resolve_registered, Acl, ActionInput, Children, Ctx, Domain, DomainError,
    FieldDef, FieldKind, Operation, RecordRule, ResolvedModel, Value,
};
use meshble_schema::to_ddl;
use serde_json::{Map, Value as Json};
use sqlx::postgres::{PgArguments, PgPoolOptions, PgRow};
use sqlx::query::{Query, QueryScalar};
use sqlx::{PgPool, Postgres, Row};
use std::collections::BTreeMap;

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

/// A connection pool to a Postgres database.
pub struct Db {
    pool: PgPool,
}

/// A page of list results plus the total count under the same secured domain.
pub struct ListPage {
    pub data: Vec<Json>,
    pub total: i64,
}

impl Db {
    /// Connects to `url` (e.g. `postgres://user@host/db`).
    pub async fn connect(url: &str) -> Result<Db, DbError> {
        let pool = PgPoolOptions::new().max_connections(5).connect(url).await?;
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

    /// Drops the model's table if it exists.
    pub async fn drop_table(&self, model: &ResolvedModel) -> Result<(), DbError> {
        let sql = format!("DROP TABLE IF EXISTS {}", model.table);
        sqlx::query(&sql).execute(&self.pool).await?;
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
        if !check_access(Operation::Create, model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: model.name.to_string(), operation: "create" });
        }
        check_writable_fields(model, ctx, values)?; // D6: reject fields the caller may not write
        // Split the payload: scalar columns vs One2many child-create payloads (nested writes).
        let (mut scalars, nested) = split_nested(model, values)?;
        // Multi-company: validate the supplied company_id against the caller's scope and default it
        // on create (single chokepoint, reused for nested children + update below).
        apply_company_scope(model, ctx, &mut scalars, true)?;
        apply_defaults(model, &mut scalars); // fill unset fields with their declared defaults
        let cols = validate_write_values(model, &scalars, true)?;
        if cols.is_empty() {
            return Err(DbError::BadInput("no values provided".to_string()));
        }
        // Run the compute engine: stored computed fields are derived from the record and inserted.
        // A brand-new row has no children yet, so aggregate computes start at their empty value
        // (recomputed below once the nested children are inserted).
        let mut record: BTreeMap<String, Value> =
            cols.into_iter().map(|(c, v)| (c.to_string(), v)).collect();
        compute_stored(model, &mut record, &Children::new());

        let (names, vals): (Vec<&str>, Vec<Value>) =
            record.iter().map(|(k, v)| (k.as_str(), v.clone())).unzip();
        let placeholders: Vec<String> = (1..=names.len()).map(|i| format!("${i}")).collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}) RETURNING id",
            model.table,
            names.join(", "),
            placeholders.join(", ")
        );

        let mut tx = self.pool.begin().await?;
        let mut q = sqlx::query_scalar::<Postgres, i64>(&sql);
        q = bind_all(q, &vals);
        let id: i64 = q.fetch_one(&mut *tx).await?;

        if let Some(rule) = record_rule_domain(Operation::Create, model.name, ctx, rules) {
            let mut params: Vec<Value> = vec![Value::Int(id)];
            let where_sql = rule.compile_into(model, &mut params)?;
            let check = format!("SELECT 1 FROM {} WHERE id = $1 AND {}", model.table, where_sql);
            let mut cq = sqlx::query(&check);
            for v in &params {
                cq = bind_query(cq, v);
            }
            if cq.fetch_optional(&mut *tx).await?.is_none() {
                // The created row violates the create rule → roll back by dropping the tx.
                return Err(DbError::AccessDenied {
                    model: model.name.to_string(),
                    operation: "create (record rule)",
                });
            }
        }

        // Nested One2many children: create-only on a brand-new parent, in the SAME transaction with
        // child ACL + record rules re-checked — parent + children are all-or-nothing.
        if !nested.is_empty() {
            self.apply_nested_in_tx(&mut tx, ctx, acls, rules, &nested, id, false).await?;
        }

        // Recompute this parent's own aggregate from the just-inserted children IN THIS TRANSACTION,
        // so the parent row, its children, and its aggregate commit atomically (no stale window, and
        // no "stale forever" if a post-commit recompute were to fail). The parent is brand-new — its
        // id is invisible to other transactions — so no advisory lock is needed here.
        if !nested.is_empty() {
            recompute_columns_on(&mut tx, model, id).await?;
        }
        tx.commit().await?;

        // Grandparents are a separate aggregate (single-level by design); recompute post-commit. The
        // call is idempotent (it reads current state), so a retry repairs it.
        self.recompute_parents_of(model, &record).await?;
        Ok(id)
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
        // Split scalar fields from One2many child commands (D4 typed write-through).
        let (mut scalars, nested) = split_nested(model, values)?;
        // Multi-company: a scoped caller may not reassign a row into a foreign company or NULL.
        apply_company_scope(model, ctx, &mut scalars, false)?;
        let cols = validate_write_values(model, &scalars, false)?;
        if cols.is_empty() && nested.is_empty() {
            return Err(DbError::BadInput("no values provided".to_string()));
        }

        let mut tx = self.pool.begin().await?;
        // Parents this row points to BEFORE the write (re-parenting uses before + after).
        let before = self.parent_targets(model, id).await?;

        // 1) Scalar UPDATE of the provided columns (computed columns recomputed in step 3); the Write
        //    rule + company scope are enforced in the WHERE.
        let mut affected = 1u64;
        if !cols.is_empty() {
            let set: Vec<String> =
                cols.iter().enumerate().map(|(i, (c, _))| format!("{} = ${}", c, i + 1)).collect();
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

        // 3) Recompute this row's computed columns (same-record + aggregate over its children),
        //    in-tx and serialized per row so concurrent child writes cannot lose-update the aggregate.
        if !computed_fields(model).is_empty() {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
                .bind(format!("agg:{}:{}", model.table, id))
                .execute(&mut *tx)
                .await?;
            recompute_columns_on(&mut tx, model, id).await?;
        }
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
        let cph: Vec<String> = (1..=cn.len()).map(|i| format!("${i}")).collect();
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
            set_pairs.iter().enumerate().map(|(i, (c, _))| format!("{} = ${}", c, i + 1)).collect();
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
        for (parent, pid) in parents {
            self.recompute_parent(&parent, pid).await?;
        }
        Ok(affected)
    }
}

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

/// Splits a write payload into scalar columns and One2many child commands. Each One2many value is an
/// array of typed commands `{op:'create',values}` / `{op:'update',id,values}` / `{op:'delete',id}`;
/// a bare object (no `op`/`id`) is shorthand for create.
fn split_nested(
    model: &ResolvedModel,
    values: &Map<String, Json>,
) -> Result<(Map<String, Json>, Vec<NestedWrite>), DbError> {
    let mut scalars = Map::new();
    let mut nested = Vec::new();
    for (key, jv) in values {
        match model.fields.iter().find(|f| f.name == *key).map(|f| f.kind) {
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
            // Scalar (or unknown) field: validate_write_values will accept or reject it.
            _ => {
                scalars.insert(key.clone(), jv.clone());
            }
        }
    }
    Ok((scalars, nested))
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
        FieldKind::Integer | FieldKind::Many2one { .. } => Json::from(d.parse::<i64>().ok()?),
        // Decimals travel as strings → parsed exactly by json_to_value.
        FieldKind::Decimal { .. } | FieldKind::Text | FieldKind::Selection(_) => Json::from(d.to_string()),
        FieldKind::One2many { .. } => return None,
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
        if f.has_column() && !f.is_computed() && !payload.contains_key(f.name) {
            if let Some(v) = default_json(f) {
                payload.insert(f.name.to_string(), v);
            }
        }
    }
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
        (FieldKind::Selection(opts), Json::String(s)) => {
            if !opts.iter().any(|(k, _)| k == s) {
                return Err(DbError::BadInput(format!(
                    "'{s}' is not a valid option for '{}'",
                    field.name
                )));
            }
            Value::Str(s.clone())
        }
        (FieldKind::Integer | FieldKind::Many2one { .. }, Json::Number(n)) => {
            Value::Int(n.as_i64().ok_or_else(bad)?)
        }
        // Exact decimal: parse from the number's canonical STRING (not f64) so 0.01 etc. are exact;
        // also accept a JSON string (the canonical money representation).
        (FieldKind::Decimal { .. }, Json::Number(n)) => {
            Value::Decimal(n.to_string().parse().map_err(|_| bad())?)
        }
        (FieldKind::Decimal { .. }, Json::String(s)) => {
            Value::Decimal(s.parse().map_err(|_| bad())?)
        }
        (FieldKind::Bool, Json::Bool(b)) => Value::Bool(*b),
        _ => return Err(bad()),
    })
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
        computed.iter().enumerate().map(|(i, name)| format!("{} = ${}", name, i + 1)).collect();
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
            // Decimal columns are read as exact NUMERIC (decoded into rust_decimal) — no float8 cast.
            cols.push(f.name.to_string());
        }
    }
    cols.join(", ")
}

/// Converts a database row into a typed `Value` map keyed by field name (for the compute engine).
fn record_to_values(model: &ResolvedModel, row: &PgRow) -> BTreeMap<String, Value> {
    let mut m = BTreeMap::new();
    for f in &model.fields {
        if !f.has_column() {
            continue;
        }
        let v = match &f.kind {
            FieldKind::Text | FieldKind::Selection(_) => {
                row.try_get::<Option<String>, _>(f.name).ok().flatten().map(Value::Str).unwrap_or(Value::Null)
            }
            FieldKind::Integer | FieldKind::Many2one { .. } => {
                row.try_get::<Option<i64>, _>(f.name).ok().flatten().map(Value::Int).unwrap_or(Value::Null)
            }
            FieldKind::Decimal { .. } => row
                .try_get::<Option<rust_decimal::Decimal>, _>(f.name)
                .ok()
                .flatten()
                .map(Value::Decimal)
                .unwrap_or(Value::Null),
            FieldKind::Bool => {
                row.try_get::<Option<bool>, _>(f.name).ok().flatten().map(Value::Bool).unwrap_or(Value::Null)
            }
            FieldKind::One2many { .. } => continue,
        };
        m.insert(f.name.to_string(), v);
    }
    m
}

/// Converts a database row into a JSON object keyed by field name, decoding each column per its
/// field kind (NULL → JSON null).
fn row_to_json(model: &ResolvedModel, row: &PgRow) -> Result<Json, DbError> {
    let mut obj = Map::new();
    let id: i64 = row.try_get("id")?;
    obj.insert("id".to_string(), Json::from(id));
    for f in &model.fields {
        if !f.has_column() {
            continue;
        }
        let v: Json = match &f.kind {
            FieldKind::Text | FieldKind::Selection(_) => {
                row.try_get::<Option<String>, _>(f.name)?.map(Json::from).unwrap_or(Json::Null)
            }
            FieldKind::Integer | FieldKind::Many2one { .. } => {
                row.try_get::<Option<i64>, _>(f.name)?.map(Json::from).unwrap_or(Json::Null)
            }
            // Exact decimal serialized as a JSON STRING (e.g. "1240.00") to preserve precision.
            FieldKind::Decimal { .. } => match row.try_get::<Option<rust_decimal::Decimal>, _>(f.name)? {
                Some(d) => Json::from(d.to_string()),
                None => Json::Null,
            },
            FieldKind::Bool => {
                row.try_get::<Option<bool>, _>(f.name)?.map(Json::from).unwrap_or(Json::Null)
            }
            FieldKind::One2many { .. } => continue,
        };
        obj.insert(f.name.to_string(), v);
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
}
