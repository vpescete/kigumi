//! Cross-record service seam — the framework primitive that lets a MODULE own a multi-record, async
//! operation on any model, registered with one `register_service!` line, dispatched by a single generic
//! route. It is the transactional twin of the (pure, same-record) action seam in meshble-core: where an
//! action returns a value diff, a service runs arbitrary secured reads/writes and returns free-form JSON
//! (a created id, a count, a report).
//!
//! Why this exists: the ERP engines (invoicing, payments, posting, tax application, stock reservation)
//! need exactly this shape. Without it they were written INTO this crate; with it they move OUT into the
//! ERP modules, and meshble-db keeps only the generic dispatcher — so the ERP becomes an optional layer.
//!
//! Security boundary: [`Db::run_service`] runs the IDENTICAL gate to `run_action` (ACL + group + record
//! rule + company visibility) BEFORE the body runs; only past the gate is the body entered. The body
//! reaches the DB solely through [`ServiceCtx`], whose secured-CRUD helpers re-apply the full security
//! path (ACL + D6 + record rule + company scope) for every call, under the caller's context.
//!
//! Scope (this file): the secured-CRUD surface every relocated service needs — `find_one_secured`,
//! `find_secured`, `insert_secured`, `update_secured` — each delegating to `Db`'s own pool methods, so a
//! relocated method behaves byte-for-byte as before (own-transaction-per-write + full recompute/tracking),
//! plus the chart/sequence/CAS helpers (`first_match`, `next_value`, `ensure_sequence`, `guarded_cas`) and
//! the raw-SQL escape (`pool`). The relocated ERP methods opened no transaction, so this surface is
//! sufficient; single-transaction atomicity (a live `tx()` handle + in-tx CRUD twins) is added only when
//! the stock batch, which uses FOR UPDATE, is migrated.

use crate::{Db, DbError};
use meshble_core::{check_access, Acl, Ctx, Domain, Operation, RecordRule, ResolvedModel};
use serde_json::{Map, Value as Json};
use std::future::Future;
use std::pin::Pin;

/// A boxed Send future borrowing the `&mut ServiceCtx` for lifetime `'c` — what lets a service body
/// `.await` secured-CRUD calls while holding the context borrow.
pub type BoxServiceFut<'c, T> = Pin<Box<dyn Future<Output = T> + Send + 'c>>;

/// The route `:id` plus the POST JSON body (e.g. a payment `{amount, journal_id}`).
pub struct ServiceInput {
    pub record_id: i64,
    pub body: Map<String, Json>,
}

impl ServiceInput {
    /// A body field as i64 (0 when absent/not a number).
    pub fn int(&self, key: &str) -> i64 {
        self.body.get(key).and_then(|v| v.as_i64()).unwrap_or(0)
    }
    /// A body field as &str (empty when absent).
    pub fn str(&self, key: &str) -> &str {
        self.body.get(key).and_then(|v| v.as_str()).unwrap_or("")
    }
}

/// What a service returns: free-form JSON the handler relays verbatim (services produce a result — an id,
/// a count, a report — not a same-record diff like an `ActionOutcome`).
pub struct ServiceOutput(pub Json);

impl ServiceOutput {
    pub fn json(v: Json) -> Self {
        ServiceOutput(v)
    }
}

/// A registered service fn. A HRTB plain `fn` pointer (so it is storable in `inventory`, like `ActionFn`)
/// returning a boxed future whose lifetime is tied to the `&mut ServiceCtx` borrow. Module authors write a
/// bare `async fn(&mut ServiceCtx, ServiceInput) -> Result<ServiceOutput, DbError>` and `register_service!`
/// wraps it with the one `Box::pin`.
pub type ServiceFn =
    for<'c, 'a> fn(&'c mut ServiceCtx<'a>, ServiceInput) -> BoxServiceFut<'c, Result<ServiceOutput, DbError>>;

/// Registration of a service by (model, name) — the transactional twin of `ActionRegistration`.
/// `write_gate` selects the ACL operation the dispatcher checks (Write for a mutating service, Read for a
/// read-only one such as a report). `groups` (if non-empty) restricts who may run it, on top of the ACL.
pub struct ServiceRegistration {
    pub model: &'static str,
    pub name: &'static str,
    pub func: ServiceFn,
    pub write_gate: bool,
    pub groups: &'static [&'static str],
}
meshble_core::inventory::collect!(ServiceRegistration);

/// Looks up a registered service by model + name.
pub fn service_for(model: &str, name: &str) -> Option<&'static ServiceRegistration> {
    meshble_core::inventory::iter::<ServiceRegistration>
        .into_iter()
        .find(|s| s.model == model && s.name == name)
}

/// All services registered on `model` (for the UI contract, so a form can render its service buttons).
pub fn services_for(model: &str) -> Vec<&'static ServiceRegistration> {
    meshble_core::inventory::iter::<ServiceRegistration>
        .into_iter()
        .filter(|s| s.model == model)
        .collect()
}

/// A read-only REPORT fn — bespoke aggregate SQL over the pool returning JSON rows, with optional query
/// params. The read-only, record-less sibling of a service (a report has no record id): gated only on Read
/// of a declared model, then dispatched generically. The ERP report logic lives in the module that owns
/// the tables; the core just gates + dispatches.
pub type LedgerReportFn =
    for<'a> fn(&'a sqlx::PgPool, Map<String, Json>) -> BoxServiceFut<'a, Result<Vec<Json>, DbError>>;

/// Registration of a report by name, emitted by `register_report!`. `read_model` is the model whose Read
/// ACL gates it (e.g. account.account); `groups` (if non-empty) further restricts who may run it.
pub struct LedgerReportRegistration {
    pub name: &'static str,
    pub read_model: &'static str,
    pub func: LedgerReportFn,
    pub groups: &'static [&'static str],
}
meshble_core::inventory::collect!(LedgerReportRegistration);

/// Looks up a registered report by name.
pub fn ledger_report_for(name: &str) -> Option<&'static LedgerReportRegistration> {
    meshble_core::inventory::iter::<LedgerReportRegistration>.into_iter().find(|r| r.name == name)
}

/// All registered report names (for a UI report menu).
pub fn ledger_report_names() -> Vec<&'static str> {
    meshble_core::inventory::iter::<LedgerReportRegistration>.into_iter().map(|r| r.name).collect()
}

/// The secured-primitive surface handed to a service body. A concrete struct (no trait, no `dyn`): its
/// methods are `async fn`s delegating to `Db`'s secured CRUD under the caller's context, so the security
/// engine is re-applied on every call. The ERP model-name literals a body resolves live in the MODULE,
/// never in this crate.
pub struct ServiceCtx<'a> {
    db: &'a Db,
    caller: Ctx,
    acls: &'a [Acl],
    rules: &'a [RecordRule],
}

impl<'a> ServiceCtx<'a> {
    /// The authenticated caller (the dispatcher has already gated it).
    pub fn caller(&self) -> &Ctx {
        &self.caller
    }
    /// Explicit, greppable elevation past the gate for engine-owned rows (GL lines, join rows, sequences).
    pub fn elevated(&self) -> Ctx {
        self.caller.sudo()
    }
    /// Resolve a model the service owns — the ERP model-name literal lives in the MODULE body, never here.
    pub fn resolve(&self, model: &str) -> Result<ResolvedModel, DbError> {
        meshble_core::resolve_registered(model).map_err(DbError::BadInput)
    }
    /// ACL check for the caller on a model — so a service can gate on a SECONDARY model (e.g. a discount
    /// wizard service gating on Write of the underlying `sale.order`, not just the wizard the route named).
    pub fn check_access(&self, op: Operation, model: &str) -> bool {
        check_access(op, model, &self.caller, self.acls)
    }
    /// The instance clock (current date as YYYY-MM-DD) — a framework concern, used by pricing/tax/FX.
    pub async fn today(&self) -> Result<String, DbError> {
        self.db.today().await
    }
    /// Gapless sequence: the next formatted value for `code` (advances the counter atomically). Framework
    /// numbering an ERP service uses to number an invoice / journal entry.
    pub async fn next_value(&self, code: &str) -> Result<String, DbError> {
        self.db.next_value(code).await
    }
    /// Registers a sequence `code` with its formatting if absent (idempotent).
    pub async fn ensure_sequence(&self, code: &str, prefix: &str, suffix: &str, padding: i32) -> Result<(), DbError> {
        self.db.ensure_sequence(code, prefix, suffix, padding).await
    }
    /// The first ACTIVE id of `model` whose `field` == `value` and whose company matches (company_id = c,
    /// or IS NULL when none) — the generic chart / journal / location reference resolution. Runs ELEVATED
    /// (engine-owned config lookup), so a service past the gate can resolve config rows the caller may not
    /// directly read, while the company pin keeps the lookup deterministic (never another company's rows).
    pub async fn first_match(&self, model: &ResolvedModel, field: &str, value: &str, company: Option<i64>) -> Result<Option<i64>, DbError> {
        self.db.first_match(model, &self.caller.sudo(), field, value, company).await
    }
    /// A guarded compare-and-set under the CALLER's row-level authorization (Write record rule + company):
    /// `UPDATE model SET set_clause WHERE id AND extra_where`. Static fragments only (no injection surface).
    /// Returns true iff this call won the transition — e.g. atomically claiming an order for invoicing.
    pub async fn guarded_cas(&self, model: &ResolvedModel, id: i64, set_clause: &str, extra_where: &str) -> Result<bool, DbError> {
        self.db.guarded_cas(model, &self.caller, self.rules, id, set_clause, extra_where).await
    }
    /// The connection pool, for a module's own bespoke SQL the domain-based secured finds cannot express:
    /// reference reads (currency, fiscal positions, recursive category trees), junction reads, most-
    /// specific-rule queries, and engine-owned BULK operations (e.g. replacing a line's tax breakdown rows
    /// with a single DELETE). Past the gate the body is trusted first-party code, exactly as the ERP
    /// methods were before relocation. Per-RECORD user-data writes should still go through the secured
    /// helpers so ACL/record-rule/company scope are re-applied; the raw pool is for engine-owned rows.
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.db.pool
    }

    // ── secured reads (visibility-gated) ──
    pub async fn find_one_secured(&self, m: &ResolvedModel, ctx: &Ctx, id: i64) -> Result<Option<Json>, DbError> {
        self.db.find_one_secured(m, ctx, self.acls, self.rules, id).await
    }
    pub async fn find_secured(&self, m: &ResolvedModel, ctx: &Ctx, filter: Option<&Domain>) -> Result<Vec<Json>, DbError> {
        self.db.find_secured(m, ctx, self.acls, self.rules, filter).await
    }

    // ── secured writes (own transaction per call, full recompute/tracking — behavior-identical to the
    //    pre-relocation ERP methods, which already wrote one secured row at a time) ──
    pub async fn insert_secured(&self, m: &ResolvedModel, ctx: &Ctx, values: &Map<String, Json>) -> Result<i64, DbError> {
        self.db.insert_secured(m, ctx, self.acls, self.rules, values).await
    }
    pub async fn update_secured(&self, m: &ResolvedModel, ctx: &Ctx, id: i64, values: &Map<String, Json>) -> Result<u64, DbError> {
        self.db.update_secured(m, ctx, self.acls, self.rules, id, values).await
    }
}

impl Db {
    /// The generic service dispatcher — the `run_action` twin. ZERO ERP literals. Gates exactly like
    /// `run_action` (ACL + group + record-rule/company visibility), then runs the registered body and
    /// returns its JSON result. v1 services manage their own per-write transactions via [`ServiceCtx`];
    /// the single-transaction variant arrives with the account/stock batch.
    pub async fn run_service(
        &self,
        model: &ResolvedModel,
        ctx: &Ctx,
        acls: &[Acl],
        rules: &[RecordRule],
        id: i64,
        name: &str,
        body: Map<String, Json>,
    ) -> Result<Json, DbError> {
        let reg = service_for(model.name, name)
            .ok_or_else(|| DbError::BadInput(format!("unknown service '{name}' on '{}'", model.name)))?;
        // (1) ACL — Write for a mutating service, Read for a read-only one.
        let op = if reg.write_gate { Operation::Write } else { Operation::Read };
        if !check_access(op, model.name, ctx, acls) {
            return Err(DbError::AccessDenied { model: model.name.to_string(), operation: "service" });
        }
        // (2) group gate — identical to run_action.
        if !reg.groups.is_empty() && !ctx.is_su() && !reg.groups.iter().any(|g| ctx.is_member(g)) {
            return Err(DbError::AccessDenied { model: model.name.to_string(), operation: "service (group)" });
        }
        // (3) record-rule + company visibility — the target row must be visible to the caller.
        if self.find_one_secured(model, ctx, acls, rules, id).await?.is_none() {
            return Err(DbError::BadInput("record not found or not permitted".to_string()));
        }
        let mut cx = ServiceCtx { db: self, caller: ctx.clone(), acls, rules };
        let out = (reg.func)(&mut cx, ServiceInput { record_id: id, body }).await?;
        Ok(out.0)
    }

    /// The generic READ-ONLY report dispatcher. Gates on Read of the report's declared model (+ optional
    /// group), then runs the module-registered report fn over the pool with the query params. ZERO ERP
    /// literals — a new report needs no edit here. Returns the JSON rows.
    pub async fn run_ledger_report(
        &self,
        ctx: &Ctx,
        acls: &[Acl],
        name: &str,
        params: Map<String, Json>,
    ) -> Result<Vec<Json>, DbError> {
        let reg = ledger_report_for(name).ok_or_else(|| DbError::BadInput(format!("unknown report '{name}'")))?;
        if !check_access(Operation::Read, reg.read_model, ctx, acls) {
            return Err(DbError::AccessDenied { model: reg.read_model.to_string(), operation: "report" });
        }
        if !reg.groups.is_empty() && !ctx.is_su() && !reg.groups.iter().any(|g| ctx.is_member(g)) {
            return Err(DbError::AccessDenied { model: reg.read_model.to_string(), operation: "report (group)" });
        }
        (reg.func)(&self.pool, params).await
    }
}
