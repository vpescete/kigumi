//! Postgres persistence layer.
//!
//! Closes the loop: the metamodel's generated DDL creates real tables, and a [`Domain`] is
//! compiled to a PARAMETERIZED `WHERE` whose values are BOUND (never interpolated) before
//! execution. The `*_secured` methods enforce the security engine (ACL + record rules) at the
//! database boundary: access is checked, and the user's record-rule domain is AND-ed into the
//! query — so a user can never read rows the rules forbid.

mod auth_store;
mod migration;
pub use auth_store::UserRow;
pub use migration::{Migration, MigrationOutcome};

use meshble_core::{
    check_access, compute_stored, computed_fields, record_rule_domain, resolve_all_registered,
    resolve_registered, Acl, Children, Ctx, Domain, DomainError, FieldDef, FieldKind, Operation,
    RecordRule, ResolvedModel, Value,
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
}

impl From<sqlx::Error> for DbError {
    fn from(e: sqlx::Error) -> Self {
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

impl Db {
    /// Connects to `url` (e.g. `postgres://user@host/db`).
    pub async fn connect(url: &str) -> Result<Db, DbError> {
        let pool = PgPoolOptions::new().max_connections(5).connect(url).await?;
        Ok(Db { pool })
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
                Value::Bool(b) => q.bind(*b),
                Value::Null => q.bind(Option::<String>::None),
                Value::List(_) => q,
            };
        }
        let rows = q.fetch_all(&self.pool).await?;
        rows.iter().map(|r| row_to_json(model, r)).collect()
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
        let where_sql = match record_rule_domain(Operation::Read, model.name, ctx, rules) {
            Some(rule) => format!("id = $1 AND {}", rule.compile_into(model, &mut params)?),
            None => "id = $1".to_string(),
        };
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
        // Inline each One2many field as an array of the caller's visible child rows.
        for f in &model.fields {
            if let FieldKind::One2many { target, inverse } = f.kind {
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
        let rule = record_rule_domain(Operation::Read, model.name, ctx, rules);
        Ok(match (filter, rule) {
            (Some(f), Some(r)) => f.clone().and(r),
            (Some(f), None) => f.clone(),
            (None, Some(r)) => r,
            (None, None) => Domain::True,
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
        // Split the payload: scalar columns vs One2many child-create payloads (nested writes).
        let (scalars, nested) = split_nested(model, values)?;
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

        // Nested One2many children: create each in the SAME transaction with its inverse FK pointed
        // at the new parent, so the parent+children are all-or-nothing. ACL Create is enforced on
        // the child model too; a denial drops the tx and rolls back the parent.
        for nc in &nested {
            if !check_access(Operation::Create, nc.child.name, ctx, acls) {
                return Err(DbError::AccessDenied {
                    model: nc.child.name.to_string(),
                    operation: "create",
                });
            }
            for row in &nc.rows {
                let mut cvals = row.clone();
                cvals.insert(nc.inverse.to_string(), Json::from(id)); // parent owns the FK
                let ccols = validate_write_values(&nc.child, &cvals, true)?;
                let mut crec: BTreeMap<String, Value> =
                    ccols.into_iter().map(|(c, v)| (c.to_string(), v)).collect();
                compute_stored(&nc.child, &mut crec, &Children::new());
                let (cn, cv): (Vec<&str>, Vec<Value>) =
                    crec.iter().map(|(k, v)| (k.as_str(), v.clone())).unzip();
                let cph: Vec<String> = (1..=cn.len()).map(|i| format!("${i}")).collect();
                let csql = format!(
                    "INSERT INTO {} ({}) VALUES ({}) RETURNING id",
                    nc.child.table,
                    cn.join(", "),
                    cph.join(", ")
                );
                let mut cq = sqlx::query_scalar::<Postgres, i64>(&csql);
                cq = bind_all(cq, &cv);
                let child_id: i64 = cq.fetch_one(&mut *tx).await?;

                // The child's own Create record rule must hold too — otherwise nesting would be a
                // weaker path than the child's own endpoint (record-rule bypass). Violation rolls
                // back the whole parent+children tx.
                if let Some(rule) = record_rule_domain(Operation::Create, nc.child.name, ctx, rules) {
                    let mut params: Vec<Value> = vec![Value::Int(child_id)];
                    let where_sql = rule.compile_into(&nc.child, &mut params)?;
                    let check =
                        format!("SELECT 1 FROM {} WHERE id = $1 AND {}", nc.child.table, where_sql);
                    let mut chk = sqlx::query(&check);
                    for v in &params {
                        chk = bind_query(chk, v);
                    }
                    if chk.fetch_optional(&mut *tx).await?.is_none() {
                        return Err(DbError::AccessDenied {
                            model: nc.child.name.to_string(),
                            operation: "create (record rule)",
                        });
                    }
                }
            }
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
        let cols = validate_write_values(model, values, false)?;
        if cols.is_empty() {
            return Err(DbError::BadInput("no values provided".to_string()));
        }
        // Recompute stored computed fields from the merged record (current row + this update).
        let computed = computed_fields(model);
        let mut set_pairs: Vec<(String, Value)> =
            cols.iter().map(|(c, v)| (c.to_string(), v.clone())).collect();
        if !computed.is_empty() {
            let mut record = match self.read_record(model, id).await? {
                Some(r) => r,
                None => return Ok(0), // no such row
            };
            for (c, v) in &cols {
                record.insert(c.to_string(), v.clone());
            }
            let children = self.read_children(model, id).await?;
            compute_stored(model, &mut record, &children);
            for name in &computed {
                if let Some(v) = record.get(*name) {
                    set_pairs.push((name.to_string(), v.clone()));
                }
            }
        }

        let set: Vec<String> =
            set_pairs.iter().enumerate().map(|(i, (c, _))| format!("{} = ${}", c, i + 1)).collect();
        let id_ph = set_pairs.len() + 1;
        let mut params: Vec<Value> = set_pairs.iter().map(|(_, v)| v.clone()).collect();
        params.push(Value::Int(id));
        let where_sql = match record_rule_domain(Operation::Write, model.name, ctx, rules) {
            Some(rule) => format!("id = ${id_ph} AND {}", rule.compile_into(model, &mut params)?),
            None => format!("id = ${id_ph}"),
        };
        let sql = format!("UPDATE {} SET {} WHERE {}", model.table, set.join(", "), where_sql);
        let mut q = sqlx::query(&sql);
        for v in &params {
            q = bind_query(q, v);
        }
        // Capture the parents the child pointed to BEFORE the update, run it, then capture the new
        // parents — so re-parenting a child recomputes BOTH the old and the new aggregate.
        let before = self.parent_targets(model, id).await?;
        let affected = q.execute(&self.pool).await?.rows_affected();
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

    /// Reads a row's stored field values into a typed map (for recompute on update).
    async fn read_record(
        &self,
        model: &ResolvedModel,
        id: i64,
    ) -> Result<Option<BTreeMap<String, Value>>, DbError> {
        let mut c = self.pool.acquire().await?;
        read_record_on(&mut c, model, id).await
    }

    /// Loads the One2many children of `parent_id` (one entry per o2m field) for aggregate compute.
    async fn read_children(&self, parent: &ResolvedModel, parent_id: i64) -> Result<Children, DbError> {
        let mut c = self.pool.acquire().await?;
        read_children_on(&mut c, parent, parent_id).await
    }

    /// Recomputes `parent`'s aggregate computed columns from its current children (a direct UPDATE,
    /// so it never re-enters the secured write path / re-triggers). Serialized per parent with an
    /// advisory lock so concurrent child writes can't lose-update the aggregate. All reads and the
    /// write run on the SAME locked connection, so holding the lock never contends for a second
    /// pool connection.
    async fn recompute_parent(&self, parent: &ResolvedModel, parent_id: i64) -> Result<(), DbError> {
        if computed_fields(parent).is_empty() {
            return Ok(());
        }
        // Hold a per-parent advisory lock across read+recompute+write: a concurrent recompute for
        // the same parent blocks here until we commit, then re-reads the full child set.
        let mut lock = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
            .bind(format!("agg:{}:{}", parent.table, parent_id))
            .execute(&mut *lock)
            .await?;
        let outcome = recompute_columns_on(&mut lock, parent, parent_id).await;
        lock.commit().await?; // release the lock
        outcome
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
        let where_sql = match record_rule_domain(Operation::Delete, model.name, ctx, rules) {
            Some(rule) => format!("id = $1 AND {}", rule.compile_into(model, &mut params)?),
            None => "id = $1".to_string(),
        };
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

/// A One2many field's child create-payloads, extracted from a write `values` map.
struct NestedCreate {
    child: ResolvedModel,
    inverse: &'static str,
    rows: Vec<Map<String, Json>>,
}

/// Splits a write payload into scalar columns and One2many child-create payloads. A One2many value
/// must be an array of objects; each object is a NEW child to create. Only the create form is
/// supported through a parent write — an item carrying an `id` is rejected (linking/updating
/// existing children goes through the child's own CRUD endpoints).
fn split_nested(
    model: &ResolvedModel,
    values: &Map<String, Json>,
) -> Result<(Map<String, Json>, Vec<NestedCreate>), DbError> {
    let mut scalars = Map::new();
    let mut nested = Vec::new();
    for (key, jv) in values {
        match model.fields.iter().find(|f| f.name == *key).map(|f| f.kind) {
            Some(FieldKind::One2many { target, inverse }) => {
                let arr = jv.as_array().ok_or_else(|| {
                    DbError::BadInput(format!("'{key}' must be an array of child records"))
                })?;
                let mut rows = Vec::with_capacity(arr.len());
                for item in arr {
                    let obj = item.as_object().ok_or_else(|| {
                        DbError::BadInput(format!("each '{key}' item must be an object"))
                    })?;
                    if obj.contains_key("id") {
                        return Err(DbError::BadInput(format!(
                            "'{key}': nested writes can only create children (item with 'id' not allowed)"
                        )));
                    }
                    rows.push(obj.clone());
                }
                if !rows.is_empty() {
                    let child = resolve_registered(target).map_err(|e| {
                        DbError::BadInput(format!("unknown child model '{target}': {e}"))
                    })?;
                    nested.push(NestedCreate { child, inverse, rows });
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
        (FieldKind::Decimal { .. }, Json::Number(n)) => {
            Value::Float(n.as_f64().filter(|x| x.is_finite()).ok_or_else(bad)?)
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
        if !f.has_column() {
            continue;
        }
        if matches!(f.kind, FieldKind::Decimal { .. }) {
            cols.push(format!("{}::float8 AS {}", f.name, f.name));
        } else {
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
            FieldKind::Decimal { .. } => {
                row.try_get::<Option<f64>, _>(f.name).ok().flatten().map(Value::Float).unwrap_or(Value::Null)
            }
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
            FieldKind::Decimal { .. } => match row.try_get::<Option<f64>, _>(f.name)? {
                Some(x) => serde_json::json!(x),
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
                required: true, stored: true, compute: None, depends: &[],
            },
            FieldDef {
                name: "note", label: "Note", kind: FieldKind::Text,
                required: false, stored: true, compute: None, depends: &[],
            },
            FieldDef {
                name: "total", label: "Total", kind: FieldKind::Decimal { currency_field: None },
                required: false, stored: true, compute: Some("c"), depends: &[],
            },
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
