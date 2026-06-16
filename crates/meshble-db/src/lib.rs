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
    check_access, compute_stored, computed_fields, record_rule_domain, Acl, Ctx, Domain, DomainError,
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
        let cols = validate_write_values(model, values, true)?;
        if cols.is_empty() {
            return Err(DbError::BadInput("no values provided".to_string()));
        }
        // Run the compute engine: stored computed fields are derived from the record and inserted.
        let mut record: BTreeMap<String, Value> =
            cols.into_iter().map(|(c, v)| (c.to_string(), v)).collect();
        compute_stored(model, &mut record);

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
        tx.commit().await?;
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
            compute_stored(model, &mut record);
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
        Ok(q.execute(&self.pool).await?.rows_affected())
    }

    /// Reads a row's stored field values into a typed map (for recompute on update).
    async fn read_record(
        &self,
        model: &ResolvedModel,
        id: i64,
    ) -> Result<Option<BTreeMap<String, Value>>, DbError> {
        let sql = format!("SELECT {} FROM {} WHERE id = $1", select_columns(model), model.table);
        let row = sqlx::query(&sql).bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| record_to_values(model, &r)))
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
        Ok(q.execute(&self.pool).await?.rows_affected())
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

/// Builds the SELECT column list for a model. NUMERIC columns are cast to float8 so they decode
/// into `f64` without a decimal dependency. Identifiers come from the model, never user input.
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
