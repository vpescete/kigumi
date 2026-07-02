//! Cross-record service seam — the framework primitive that lets a MODULE own a multi-record, async
//! operation on any model, registered with one `register_service!` line, dispatched by a single generic
//! route. It is the transactional twin of the (pure, same-record) action seam in kigumi-core: where an
//! action returns a value diff, a service runs arbitrary secured reads/writes and returns free-form JSON
//! (a created id, a count, a report).
//!
//! Why this exists: the ERP engines (invoicing, payments, posting, tax application, stock reservation)
//! need exactly this shape. Without it they were written INTO this crate; with it they move OUT into the
//! ERP modules, and kigumi-db keeps only the generic dispatcher — so the ERP becomes an optional layer.
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
//! the raw-SQL escape (`pool`). For a body needing single-transaction atomicity, `run_service` opens ONE
//! transaction the body reaches via `tx()` (FOR UPDATE locking / raw multi-row writes), with `insert_in_tx`
//! (secured insert on that tx), `emit_event` (a domain event atomic with it), and `defer_insert` (a
//! post-commit follow-on insert). Stock reservation/validation and the variant generator use this surface.

use crate::{Db, DbError};
use kigumi_core::{check_access, Acl, Ctx, Domain, Operation, RecordRule, ResolvedModel};
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
pub type ServiceFn = for<'c, 'a, 't> fn(&'c mut ServiceCtx<'a, 't>, ServiceInput) -> BoxServiceFut<'c, Result<ServiceOutput, DbError>>;

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
kigumi_core::inventory::collect!(ServiceRegistration);

/// Looks up a registered service by model + name.
pub fn service_for(model: &str, name: &str) -> Option<&'static ServiceRegistration> {
    kigumi_core::inventory::iter::<ServiceRegistration>
        .into_iter()
        .find(|s| s.model == model && s.name == name)
}

/// All services registered on `model` (for the UI contract, so a form can render its service buttons).
pub fn services_for(model: &str) -> Vec<&'static ServiceRegistration> {
    kigumi_core::inventory::iter::<ServiceRegistration>
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
kigumi_core::inventory::collect!(LedgerReportRegistration);

/// Looks up a registered report by name.
pub fn ledger_report_for(name: &str) -> Option<&'static LedgerReportRegistration> {
    kigumi_core::inventory::iter::<LedgerReportRegistration>.into_iter().find(|r| r.name == name)
}

/// All registered report names (for a UI report menu).
pub fn ledger_report_names() -> Vec<&'static str> {
    kigumi_core::inventory::iter::<LedgerReportRegistration>.into_iter().map(|r| r.name).collect()
}

/// HTTP method of a module route. An enum here (not an http-crate type) keeps this crate HTTP-free;
/// the server translates. Get also serves HEAD (axum's default).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum RouteMethod {
    Get,
    Post,
}

/// Everything a module route body receives, expressed in DB/JSON terms (no axum types):
/// the caller context (authenticated, or the GUEST context for `auth: false` routes), the query
/// params, the parsed JSON body (empty when the body is not a JSON object — NOT an error: webhook
/// senders post forms and raw payloads), the EXACT raw bytes (HMAC signature verification must hash
/// what was sent, not a re-serialization), and the request headers (lowercased names, duplicate
/// values joined with ", ").
pub struct RouteInput {
    pub ctx: Ctx,
    pub query: Map<String, Json>,
    pub body: Map<String, Json>,
    pub raw_body: Vec<u8>,
    pub headers: std::collections::BTreeMap<String, String>,
}

impl RouteInput {
    /// A header by (lowercase) name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
    /// A query param as &str (empty when absent).
    pub fn query_str(&self, key: &str) -> &str {
        self.query.get(key).and_then(|v| v.as_str()).unwrap_or("")
    }
    /// Verifies a hex-encoded HMAC-SHA256 of `raw_body` in CONSTANT TIME — the safe default for a
    /// webhook receiver. Use THIS (or your provider's exact scheme) rather than hand-rolling: a
    /// plain hash of secret+body is length-extension forgeable, and a `==` comparison is a timing
    /// oracle. Returns false on any malformed input.
    pub fn verify_hmac_sha256(&self, secret: &[u8], signature_hex: &str) -> bool {
        use hmac::Mac;
        let Ok(mut mac) = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret) else { return false };
        mac.update(&self.raw_body);
        let Some(sig) = decode_hex(signature_hex) else { return false };
        mac.verify_slice(&sig).is_ok()
    }
}

/// Strict hex decode (lowercase/uppercase accepted), None on any malformation.
fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// What a module route returns: JSON (the normal case) or PLAIN TEXT — some webhook providers'
/// challenge handshakes require echoing a token as an unquoted text body (a JSON string would be
/// quoted and fail their exact-match).
pub enum RouteOutput {
    Json(Json),
    Text(String),
}

/// A registered module route body: bespoke logic over the full `Db` handle. Same trust class as a
/// service body (`ServiceCtx::pool` already hands services the raw pool past the gate): module code
/// is trusted first-party code once it compiles; AUTHORIZATION is the dispatcher's job.
///
/// SECURITY: `query`, `body` and `headers` are RAW CALLER INPUT — on an `auth: false` route,
/// anonymous internet input. When a body runs bespoke SQL via the pool, these values must only ever
/// reach it as BOUND parameters, never interpolated into the SQL string.
pub type RouteFn = for<'a> fn(&'a Db, RouteInput) -> BoxServiceFut<'a, Result<RouteOutput, DbError>>;

/// Registration of a module HTTP route, emitted by `register_route!`. Keyed by (name, method) — a
/// provider like Meta uses GET for its verification handshake and POST for delivery on the SAME
/// path. `auth: true` routes require a bearer (plus the optional group gate); `auth: false` routes
/// run under the GUEST context — uid −1, NO groups, non-su — which the default-deny ACL engine
/// blocks from every secured read/write, so the body must authenticate the caller itself (e.g. an
/// HMAC signature over `raw_body`) and then elevate explicitly via `Ctx::sudo` (the same greppable
/// idiom as `ServiceCtx::elevated`).
pub struct RouteRegistration {
    pub name: &'static str,
    pub method: RouteMethod,
    pub auth: bool,
    pub groups: &'static [&'static str],
    pub func: RouteFn,
}
kigumi_core::inventory::collect!(RouteRegistration);

/// Looks up a route by (name, method).
pub fn route_for(name: &str, method: RouteMethod) -> Option<&'static RouteRegistration> {
    kigumi_core::inventory::iter::<RouteRegistration>
        .into_iter()
        .find(|r| r.name == name && r.method == method)
}

/// The methods registered under `name` (for the 405 Allow header when the method doesn't match).
pub fn route_methods(name: &str) -> Vec<RouteMethod> {
    kigumi_core::inventory::iter::<RouteRegistration>
        .into_iter()
        .filter(|r| r.name == name)
        .map(|r| r.method)
        .collect()
}

/// Startup validation of the route registry: names must be single path segments (the generic
/// `/api/x/:route` route can't match a slash) and (name, method) must be unique. Call once when
/// building the router; a violation is an authoring bug, not a runtime condition.
pub fn validate_routes() -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for r in kigumi_core::inventory::iter::<RouteRegistration> {
        if r.name.contains('/') || r.name.is_empty() {
            return Err(format!("module route '{}' must be a single, non-empty path segment", r.name));
        }
        if !seen.insert((r.name, r.method)) {
            return Err(format!("duplicate module route registration: {} {:?}", r.name, r.method));
        }
        if !r.auth && !r.groups.is_empty() {
            return Err(format!(
                "module route '{}' is auth: false with a group gate — the guest context has no groups, so it could never succeed",
                r.name
            ));
        }
    }
    Ok(())
}

/// An in-tx WRITE TRIGGER — a module hook that runs on the caller's transaction AFTER a secured write to
/// `model`, when one of `watch`ed columns changed (empty = any). The framework's own `depends`-driven
/// recompute handles same-record + child aggregates; this seam covers the effects it can't express on read
/// — chiefly a Many2many aggregate stored across a join (e.g. a product variant's `price_extra` summed over
/// its attribute-value cells). Runs on `&mut Transaction` only (no ACL re-entry), so it stays a pure,
/// engine-owned in-tx side effect, atomic with the write. ZERO ERP literals reach this crate — the model
/// name and the SQL live in the module that owns the tables.
pub type WriteTriggerFn = for<'c, 't> fn(
    &'c mut sqlx::Transaction<'t, sqlx::Postgres>,
    i64,
    &'c [&'c str],
) -> BoxServiceFut<'c, Result<(), DbError>>;

/// Registration of a write trigger, emitted by `register_write_trigger!`.
pub struct WriteTriggerRegistration {
    pub model: &'static str,
    /// Columns whose change fires the trigger; empty fires on every write that matched a row.
    pub watch: &'static [&'static str],
    pub func: WriteTriggerFn,
}
kigumi_core::inventory::collect!(WriteTriggerRegistration);

/// The write triggers registered on `model` (empty when none — the common case, iterated per secured write).
pub fn write_triggers_for(model: &str) -> Vec<&'static WriteTriggerRegistration> {
    kigumi_core::inventory::iter::<WriteTriggerRegistration>
        .into_iter()
        .filter(|t| t.model == model)
        .collect()
}

/// The secured-primitive surface handed to a service body. A concrete struct (no trait, no `dyn`): its
/// methods are `async fn`s delegating to `Db`'s secured CRUD under the caller's context, so the security
/// engine is re-applied on every call. The ERP model-name literals a body resolves live in the MODULE,
/// never in this crate.
pub struct ServiceCtx<'a, 't> {
    db: &'a Db,
    tx: &'a mut sqlx::Transaction<'t, sqlx::Postgres>,
    caller: Ctx,
    acls: &'a [Acl],
    rules: &'a [RecordRule],
    /// Secured inserts to run POST-COMMIT, in order, each on its own transaction (via `defer_insert`) — for
    /// a follow-on record that must NOT roll the main tx back if it fails (e.g. a stock backorder: the
    /// validation stays durable even if the backorder can't be created). run_service drains this after the
    /// body's tx commits.
    deferred: Vec<(ResolvedModel, Ctx, Map<String, Json>)>,
    /// Tracked-field diffs from `update_in_tx`, written to the chatter POST-COMMIT (best-effort, never
    /// propagated) — the same contract as `Db::update_secured`, which writes tracking only after its own
    /// commit. Queued per update as (model, record, author uid, changes); drained by run_service.
    tracking: Vec<(String, i64, i64, Vec<(String, Option<String>, Option<String>)>)>,
}

impl<'a, 't> ServiceCtx<'a, 't> {
    /// The authenticated caller (the dispatcher has already gated it).
    pub fn caller(&self) -> &Ctx {
        &self.caller
    }
    /// The LIVE service transaction, for a body that needs single-transaction atomicity (FOR UPDATE
    /// locking, a compare-and-set, multi-row updates that must commit together) — e.g. stock reservation
    /// / validation. run_service opens it, the body runs all its tx-bound SQL here, and run_service commits
    /// on Ok (rolls back on Err). Pool-based services (the secured-CRUD methods) leave it untouched.
    pub fn tx(&mut self) -> &mut sqlx::Transaction<'t, sqlx::Postgres> {
        self.tx
    }
    /// Emits a domain event on the SERVICE transaction — atomic with the body's writes (a rolled-back
    /// service emits nothing). For a tx-bound service whose event is part of its atomic effect, e.g.
    /// `stock.picking.done` when a transfer is validated. author_uid is the caller.
    pub async fn emit_event(&mut self, event_type: &str, model: &str, record_id: i64, company_id: Option<i64>, changes: Json) -> Result<(), DbError> {
        let uid = self.caller.uid;
        self.db
            .enqueue_event_in_tx(
                self.tx,
                &crate::OutboxEvent {
                    event_type: event_type.to_string(),
                    model: model.to_string(),
                    record_id,
                    author_uid: Some(uid),
                    company_id,
                    change_summary: changes,
                },
            )
            .await
    }
    /// Queues a secured insert to run AFTER the body's tx commits (each on its own transaction, in order).
    /// For a follow-on record whose failure must NOT roll back the main effect — e.g. a stock backorder:
    /// the validation stays durable even if the backorder can't be created (documented non-atomicity).
    pub fn defer_insert(&mut self, model: ResolvedModel, ctx: Ctx, payload: Map<String, Json>) {
        self.deferred.push((model, ctx, payload));
    }
    /// Explicit, greppable elevation past the gate for engine-owned rows (GL lines, join rows, sequences).
    pub fn elevated(&self) -> Ctx {
        self.caller.sudo()
    }
    /// Resolve a model the service owns — the ERP model-name literal lives in the MODULE body, never here.
    pub fn resolve(&self, model: &str) -> Result<ResolvedModel, DbError> {
        kigumi_core::resolve_registered(model).map_err(DbError::BadInput)
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
    /// Runs on the LIVE service transaction, so the claim commits (or rolls back) WITH the body's other
    /// writes: a failure after the claim un-claims. A concurrent claimant's UPDATE blocks on the row lock
    /// until this tx resolves, then re-evaluates its guard — exactly one caller wins either way.
    pub async fn guarded_cas(&mut self, model: &ResolvedModel, id: i64, set_clause: &str, extra_where: &str) -> Result<bool, DbError> {
        self.db.guarded_cas(model, &self.caller, self.rules, id, set_clause, extra_where, self.tx).await
    }
    /// A secured update on the LIVE service transaction (not its own) — the in-tx twin of `update_secured`,
    /// for a write that must commit atomically with the body's other effects (e.g. posting flips a move's
    /// state in the same tx that created it). The full secured path (ACL Write + D6 + record rule/company +
    /// recompute + constraints + write triggers + domain events) runs in-tx; the tracked-field diff is
    /// queued and written to the chatter after run_service commits (best-effort), matching `update_secured`.
    /// Like `insert_in_tx`, it does NOT run the pool wrapper's post-commit parent re-parenting/recompute —
    /// use it where the updated model has no aggregate parent to roll up (e.g. account.move), or roll the
    /// parent up yourself.
    pub async fn update_in_tx(&mut self, m: &ResolvedModel, ctx: &Ctx, id: i64, values: &Map<String, Json>) -> Result<u64, DbError> {
        let (affected, track) = self.db.update_secured_in_tx(m, ctx, self.acls, self.rules, id, values, self.tx).await?;
        if affected > 0 && !track.is_empty() {
            self.tracking.push((m.name.to_string(), id, ctx.uid, track));
        }
        Ok(affected)
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
    /// A secured insert on the LIVE service transaction (not its own), returning the new id — the in-tx
    /// twin of `insert_secured`. For a service that builds a batch of related rows which must commit
    /// atomically together, e.g. the variant generator inserting each variant plus its join rows under one
    /// per-template advisory lock. Like the pre-relocation engine's own `insert_secured_in_tx` use, it does
    /// NOT run the post-commit grandparent recompute (the caller owns the tx); use it where the inserted
    /// model has no aggregate parent to roll up.
    pub async fn insert_in_tx(&mut self, m: &ResolvedModel, ctx: &Ctx, values: &Map<String, Json>) -> Result<i64, DbError> {
        let (id, _record) = self.db.insert_secured_in_tx(m, ctx, self.acls, self.rules, values, self.tx).await?;
        Ok(id)
    }
    pub async fn update_secured(&self, m: &ResolvedModel, ctx: &Ctx, id: i64, values: &Map<String, Json>) -> Result<u64, DbError> {
        self.db.update_secured(m, ctx, self.acls, self.rules, id, values).await
    }
}

impl Db {
    /// The generic service dispatcher — the `run_action` twin. ZERO ERP literals. Gates exactly like
    /// `run_action` (ACL + group + record-rule/company visibility), then runs the registered body on ONE
    /// transaction (committed on Ok, rolled back on Err) and returns its JSON result. Pool-based services
    /// leave the tx untouched (a no-op commit); tx-bound ones drive it through [`ServiceCtx::tx`].
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
        // ONE transaction for the whole body: a tx-bound service (FOR UPDATE / CAS / atomic multi-write)
        // runs its SQL on cx.tx() and commits here; a pool-based service leaves it empty (a no-op commit).
        let mut tx = self.pool.begin().await?;
        let mut cx = ServiceCtx { db: self, tx: &mut tx, caller: ctx.clone(), acls, rules, deferred: Vec::new(), tracking: Vec::new() };
        let result = (reg.func)(&mut cx, ServiceInput { record_id: id, body }).await;
        let deferred = std::mem::take(&mut cx.deferred);
        let tracking = std::mem::take(&mut cx.tracking);
        drop(cx); // release the tx borrow before commit
        let out = result?; // Err → tx drops (rollback) and returns
        tx.commit().await?;
        // Post-commit tracking for the in-tx secured updates: best-effort and never propagated (the write
        // is already durable — an error here would mislead the caller into a retry), exactly like
        // `update_secured`'s own post-commit tracking.
        for (model_name, rec_id, uid, changes) in &tracking {
            if let Err(e) = self.write_tracking(model_name, *rec_id, *uid, changes).await {
                eprintln!("kigumi-db tracking write failed (write committed): {e:?}");
            }
        }
        // Post-commit follow-on inserts (best-effort ordering, each its own tx) — the main effect is already
        // durable, so a failure here surfaces to the caller without un-doing the committed work.
        for (m, c, payload) in &deferred {
            self.insert_secured(m, c, acls, rules, payload).await?;
        }
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

    /// The generic MODULE-ROUTE dispatcher (`POST|GET /api/x/:route`). The server has already
    /// resolved the registration and built the caller context (bearer-authenticated, or the guest
    /// context for `auth: false` routes); this applies the optional group gate and runs the body.
    /// ZERO module literals — a new route needs no edit here or in the server.
    pub async fn run_route(&self, reg: &RouteRegistration, input: RouteInput) -> Result<RouteOutput, DbError> {
        if !reg.groups.is_empty() && !input.ctx.is_su() && !reg.groups.iter().any(|g| input.ctx.is_member(g)) {
            return Err(DbError::AccessDenied { model: format!("route:{}", reg.name), operation: "route (group)" });
        }
        (reg.func)(self, input).await
    }
}
