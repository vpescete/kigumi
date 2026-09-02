//! Headless HTTP layer (axum). Serves the integration surface from a model set:
//! the OpenAPI spec, the model list, per-model UI contracts, and — when a database backend is
//! provided — secured data endpoints that enforce the ACL + record-rule engine.
//!
//! The server is agnostic of any module: a host wires its catalog in with
//! `kigumi_core::resolve_all_registered()` and its security policy, then calls [`router`] or
//! [`router_with_data`]. The core stays headless; this crate is optional.

mod pdf;
pub use pdf::GenpdfRasterizer;
mod oidc;
pub use oidc::OidcState;
use oidc::OidcError;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, RwLock};

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde_json::Value as Json2;
use kigumi_auth::{hash_password, new_jti, Authenticator};
use kigumi_core::{
    check_access, delegated_fields, is_mailed, module_closure, module_of,
    registered_acls, registered_rules, report_for, resolve_modules, wizard_for, Acl, Condition, Ctx,
    Domain, FieldDef, FieldKind, Operation, Operator, RecordRule, ResolvedModel, Value, WizardContext,
    PUBLIC_GROUP,
};
use kigumi_db::{custom_scalar_kind, is_safe_ident, route_for, route_methods, validate_routes, Db, DbError, RouteInput, RouteMethod, RouteOutput, StoredEvent, Translation, ViewOverride};
use kigumi_schema::{openapi, pg_column_type, to_ui_contract};
use kigumi_storage::{sha256_hex, BlobStore};

// Re-exported so hosts (the CLI) and tests can construct a store without a direct kigumi-storage dep.
pub use kigumi_storage::FsBlobStore;

/// Rasterizes a rendered HTML report into PDF bytes. The seam for PDF output: a concrete backend
/// (e.g. typst- or headless-Chromium-based) plugs in via `router_with_data_rasterized`. When none is
/// configured the report endpoint serves HTML and answers `?format=pdf` with 501.
pub trait Rasterizer: Send + Sync {
    fn render_pdf(&self, html: &str) -> Result<Vec<u8>, String>;
}

/// Access tokens are short-lived; refresh tokens long-lived (and revocable/rotated server-side).
const ACCESS_TTL: u64 = 900; // 15 minutes
const REFRESH_TTL: u64 = 2_592_000; // 30 days
/// Maximum request body, bounding an upload (and any JSON write) in memory. Explicit so it is neither
/// axum's restrictive 2 MB default nor unbounded. Config-driven sizing is a later enhancement.
const MAX_BODY_BYTES: usize = 25 * 1024 * 1024;
/// Module routes (webhook receivers) take small payloads; a dedicated cap avoids anonymous 25MB
/// bodies being buffered before any signature check.
const MODULE_ROUTE_BODY_BYTES: usize = 2 * 1024 * 1024;

/// The effective access policy, swappable at runtime. Held as an `Arc` snapshot behind an `RwLock`:
/// a request clones the current snapshot (cheap refcount bump) and reads it without holding the lock
/// across `.await`, while the poll loop and the mutation endpoints replace the snapshot wholesale —
/// so a DB-backed ACL/rule added via the CLI or the `/api/_acl` `/api/_rule` endpoints takes effect
/// with no restart, the same "declarative, no recompile" property as runtime custom fields.
pub type AclState = Arc<RwLock<Arc<[Acl]>>>;
pub type RuleState = Arc<RwLock<Arc<[RecordRule]>>>;

#[derive(Clone)]
struct AppState {
    models: Arc<Vec<ResolvedModel>>,
    /// Names of currently-installed modules — the live "served catalog" gate (Odoo's registry). A
    /// model is served only when its owning module is in this set, so installing/uninstalling a module
    /// takes effect without a restart. EMPTY means "do not gate" (metadata-only router and tests, which
    /// serve exactly the models they were handed). Shared + mutated by the install/uninstall handlers
    /// and a background refresh.
    installed: Arc<RwLock<HashSet<String>>>,
    /// Runtime custom fields, by model name — the declarative-extension layer. Merged into a model when
    /// it is resolved, so a field added at runtime appears in the contract and flows through CRUD with
    /// no recompile. Leaked to `'static` (loaded once at startup + on add), like the runtime ACLs.
    custom_fields: Arc<RwLock<HashMap<String, Vec<FieldDef>>>>,
    /// Runtime view overrides, by model name — the declarative-extension layer for the UI contract.
    /// Applied as a post-pass over the auto-derived contract when a model's `view` is served, so an
    /// admin can relabel / hide / lock / re-widget a field at runtime with no recompile. Owned data
    /// (no leak), so the poll loop can refresh it every tick freely.
    view_overrides: Arc<RwLock<HashMap<String, Vec<ViewOverride>>>>,
    /// Runtime UI translations, by model name — the i18n post-pass over the contract. When a model's
    /// `view` is served with an `Accept-Language`, matching field/option labels are swapped for the
    /// locale's text (fallback: the compile-time English). Owned data, refreshed every poll tick.
    translations: Arc<RwLock<HashMap<String, Vec<Translation>>>>,
    data: Option<DataBackend>,
}

/// Whether `model_name` is currently served: not gated when the installed set is empty, otherwise its
/// owning module must be installed (a model with no resolvable owner is always served).
fn is_served(state: &AppState, model_name: &str) -> bool {
    let inst = state.installed.read().expect("installed lock");
    inst.is_empty() || module_of(model_name).map(|owner| inst.contains(owner)).unwrap_or(true)
}

/// The scalar field kinds a runtime custom field may take (relations are a follow-up).
/// Reloads the live custom-field map from the registry (after an add, and at startup). The
/// `CustomField → FieldDef` conversion now lives in kigumi-db (`custom_fields_by_model`), shared
/// with the MCP server; this only owns the live-refresh cadence.
pub async fn refresh_custom_fields(map: &Arc<RwLock<HashMap<String, Vec<FieldDef>>>>, db: &Db) {
    if let Ok(grouped) = db.custom_fields_by_model().await {
        if let Ok(mut w) = map.write() {
            *w = grouped;
        }
    }
}

/// Groups loaded view overrides into the by-model map the contract post-pass consults.
fn group_view_overrides(rows: &[ViewOverride]) -> HashMap<String, Vec<ViewOverride>> {
    let mut map: HashMap<String, Vec<ViewOverride>> = HashMap::new();
    for o in rows {
        map.entry(o.model.clone()).or_default().push(o.clone());
    }
    map
}

/// Reloads the live view-override map from `ir_ui_view` (after a change, and at startup). Owned data,
/// so unlike the access policy this is leak-free and can run on every poll tick unconditionally.
pub async fn refresh_view_overrides(map: &Arc<RwLock<HashMap<String, Vec<ViewOverride>>>>, db: &Db) {
    if let Ok(rows) = db.load_view_overrides().await {
        if let Ok(mut w) = map.write() {
            *w = group_view_overrides(&rows);
        }
    }
}

/// Applies runtime view overrides as a post-pass over the auto-derived contract JSON: relabel /
/// re-widget / hide / lock a field without touching `to_ui_contract`. Invisible fields are dropped from
/// both `fields` and `list.columns`; `readonly` is forced true only (never false — a computed field's
/// base readonly must stand). Returns the input unchanged if it does not parse (it always should).
fn apply_view_overrides(contract: &str, overrides: &[ViewOverride]) -> String {
    let mut v: Json2 = match serde_json::from_str(contract) {
        Ok(v) => v,
        Err(_) => return contract.to_string(),
    };
    let by_field: HashMap<&str, &ViewOverride> =
        overrides.iter().map(|o| (o.field.as_str(), o)).collect();
    let rewrite = |arr: &mut Vec<Json2>| {
        arr.retain(|item| {
            item.get("name")
                .and_then(|n| n.as_str())
                .and_then(|n| by_field.get(n))
                .map(|o| !o.invisible)
                .unwrap_or(true)
        });
        for item in arr.iter_mut() {
            let Some(name) = item.get("name").and_then(|n| n.as_str()).map(str::to_string) else {
                continue;
            };
            let Some(o) = by_field.get(name.as_str()) else { continue };
            let Some(obj) = item.as_object_mut() else { continue };
            if let Some(l) = &o.label {
                obj.insert("label".into(), Json2::String(l.clone()));
            }
            if let Some(w) = &o.widget {
                obj.insert("widget".into(), Json2::String(w.clone()));
            }
            if o.readonly {
                obj.insert("readonly".into(), Json2::Bool(true));
            }
            // Conditional domains: inject the stored JSON AST as `invisible_when`/`readonly_when` so the
            // frontend evaluates them per record (the same keys a compile-time UI rule would emit).
            for (col, dom) in [("invisible_when", &o.invisible_when), ("readonly_when", &o.readonly_when)] {
                if let Some(d) = dom {
                    if let Ok(ast) = serde_json::from_str::<Json2>(d) {
                        obj.insert(col.into(), ast);
                    }
                }
            }
        }
    };
    if let Some(arr) = v.get_mut("fields").and_then(|f| f.as_array_mut()) {
        rewrite(arr);
    }
    if let Some(cols) =
        v.get_mut("list").and_then(|l| l.get_mut("columns")).and_then(|c| c.as_array_mut())
    {
        rewrite(cols);
    }
    v.to_string()
}

fn group_translations(rows: &[Translation]) -> HashMap<String, Vec<Translation>> {
    let mut map: HashMap<String, Vec<Translation>> = HashMap::new();
    for t in rows {
        map.entry(t.model.clone()).or_default().push(t.clone());
    }
    map
}

/// Reloads the live translation map from `ir_translation` (after a change, and at startup). Owned data,
/// so like the view overrides this can run on every poll tick unconditionally.
pub async fn refresh_translations(map: &Arc<RwLock<HashMap<String, Vec<Translation>>>>, db: &Db) {
    if let Ok(rows) = db.load_translations().await {
        if let Ok(mut w) = map.write() {
            *w = group_translations(&rows);
        }
    }
}

/// The primary language subtag of an `Accept-Language` header, lowercased (`it-IT,it;q=0.9,en` -> `it`).
/// Absent/empty -> `None`.
// ponytail: first tag's primary subtag; add full q-value negotiation only if a caller needs it.
fn accept_language(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get("accept-language")?.to_str().ok()?;
    let primary = raw.split(',').next()?.split(';').next()?.trim().split('-').next()?.trim();
    (!primary.is_empty()).then(|| primary.to_ascii_lowercase())
}

/// Post-pass over the contract JSON: for `lang`, replace field labels, selection option labels, and
/// matching list-column labels with their translation. Anything without a translation keeps the
/// compile-time English. Returns the input unchanged if there is nothing for `lang` or it does not parse.
fn apply_translations(contract: &str, translations: &[Translation], lang: &str) -> String {
    // Lang-specific lookups: (field -> label) for value "", and (field, option value -> label) otherwise.
    let mut field_label: HashMap<&str, &str> = HashMap::new();
    let mut option_label: HashMap<(&str, &str), &str> = HashMap::new();
    for t in translations.iter().filter(|t| t.lang == lang) {
        if t.value.is_empty() {
            field_label.insert(&t.field, &t.text);
        } else {
            option_label.insert((&t.field, &t.value), &t.text);
        }
    }
    if field_label.is_empty() && option_label.is_empty() {
        return contract.to_string();
    }
    let mut v: Json2 = match serde_json::from_str(contract) {
        Ok(v) => v,
        Err(_) => return contract.to_string(),
    };
    let translate = |arr: &mut Vec<Json2>| {
        for item in arr.iter_mut() {
            let Some(name) = item.get("name").and_then(|n| n.as_str()).map(str::to_string) else {
                continue;
            };
            let Some(obj) = item.as_object_mut() else { continue };
            if let Some(t) = field_label.get(name.as_str()) {
                obj.insert("label".into(), Json2::String((*t).to_string()));
            }
            // Selection option labels live in the field's `options` array of {value, label}.
            if let Some(Json2::Array(opts)) = obj.get_mut("options") {
                for opt in opts.iter_mut() {
                    let Some(val) = opt.get("value").and_then(|x| x.as_str()).map(str::to_string) else {
                        continue;
                    };
                    if let Some(t) = option_label.get(&(name.as_str(), val.as_str())) {
                        if let Some(o) = opt.as_object_mut() {
                            o.insert("label".into(), Json2::String((*t).to_string()));
                        }
                    }
                }
            }
        }
    };
    if let Some(arr) = v.get_mut("fields").and_then(|f| f.as_array_mut()) {
        translate(arr);
    }
    if let Some(cols) =
        v.get_mut("list").and_then(|l| l.get_mut("columns")).and_then(|c| c.as_array_mut())
    {
        translate(cols);
    }
    v.to_string()
}

#[derive(Clone)]
struct DataBackend {
    db: Arc<Db>,
    /// Live outbox batches from the shared poller task — each SSE client subscribes here.
    events: tokio::sync::broadcast::Sender<Arc<Vec<StoredEvent>>>,
    acls: AclState,
    rules: RuleState,
    auth: Arc<Authenticator>,
    blobs: Arc<dyn BlobStore>,
    rasterizer: Option<Arc<dyn Rasterizer>>,
    /// The configured SSO provider, or `None` when `[oidc]` is absent (the /auth/oidc routes then 404).
    oidc: Option<Arc<OidcState>>,
}

impl DataBackend {
    /// A consistent snapshot of the effective ACLs (cheap `Arc` clone — does not hold the lock).
    fn acls(&self) -> Arc<[Acl]> {
        self.acls.read().expect("acls lock").clone()
    }
    /// A consistent snapshot of the effective record rules.
    fn rules(&self) -> Arc<[RecordRule]> {
        self.rules.read().expect("rules lock").clone()
    }
    /// Reload the effective access policy after a mutation — but only if the rows actually changed
    /// since `before` (a no-op upsert must not pay the load_*_static leak). The caller captures the
    /// fingerprint before its DB write and passes it here.
    async fn reload_access(&self, before: u64) {
        if access_fingerprint(&self.db).await != before {
            refresh_access(&self.acls, &self.rules, &self.db).await;
        }
    }
}

/// Recomputes the effective access policy — compiled-in baseline ∪ DB overrides — and swaps it into
/// the live snapshots, so a runtime ACL/rule change takes effect without a restart. On a DB load error
/// it KEEPS the prior good snapshot (it does not degrade to baseline-only — that would silently drop a
/// live restricting rule) and returns `false`, so the caller can retry on the next tick. Returns `true`
/// only when both halves reloaded. NB it leaks the DB rows' identifier strings (load_*_static), so the
/// poll loop and the mutation endpoints guard it behind [`access_fingerprint`] and only call it when the
/// rows changed (rare → bounded leak, like a custom-field add).
pub async fn refresh_access(acls: &AclState, rules: &RuleState, db: &Db) -> bool {
    let mut ok = true;
    match db.load_acls_static().await {
        Ok(db_a) => {
            let mut a = registered_acls();
            a.extend(db_a);
            if let Ok(mut w) = acls.write() {
                *w = Arc::from(a);
            }
        }
        // Keep the prior snapshot (which still holds the baseline) rather than dropping the DB grants.
        Err(_) => ok = false,
    }
    match db.load_rules_static().await {
        Ok(db_r) => {
            let mut r = registered_rules();
            r.extend(db_r);
            if let Ok(mut w) = rules.write() {
                *w = Arc::from(r);
            }
        }
        Err(_) => ok = false,
    }
    ok
}

/// A cheap content hash of the DB-backed ACL/rule rows (owned data, no leak). The poll loop compares
/// it across ticks and only reloads the policy when it changes — so out-of-band edits (the `kigumi
/// acl/rule` CLI, direct SQL) go live without paying the load_*_static string leak on every tick.
pub async fn access_fingerprint(db: &Db) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Ok(acls) = db.list_db_acls().await {
        for a in &acls {
            (a.model.as_str(), a.group.as_str(), a.read, a.write, a.create, a.delete).hash(&mut h);
        }
    }
    if let Ok(rules) = db.list_db_rules().await {
        for r in &rules {
            (r.id, r.model.as_str(), r.groups.as_str(), r.ops.as_str(), r.domain.as_str(), r.active)
                .hash(&mut h);
        }
    }
    h.finish()
}

fn base_router() -> Router<AppState> {
    Router::new()
        .route("/openapi.json", get(openapi_handler))
        .route("/api/models", get(models_handler))
        .route("/api/modules", get(modules_handler))
        .route("/api/modules/:name/install", post(module_install_handler))
        .route("/api/modules/:name/uninstall", post(module_uninstall_handler))
        .route("/api/:name/_fields", post(add_field_handler))
        .route("/api/:name/_view", get(list_view_handler).post(add_view_handler))
        .route("/api/:name/_translation", post(add_translation_handler))
        .route("/api/:name/view", get(view_handler))
}

/// Metadata-only router: OpenAPI spec, model list, UI contracts. No database.
pub fn router(models: Vec<ResolvedModel>) -> Router {
    base_router().with_state(AppState {
        models: Arc::new(models),
        installed: Arc::new(RwLock::new(HashSet::new())),
        custom_fields: Arc::new(RwLock::new(HashMap::new())),
        view_overrides: Arc::new(RwLock::new(HashMap::new())),
        translations: Arc::new(RwLock::new(HashMap::new())),
        data: None,
    })
}

/// Full router: metadata routes plus secured CRUD data endpoints. `auth_secret` is the HS256
/// secret used to verify the `Authorization: Bearer <token>` of each data request into a `Ctx`.
/// No PDF rasterizer is configured (report `?format=pdf` answers 501); see
/// [`router_with_data_rasterized`] to attach one.
pub fn router_with_data(
    models: Vec<ResolvedModel>,
    db: Db,
    acls: &'static [Acl],
    rules: &'static [RecordRule],
    auth_secret: impl Into<String>,
    blobs: Arc<dyn BlobStore>,
) -> Router {
    router_with_data_rasterized(models, db, acls, rules, auth_secret, blobs, None)
}

/// Like [`router_with_data`] but with a PDF rasterizer for report `?format=pdf` (None → those requests
/// get 501).
#[allow(clippy::too_many_arguments)]
pub fn router_with_data_rasterized(
    models: Vec<ResolvedModel>,
    db: Db,
    acls: &'static [Acl],
    rules: &'static [RecordRule],
    auth_secret: impl Into<String>,
    blobs: Arc<dyn BlobStore>,
    rasterizer: Option<Arc<dyn Rasterizer>>,
) -> Router {
    // Empty installed set = no gating: serves exactly `models` (the host/tests' chosen catalog). No
    // runtime custom fields (tests use the compile-time models directly). The `'static` baseline is
    // wrapped into a (non-refreshed) live snapshot — fine for tests, which never mutate access.
    build_data_router(
        models,
        Arc::new(RwLock::new(HashSet::new())),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
        db,
        Arc::new(RwLock::new(Arc::from(acls))),
        Arc::new(RwLock::new(Arc::from(rules))),
        auth_secret,
        blobs,
        rasterizer,
        None,
    )
}

/// Like [`router_with_data`] but with a **live served catalog**: `installed` (a shared, mutable set of
/// installed module names) gates which models are served, so installing/uninstalling a module via the
/// `/api/modules/*` endpoints takes effect without restarting the process (the host passes the FULL
/// linked catalog as `models` and keeps `installed` in sync with the DB). Used by `kigumi serve`.
#[allow(clippy::too_many_arguments)]
pub fn router_with_data_dynamic(
    models: Vec<ResolvedModel>,
    installed: Arc<RwLock<HashSet<String>>>,
    custom_fields: Arc<RwLock<HashMap<String, Vec<FieldDef>>>>,
    view_overrides: Arc<RwLock<HashMap<String, Vec<ViewOverride>>>>,
    translations: Arc<RwLock<HashMap<String, Vec<Translation>>>>,
    db: Db,
    acls: AclState,
    rules: RuleState,
    auth_secret: impl Into<String>,
    blobs: Arc<dyn BlobStore>,
    oidc: Option<Arc<OidcState>>,
) -> Router {
    build_data_router(
        models,
        installed,
        custom_fields,
        view_overrides,
        translations,
        db,
        acls,
        rules,
        auth_secret,
        blobs,
        None,
        oidc,
    )
}

/// Like [`router_with_data_dynamic`] but with a PDF rasterizer for report `?format=pdf` (None → 501).
#[allow(clippy::too_many_arguments)]
pub fn router_with_data_dynamic_rasterized(
    models: Vec<ResolvedModel>,
    installed: Arc<RwLock<HashSet<String>>>,
    custom_fields: Arc<RwLock<HashMap<String, Vec<FieldDef>>>>,
    view_overrides: Arc<RwLock<HashMap<String, Vec<ViewOverride>>>>,
    translations: Arc<RwLock<HashMap<String, Vec<Translation>>>>,
    db: Db,
    acls: AclState,
    rules: RuleState,
    auth_secret: impl Into<String>,
    blobs: Arc<dyn BlobStore>,
    rasterizer: Option<Arc<dyn Rasterizer>>,
    oidc: Option<Arc<OidcState>>,
) -> Router {
    build_data_router(
        models,
        installed,
        custom_fields,
        view_overrides,
        translations,
        db,
        acls,
        rules,
        auth_secret,
        blobs,
        rasterizer,
        oidc,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_data_router(
    models: Vec<ResolvedModel>,
    installed: Arc<RwLock<HashSet<String>>>,
    custom_fields: Arc<RwLock<HashMap<String, Vec<FieldDef>>>>,
    view_overrides: Arc<RwLock<HashMap<String, Vec<ViewOverride>>>>,
    translations: Arc<RwLock<HashMap<String, Vec<Translation>>>>,
    db: Db,
    acls: AclState,
    rules: RuleState,
    auth_secret: impl Into<String>,
    blobs: Arc<dyn BlobStore>,
    rasterizer: Option<Arc<dyn Rasterizer>>,
    oidc: Option<Arc<OidcState>>,
) -> Router {
    // Module route registrations are compile-time-authored; an invalid one (slash in the name, a
    // duplicate (name, method)) is a bug that must fail the boot, not surface as a puzzling 404.
    if let Err(e) = validate_routes() {
        panic!("kigumi-server: {e}");
    }
    let db = Arc::new(db);
    let (events, _) = tokio::sync::broadcast::channel::<Arc<Vec<StoredEvent>>>(64);
    spawn_event_poller(db.clone(), events.clone());
    base_router()
        .route("/auth/login", post(login_handler))
        .route("/auth/refresh", post(refresh_handler))
        .route("/auth/logout", post(logout_handler))
        .route("/auth/oidc/start", get(oidc_start_handler))
        .route("/auth/oidc/callback", get(oidc_callback_handler))
        .route("/auth/me", get(me_handler))
        .route("/auth/keys", get(list_keys_handler).post(create_key_handler))
        .route("/auth/keys/:id", axum::routing::delete(revoke_key_handler))
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        // Runtime access policy (admin only): grant a DB ACL / add a DB record rule, live (no restart).
        // Static segments, so they take precedence over the `/api/:name` model routes below.
        .route("/api/_acl", post(set_acl_handler))
        .route("/api/_rule", post(set_rule_handler))
        .route("/api/_webhooks", get(list_webhooks_handler).post(create_webhook_handler))
        .route("/api/_webhooks/:id/deactivate", post(deactivate_webhook_handler))
        .route("/api/:name", get(list_handler).post(create_handler))
        .route(
            "/api/:name/:id",
            get(get_one_handler).patch(update_handler).delete(delete_handler),
        )
        .route("/api/:name/:id/action/:action", post(action_handler))
        // The ONE generic cross-record service route (the run_action twin). A module owns the logic via
        // register_service! and kigumi-db dispatches by capability — no per-service handler, no model-name
        // literal. The named ERP endpoints below are migrating onto this and will be deleted.
        .route("/api/:name/:id/service/:service", post(service_handler))
        // Read-only ledger reports (trial balance, general ledger, aged balance): GET /api/reports/:report.
        .route("/api/reports/:report", get(ledger_report_handler))
        // The live record stream: gap-safe outbox events, visibility-filtered per caller (SSE).
        .route("/api/events/stream", get(events_stream_handler))
        // Module HTTP routes (register_route!): bespoke module endpoints — inbound webhook receivers,
        // custom reads — dispatched by name with zero module literals here. x = extension.
        .nest(
            "/api/x",
            Router::new()
                .route("/:route", get(module_route_get).post(module_route_post))
                .layer(axum::extract::DefaultBodyLimit::max(MODULE_ROUTE_BODY_BYTES)),
        )
        // Open a wizard (transient model): seed it via default_get and return the scratchpad record.
        .route("/api/:name/open", post(open_wizard_handler))
        // Render a record's report as HTML (secured entirely by read access to the record).
        .route("/api/:name/:id/report/:report", get(report_handler))
        // Attachments (ir.attachment): files on a record. List/download need host read; upload/delete
        // need host write. Bytes live in the content-addressed blob store; the row is metadata.
        .route("/api/:name/:id/attachments", get(list_attachments_handler).post(upload_attachment_handler))
        .route("/api/attachment/:aid/content", get(download_attachment_handler))
        .route("/api/attachment/:aid", delete(delete_attachment_handler))
        // Chatter (mail subsystem): a record's message thread. Gated by read access to the host.
        .route("/api/:name/:id/messages", get(messages_handler))
        .route("/api/:name/:id/message", post(post_message_handler))
        // Activities (to-dos) on a record: list open ones (state derived), schedule, mark done.
        .route("/api/:name/:id/activities", get(activities_handler))
        .route("/api/:name/:id/activity", post(schedule_activity_handler))
        .route("/api/:name/:id/activities/:aid/done", post(activity_done_handler))
        // Followers: who is subscribed to a record's thread. Follow/unfollow are idempotent.
        .route("/api/:name/:id/followers", get(followers_handler))
        .route("/api/:name/:id/follow", post(follow_handler))
        .route("/api/:name/:id/unfollow", post(unfollow_handler))
        .with_state(AppState {
            models: Arc::new(models),
            installed,
            custom_fields,
            view_overrides,
            translations,
            data: Some(DataBackend {
                db,
                events,
                acls,
                rules,
                auth: Arc::new(Authenticator::new(auth_secret)),
                blobs,
                rasterizer,
                oidc,
            }),
        })
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        // Outermost: one span per request (method/path/status/latency), completed requests at INFO,
        // failures at ERROR. Metadata only — TraceLayer never records request or response bodies.
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .on_response(tower_http::trace::DefaultOnResponse::new().level(tracing::Level::INFO)),
        )
}

/// Installs the global tracing subscriber from `[log]` config: `level` seeds the filter (the standard
/// `RUST_LOG` env var overrides it) and `format` picks `json` (structured, for production log
/// pipelines) or text (human-readable, the default). Call once at startup from the binary's `main`;
/// a second call is a harmless no-op, so both the CLI and the runtime may call it.
pub fn init_tracing(level: &str, format: &str) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    // try_init errors only when a subscriber is already set — ignore it so double-init is a no-op.
    let _ = match format {
        "json" => fmt().with_env_filter(filter).json().try_init(),
        _ => fmt().with_env_filter(filter).try_init(),
    };
}

/// The SHARED outbox poller: one task per data router, broadcasting gap-safe event batches to every
/// SSE subscriber. While idle (zero subscribers) it only ADVANCES its cursor to the outbox head
/// (one index-only MAX per tick — no row reads), so on wake the backlog is at most one tick and
/// NOTHING a subscriber is entitled to can be skipped: a client's own cutoff (`id > last`, set at
/// connect) drops the pre-connect remainder, and any event after a connect has an id above the
/// idle cursor, so the first active tick reads it. Resumes are exact regardless of poller state
/// (each handler runs its own Last-Event-ID catch-up query).
/// The SSE event id: the full `(txn, id)` stream cursor, so Last-Event-ID resumes are exact.
fn sse_id(ev: &serde_json::Value) -> String {
    format!("{}:{}", ev["txn"].as_i64().unwrap_or(0), ev["id"].as_i64().unwrap_or(0))
}

/// A fresh snapshot from a live ACL/rule state (cheap Arc clone under a read lock).
fn snapshot<T: ?Sized>(state: &Arc<RwLock<Arc<T>>>) -> Arc<T> {
    state.read().expect("access state poisoned").clone()
}

fn spawn_event_poller(db: Arc<Db>, tx: tokio::sync::broadcast::Sender<Arc<Vec<StoredEvent>>>) {
    const TICK_MS: u64 = 250;
    tokio::spawn(async move {
        let mut cursor: Option<(i64, i64)> = None;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(TICK_MS)).await;
            if tx.receiver_count() == 0 {
                match db.latest_event_cursor().await {
                    Ok(c) => cursor = Some(c),
                    Err(e) => tracing::error!("kigumi-server event poller (idle cursor) failed: {e:?}"),
                }
                continue;
            }
            let from = match cursor {
                Some(c) => c,
                None => match db.latest_event_cursor().await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("kigumi-server event poller (cursor) failed: {e:?}");
                        continue;
                    }
                },
            };
            match db.events_after(from, 200).await {
                Ok(events) if events.is_empty() => {
                    cursor = Some(from);
                }
                Ok(events) => {
                    cursor = Some(events.last().map(|e| (e.txn, e.id)).unwrap_or(from));
                    let _ = tx.send(Arc::new(events)); // no receivers = fine
                }
                Err(e) => tracing::error!("kigumi-server event poller failed: {e:?}"),
            }
        }
    });
}

/// Verifies the request's bearer token into a trusted `Ctx`, or a 401 response. This is real
/// authentication: a client cannot claim a group without a token signed by the server secret.
/// How often a busy API key restamps `last_used_at` (at most once per this window).
const API_KEY_TOUCH_THROTTLE_SECS: i64 = 300;

/// Turns the bearer token into the caller's `Ctx`. A token with the API-key scheme (`kg_`) is
/// looked up in the key store, verified constant-time, and resolved to its user's Ctx narrowed to
/// the key's scopes; anything else is a JWT (`verify_bearer`, stateless, fast). One 401 for every
/// failure — never distinguish "no such key" from "bad secret".
async fn authenticate(backend: &DataBackend, headers: &HeaderMap) -> Result<Ctx, Response> {
    let header = headers.get("authorization").and_then(|v| v.to_str().ok());
    let unauthorized = || (StatusCode::UNAUTHORIZED, "unauthorized").into_response();

    // API-key path: "Authorization: Bearer kg_<prefix>_<secret>".
    if let Some(token) = header.and_then(|h| h.strip_prefix("Bearer ")) {
        if let Some((prefix, secret)) = kigumi_auth::parse_api_key(token) {
            let key = backend.db.find_api_key(&prefix).await.map_err(|_| unauthorized())?;
            // Timing equalizer (review should-fix): a missing/revoked/expired prefix must spend the
            // same Argon2 as a live one with a wrong secret, so latency does not leak key liveness.
            // Throttled: shed under a verification flood (busy is uniform, leaks nothing).
            let hash = key.as_ref().map(|k| k.hash.as_str()).unwrap_or_else(|| dummy_hash());
            let Some(ok) = kigumi_auth::verify_password_throttled(&secret, hash) else {
                return Err((StatusCode::SERVICE_UNAVAILABLE, "authentication is busy, retry shortly").into_response());
            };
            let Some(key) = key else { return Err(unauthorized()) };
            if !ok {
                return Err(unauthorized());
            }
            // Impersonate the user, narrowed to the key's scopes — the identity math lives once in
            // kigumi-db (shared with the MCP server), so the never-exceed-your-user contract has a
            // single implementation.
            let ctx = backend.db.build_key_ctx(key.user_id, &key.scopes).await.map_err(|_| unauthorized())?;
            // Best-effort usage stamp; never fails the request.
            let _ = backend.db.touch_api_key(&prefix, API_KEY_TOUCH_THROTTLE_SECS).await;
            return Ok(ctx);
        }
    }

    backend.auth.verify_bearer(header).map_err(|_| unauthorized())
}

fn json_response(body: String) -> Response {
    ([("content-type", "application/json")], body).into_response()
}

fn json_status(status: StatusCode, body: String) -> Response {
    (status, [("content-type", "application/json")], body).into_response()
}

/// Resolves the model for a path name, or a 404 response.
/// Resolves a served model, with its runtime custom fields MERGED in — returns an owned `ResolvedModel`
/// (a cheap clone of the compile-time base plus any custom fields). Owning it decouples the value from
/// the custom-field lock so handlers can hold it across `.await`. 404 when the model isn't served.
fn resolve_model(state: &AppState, name: &str) -> Result<ResolvedModel, Response> {
    let base = state
        .models
        .iter()
        .find(|m| m.name == name)
        .filter(|_| is_served(state, name))
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("unknown model: {name}")).into_response())?;
    let mut model = base.clone();
    if let Ok(map) = state.custom_fields.read() {
        if let Some(extra) = map.get(name) {
            model.fields.extend(extra.iter().copied());
        }
    }
    Ok(model)
}

/// The structured error envelope every DbError-mapped response carries:
/// `{"error": {"code": "<kebab>", "message": "...", "fields": {"<field>": ["<msg>", …]}}}`.
/// `fields` is present only when the failure is attributable to specific fields (a form renders
/// them inline); `message` is always the human line. Statuses are unchanged from the plain-text era.
fn error_response(status: StatusCode, code: &str, message: &str, fields: &[(String, String)]) -> Response {
    let mut err = serde_json::Map::new();
    err.insert("code".into(), serde_json::Value::String(code.to_string()));
    err.insert("message".into(), serde_json::Value::String(message.to_string()));
    if !fields.is_empty() {
        let mut map = serde_json::Map::new();
        for (field, msg) in fields {
            map.entry(field.clone())
                .or_insert_with(|| serde_json::Value::Array(Vec::new()))
                .as_array_mut()
                .expect("fields entries are arrays")
                .push(serde_json::Value::String(msg.clone()));
        }
        err.insert("fields".into(), serde_json::Value::Object(map));
    }
    let body = serde_json::json!({ "error": err }).to_string();
    (status, [("content-type", "application/json")], body).into_response()
}

/// Maps a write DbError to an HTTP response (opaque 500, never leaking schema/SQL on the 500 path).
fn write_error(context: &str, e: DbError) -> Response {
    match e {
        DbError::AccessDenied { .. } => error_response(StatusCode::FORBIDDEN, "access-denied", "access denied", &[]),
        DbError::BadInput(msg) => error_response(StatusCode::BAD_REQUEST, "bad-input", &msg, &[]),
        DbError::Invalid { message, fields } => error_response(StatusCode::BAD_REQUEST, "invalid", &message, &fields),
        DbError::Conflict(msg) => error_response(StatusCode::CONFLICT, "conflict", &msg, &[]),
        other => internal_error(context, other),
    }
}

async fn openapi_handler(State(state): State<AppState>) -> Response {
    let refs: Vec<&ResolvedModel> = state.models.iter().collect();
    json_response(openapi(&refs))
}

async fn models_handler(State(state): State<AppState>) -> Json<Vec<String>> {
    Json(
        state
            .models
            .iter()
            .filter(|m| is_served(&state, m.name))
            .map(|m| m.name.to_string())
            .collect(),
    )
}

/// Logs the detail server-side and returns an opaque 500 — internal schema/SQL detail must never
/// reach the client (it would leak table/column names and Postgres error text).
fn internal_error(context: &str, detail: impl std::fmt::Debug) -> Response {
    tracing::error!("kigumi-server {context} error: {detail:?}");
    error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal error", &[])
}

/// Liveness: the process is up. No DB touch — safe for a fast container health probe.
async fn health_handler() -> Response {
    json_response("{\"status\":\"ok\"}".to_string())
}

/// Readiness: the process can serve traffic (database reachable). 503 until it can.
async fn ready_handler(State(state): State<AppState>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    match backend.db.ping().await {
        Ok(_) => json_response("{\"status\":\"ready\"}".to_string()),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "{\"status\":\"not_ready\"}").into_response(),
    }
}

/// The authenticated caller's own identity — the trusted `Ctx` derived from the bearer token.
async fn me_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let body = serde_json::json!({
        "uid": ctx.uid,
        "groups": ctx.groups,
        "company_id": ctx.company_id,
        "allowed_company_ids": ctx.allowed_company_ids,
    });
    json_response(body.to_string())
}

/// `POST /auth/keys` — mint an API key for the authenticated caller. Body: `name` (required),
/// `scopes` (CSV, a SUBSET of the caller's own groups — a key never widens access), `expires_in`
/// (seconds, optional; omit for no expiry). The plain key is returned ONCE and never again.
async fn create_key_handler(State(state): State<AppState>, headers: HeaderMap, Json(body): Json<Json2>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on auth routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let Some(name) = str_field(&body, "name") else {
        return error_response(StatusCode::BAD_REQUEST, "bad-input", "a key needs a 'name'", &[]);
    };
    if name.len() > 200 {
        return error_response(StatusCode::BAD_REQUEST, "bad-input", "name is too long (max 200)", &[]);
    }
    let requested: Vec<String> = str_field(&body, "scopes")
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    // A key can only NARROW: every requested scope must be a group the caller actually holds.
    if let Some(bad) = requested.iter().find(|g| !ctx.groups.contains(g)) {
        return error_response(StatusCode::FORBIDDEN, "access-denied", &format!("scope '{bad}' is not one of your groups"), &[]);
    }
    // Scopes are FROZEN to the minter's CURRENT effective groups (review must-fix): an absent
    // request must not mean "all the user's groups, dynamically" — else a narrowed key could mint
    // an un-narrowed key and regain what it was scoped away from. Empty request = exactly the
    // groups the caller holds right now; an explicit request is that subset. A key thus never
    // grants more than the credential that minted it.
    let scopes: Vec<String> = if requested.is_empty() { ctx.groups.clone() } else { requested };
    let expires_in = body.get("expires_in").and_then(|v| v.as_i64()).filter(|&n| n > 0);
    let minted = match kigumi_auth::new_api_key() {
        Ok(m) => m,
        Err(_) => return internal_error("create_key", DbError::BadInput("key generation failed".into())),
    };
    match backend.db.create_api_key(&minted.prefix, &minted.hash, ctx.uid, name, &scopes, expires_in).await {
        Ok(id) => json_status(
            StatusCode::CREATED,
            serde_json::json!({ "id": id, "prefix": minted.prefix, "key": minted.plain,
                "note": "store this key now — it is not recoverable" }).to_string(),
        ),
        Err(e) => write_error("create_key", e),
    }
}

/// `GET /auth/keys` — list the caller's live keys (never the secret or the hash).
async fn list_keys_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let backend = state.data.as_ref().expect("data backend present on auth routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match backend.db.list_api_keys(ctx.uid).await {
        Ok(keys) => {
            let rows: Vec<Json2> = keys
                .iter()
                .map(|k| serde_json::json!({ "id": k.id, "prefix": k.prefix, "name": k.name,
                    "scopes": k.scopes, "expires_at": k.expires_at, "last_used_at": k.last_used_at,
                    "created_at": k.created_at }))
                .collect();
            json_response(serde_json::json!({ "data": rows }).to_string())
        }
        Err(e) => internal_error("list_keys", e),
    }
}

/// `DELETE /auth/keys/:id` — revoke one of the caller's keys (soft-delete). 404 if it is not
/// theirs or already gone — a caller can only revoke their own.
async fn revoke_key_handler(State(state): State<AppState>, Path(id): Path<i64>, headers: HeaderMap) -> Response {
    let backend = state.data.as_ref().expect("data backend present on auth routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match backend.db.revoke_api_key(id, ctx.uid).await {
        Ok(true) => json_status(StatusCode::OK, serde_json::json!({ "revoked": true }).to_string()),
        Ok(false) => error_response(StatusCode::NOT_FOUND, "not-found", "no such key", &[]),
        Err(e) => internal_error("revoke_key", e),
    }
}

/// Lists every linked module with its manifest + installed state. Any authenticated user may read it.
async fn modules_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    if let Err(r) = authenticate(backend, &headers).await {
        return r;
    }
    let mods = match resolve_modules() {
        Ok(m) => m,
        Err(e) => return internal_error("modules", e),
    };
    let installed = match backend.db.installed_modules().await {
        Ok(i) => i,
        Err(e) => return write_error("modules", e),
    };
    let items: Vec<serde_json::Value> = mods
        .iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name,
                "version": m.version,
                "summary": m.summary,
                "framework": m.framework,
                "depends": m.depends.iter().map(|d| serde_json::json!({ "name": d.name, "req": d.req })).collect::<Vec<_>>(),
                "installed": installed.iter().any(|i| i == m.name),
            })
        })
        .collect();
    json_response(serde_json::json!(items).to_string())
}

/// Installs a module + its dependency closure: marks the ledger, MIGRATES the new modules' tables, and
/// adds them to the live served catalog — all without a restart (the served set is consulted per
/// request, like a registry). Reference-data seeds are not run here (they apply on `kigumi migrate`).
/// Admin only.
async fn module_install_handler(State(state): State<AppState>, Path(name): Path<String>, headers: HeaderMap) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !ctx.is_member("admin") {
        return (StatusCode::FORBIDDEN, "installing a module requires the admin group").into_response();
    }
    let want = match module_closure(&name) {
        Ok(w) => w,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let mods = match resolve_modules() {
        Ok(m) => m,
        Err(e) => return internal_error("modules", e),
    };
    let mut installed_now: Vec<&str> = Vec::new();
    for m in mods.iter().filter(|m| want.contains(&m.name)) {
        match backend.db.is_module_installed(m.name).await {
            Ok(true) => {}
            Ok(false) => match backend.db.mark_module_installed(m.name, m.version).await {
                Ok(_) => installed_now.push(m.name),
                Err(e) => return write_error("module_install", e),
            },
            Err(e) => return write_error("module_install", e),
        }
    }
    // Migrate the freshly-installed modules' tables (idempotent over the whole installed set), then add
    // them to the live served catalog so their models are reachable immediately — no restart.
    if let Err(e) = backend.db.migrate_installed_schema().await {
        return write_error("module_install", e);
    }
    refresh_installed(&state.installed, backend).await;
    json_response(serde_json::json!({ "installed": installed_now, "needs_restart": false }).to_string())
}

/// Reloads the live served-catalog set from the install ledger. Called after install/uninstall and by
/// a periodic background refresh, so a change (here or by another process / the CLI) is reflected
/// without a restart — the single-process analogue of Odoo's registry-change signaling.
async fn refresh_installed(installed: &Arc<RwLock<HashSet<String>>>, backend: &DataBackend) {
    if let Ok(names) = backend.db.installed_modules().await {
        if let Ok(mut w) = installed.write() {
            *w = names.into_iter().collect();
        }
    }
}

/// Uninstalls a module (marks the ledger; its tables and data are kept). Refuses `base` and any module
/// an installed module still depends on. Applies live (the installed set refreshes). Admin only.
async fn module_uninstall_handler(State(state): State<AppState>, Path(name): Path<String>, headers: HeaderMap) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !ctx.is_member("admin") {
        return (StatusCode::FORBIDDEN, "uninstalling a module requires the admin group").into_response();
    }
    if name == "base" {
        return (StatusCode::BAD_REQUEST, "cannot uninstall 'base' (the foundational module)").into_response();
    }
    match backend.db.is_module_installed(&name).await {
        Ok(false) => return (StatusCode::BAD_REQUEST, format!("module '{name}' is not installed")).into_response(),
        Ok(true) => {}
        Err(e) => return write_error("module_uninstall", e),
    }
    let mods = match resolve_modules() {
        Ok(m) => m,
        Err(e) => return internal_error("modules", e),
    };
    let installed = match backend.db.installed_modules().await {
        Ok(i) => i,
        Err(e) => return write_error("modules", e),
    };
    let dependents: Vec<&str> = mods
        .iter()
        .filter(|m| installed.iter().any(|i| i == m.name) && m.depends.iter().any(|d| d.name == name))
        .map(|m| m.name)
        .collect();
    if !dependents.is_empty() {
        return (StatusCode::BAD_REQUEST, format!("uninstall {dependents:?} first — they depend on '{name}'")).into_response();
    }
    match backend.db.mark_module_uninstalled(&name).await {
        Ok(_) => {
            refresh_installed(&state.installed, backend).await;
            json_response(serde_json::json!({ "uninstalled": name, "needs_restart": false }).to_string())
        }
        Err(e) => write_error("module_uninstall", e),
    }
}

/// Adds a runtime custom field to a model: registers it, adds the column, and merges it into the live
/// catalog — it appears in the contract and flows through CRUD immediately, no recompile. Admin only.
/// Scalar kinds: text | integer | float | decimal | bool | date | datetime.
async fn add_field_handler(State(state): State<AppState>, Path(name): Path<String>, headers: HeaderMap, Json(body): Json<Json2>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !ctx.is_member("admin") {
        return (StatusCode::FORBIDDEN, "adding a field requires the admin group").into_response();
    }
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let obj = match body_object(&body) {
        Ok(o) => o,
        Err(r) => return r,
    };
    let Some(fname) = str_field(&body, "name") else {
        return (StatusCode::BAD_REQUEST, "'name' is required").into_response();
    };
    let Some(kind_str) = str_field(&body, "kind") else {
        return (StatusCode::BAD_REQUEST, "'kind' is required").into_response();
    };
    let label = str_field(&body, "label").unwrap_or(fname);
    let required = obj.get("required").and_then(|v| v.as_bool()).unwrap_or(false);
    let default = str_field(&body, "default");
    // many2one carries a target model in `relation` and gets a bigint FK column; scalars map directly.
    let relation = str_field(&body, "relation");
    let col_type = if kind_str == "many2one" {
        let Some(target) = relation else {
            return (StatusCode::BAD_REQUEST, "a many2one field needs a 'relation' (the target model)").into_response();
        };
        if resolve_model(&state, target).is_err() {
            return (StatusCode::BAD_REQUEST, format!("unknown target model '{target}'")).into_response();
        }
        pg_column_type(&FieldKind::Many2one { target: "" })
    } else {
        let Some(kind) = custom_scalar_kind(kind_str) else {
            return (StatusCode::BAD_REQUEST, format!("unsupported kind '{kind_str}' (text|integer|float|decimal|bool|date|datetime|many2one)")).into_response();
        };
        pg_column_type(&kind)
    };
    if !is_safe_ident(fname) {
        return (StatusCode::BAD_REQUEST, "field name must be lowercase letters, digits and underscore").into_response();
    }
    if model.fields.iter().any(|f| f.name == fname) {
        return (StatusCode::BAD_REQUEST, format!("field '{fname}' already exists on {name}")).into_response();
    }
    match backend.db.add_custom_field(&name, fname, label, kind_str, required, default, relation, model.table, col_type).await {
        Ok(_) => {
            refresh_custom_fields(&state.custom_fields, &backend.db).await;
            json_response(serde_json::json!({ "added": fname, "model": name }).to_string())
        }
        Err(e) => write_error("add_field", e),
    }
}

async fn view_handler(State(state): State<AppState>, Path(name): Path<String>, headers: HeaderMap) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let json = match to_ui_contract(&model, &[]) {
        Ok(j) => j,
        Err(e) => return internal_error("view", e),
    };
    // Post-pass 1: runtime view overrides (relabel/hide/lock/re-widget) for this model.
    let overrides = state
        .view_overrides
        .read()
        .ok()
        .and_then(|m| m.get(&name).cloned())
        .unwrap_or_default();
    let json = if overrides.is_empty() { json } else { apply_view_overrides(&json, &overrides) };
    // Post-pass 2: per-locale label/option translations for the caller's Accept-Language. No header (or
    // no translations for the model) leaves the compile-time English in place.
    let json = match accept_language(&headers) {
        Some(lang) => {
            let tr = state
                .translations
                .read()
                .ok()
                .and_then(|m| m.get(&name).cloned())
                .unwrap_or_default();
            if tr.is_empty() { json } else { apply_translations(&json, &tr, &lang) }
        }
        None => json,
    };
    json_response(json)
}

/// The runtime view overrides configured on a model (admin only) — the inverse of the contract's
/// post-pass: needed to UNDO an override (a hidden field is dropped from the contract, so the form
/// cannot show a control for it; this is how a Studio UI lists hidden fields to offer "Show").
async fn list_view_handler(State(state): State<AppState>, Path(name): Path<String>, headers: HeaderMap) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !ctx.is_member("admin") {
        return (StatusCode::FORBIDDEN, "viewing overrides requires the admin group").into_response();
    }
    let rows = state
        .view_overrides
        .read()
        .ok()
        .and_then(|m| m.get(&name).cloned())
        .unwrap_or_default();
    // The conditional domains are stored as JSON text; emit them as parsed AST (or null) so the UI can
    // pre-fill an editor.
    let cond = |s: &Option<String>| -> Json2 {
        s.as_deref().and_then(|d| serde_json::from_str::<Json2>(d).ok()).unwrap_or(Json2::Null)
    };
    let items: Vec<Json2> = rows
        .iter()
        .map(|o| {
            serde_json::json!({
                "field": o.field,
                "label": o.label,
                "widget": o.widget,
                "invisible": o.invisible,
                "readonly": o.readonly,
                "invisible_when": cond(&o.invisible_when),
                "readonly_when": cond(&o.readonly_when),
            })
        })
        .collect();
    json_response(serde_json::Value::Array(items).to_string())
}

/// Override a field's UI on a model (admin only): relabel / hide / lock / re-widget, or make it
/// conditionally invisible/readonly via a domain, at runtime, no recompile. Body: `{field, label?,
/// widget?, invisible?, readonly?, invisible_when?, readonly_when?}` where the `*_when` values are JSON
/// domain ASTs (validated against the model). Upserts the `ir_ui_view` row and reloads the live map.
/// Pure UI metadata — it cannot grant access (the ACL/rule layer) or change storage (a custom field).
async fn add_view_handler(State(state): State<AppState>, Path(name): Path<String>, headers: HeaderMap, Json(body): Json<Json2>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !ctx.is_member("admin") {
        return (StatusCode::FORBIDDEN, "overriding a view requires the admin group").into_response();
    }
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let Some(field) = str_field(&body, "field") else {
        return (StatusCode::BAD_REQUEST, "'field' is required").into_response();
    };
    // The field must appear in the served contract: a model-own/custom field, or a delegated (_inherits)
    // parent field — both are rendered, so both are legitimately overridable.
    let known = model.fields.iter().any(|f| f.name == field)
        || delegated_fields(&name).unwrap_or_default().iter().any(|d| d.def.name == field);
    if !known {
        return (StatusCode::BAD_REQUEST, format!("unknown field '{field}' on {name}")).into_response();
    }
    // The override row is a full upsert, but a client sends a PARTIAL patch (e.g. just `readonly`), so
    // merge over the existing override: a key present in the body wins, an absent key keeps its stored
    // value (otherwise hiding a field would wipe an earlier relabel, etc.).
    let existing = state
        .view_overrides
        .read()
        .ok()
        .and_then(|m| m.get(&name).and_then(|v| v.iter().find(|o| o.field == field).cloned()));
    let label = if body.get("label").is_some() {
        str_field(&body, "label").map(str::to_string)
    } else {
        existing.as_ref().and_then(|e| e.label.clone())
    };
    let widget = if body.get("widget").is_some() {
        str_field(&body, "widget").map(str::to_string)
    } else {
        existing.as_ref().and_then(|e| e.widget.clone())
    };
    let bool_field = |k: &str, prev: bool| match body.get(k) {
        Some(v) => v.as_bool().unwrap_or(false),
        None => prev,
    };
    let invisible = bool_field("invisible", existing.as_ref().map(|e| e.invisible).unwrap_or(false));
    let readonly = bool_field("readonly", existing.as_ref().map(|e| e.readonly).unwrap_or(false));
    // Conditional domains: a present value is parsed + validated against the model (an unknown field
    // would otherwise break the field's render); a null clears it; an absent key keeps the stored one.
    let parse_cond = |key: &str, prev: Option<String>| -> Result<Option<String>, Response> {
        if body.get(key).is_none() {
            return Ok(prev);
        }
        match body.get(key).filter(|v| !v.is_null()) {
            None => Ok(None),
            Some(d) => {
                let json = d.to_string();
                Domain::from_json(&json)
                    .and_then(|dm| dm.validate(&model))
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid {key}: {e:?}")).into_response())?;
                Ok(Some(json))
            }
        }
    };
    let invisible_when = match parse_cond("invisible_when", existing.as_ref().and_then(|e| e.invisible_when.clone())) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let readonly_when = match parse_cond("readonly_when", existing.as_ref().and_then(|e| e.readonly_when.clone())) {
        Ok(v) => v,
        Err(r) => return r,
    };
    match backend
        .db
        .set_view_override(
            &name,
            field,
            label.as_deref(),
            widget.as_deref(),
            invisible,
            readonly,
            invisible_when.as_deref(),
            readonly_when.as_deref(),
        )
        .await
    {
        Ok(_) => {
            refresh_view_overrides(&state.view_overrides, &backend.db).await;
            json_response(serde_json::json!({ "view": { "model": name, "field": field } }).to_string())
        }
        Err(e) => write_error("set_view", e),
    }
}

/// Set a per-locale translation for a field label or selection option (admin only): the i18n sibling of
/// the view override, applied as a post-pass when the contract is served under a matching
/// `Accept-Language`. Body: `{field, lang, text, value?}` — `value` empty/absent translates the field's
/// own label, a non-empty `value` translates that selection option's label. Upserts the `ir_translation`
/// row and reloads the live map. Pure UI metadata — it cannot grant access or change storage.
async fn add_translation_handler(State(state): State<AppState>, Path(name): Path<String>, headers: HeaderMap, Json(body): Json<Json2>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !ctx.is_member("admin") {
        return (StatusCode::FORBIDDEN, "setting a translation requires the admin group").into_response();
    }
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let (Some(field), Some(lang), Some(text)) =
        (str_field(&body, "field"), str_field(&body, "lang"), str_field(&body, "text"))
    else {
        return (StatusCode::BAD_REQUEST, "'field', 'lang' and 'text' are required").into_response();
    };
    let value = str_field(&body, "value").unwrap_or(""); // "" = the field's own label
    // The field must appear in the served contract (a model-own/custom field, or a delegated parent
    // field) — the same gate as a view override. A bogus option `value` is harmless (it simply never
    // matches an option in the contract), so it is not separately validated.
    let known = model.fields.iter().any(|f| f.name == field)
        || delegated_fields(&name).unwrap_or_default().iter().any(|d| d.def.name == field);
    if !known {
        return (StatusCode::BAD_REQUEST, format!("unknown field '{field}' on {name}")).into_response();
    }
    match backend.db.set_translation(&name, field, value, lang, text).await {
        Ok(_) => {
            refresh_translations(&state.translations, &backend.db).await;
            json_response(
                serde_json::json!({ "translation": { "model": name, "field": field, "lang": lang } }).to_string(),
            )
        }
        Err(e) => write_error("set_translation", e),
    }
}

/// Grant or update a runtime ACL (admin only): the DB half of the hybrid ir.model.access. Upserts the
/// `(model, group)` grant and reloads the live policy so it takes effect with no restart. DB ACLs only
/// WIDEN access (they union with the compiled-in baseline) — this can never revoke a static grant.
async fn set_acl_handler(State(state): State<AppState>, headers: HeaderMap, Json(body): Json<Json2>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !ctx.is_member("admin") {
        return (StatusCode::FORBIDDEN, "managing access requires the admin group").into_response();
    }
    let Some(model) = str_field(&body, "model") else {
        return (StatusCode::BAD_REQUEST, "'model' is required").into_response();
    };
    let Some(group) = str_field(&body, "group") else {
        return (StatusCode::BAD_REQUEST, "'group' is required").into_response();
    };
    let flag = |k: &str| body.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let before = access_fingerprint(&backend.db).await;
    match backend
        .db
        .set_db_acl(model, group, flag("read"), flag("write"), flag("create"), flag("delete"))
        .await
    {
        Ok(_) => {
            backend.reload_access(before).await;
            json_response(serde_json::json!({ "acl": { "model": model, "group": group } }).to_string())
        }
        Err(e) => write_error("set_acl", e),
    }
}

/// Registers a webhook subscription (admin). Body: {name, url, event_filter?:[..], company_id?}. The
/// per-subscription HMAC secret is generated server-side (CSPRNG) and returned ONCE — never again.
async fn create_webhook_handler(State(state): State<AppState>, headers: HeaderMap, Json(body): Json<Json2>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !ctx.is_member("admin") {
        return (StatusCode::FORBIDDEN, "managing webhooks requires the admin group").into_response();
    }
    let Some(url) = str_field(&body, "url") else {
        return (StatusCode::BAD_REQUEST, "'url' is required").into_response();
    };
    if !url.starts_with("https://") {
        return (StatusCode::BAD_REQUEST, "webhook url must be https").into_response();
    }
    let name = str_field(&body, "name").unwrap_or("webhook");
    let filter: Vec<String> = body
        .get("event_filter")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let company_id = body.get("company_id").and_then(|v| v.as_i64());
    // A strong write-only secret, shown once.
    let secret = format!("whsec_{}{}", new_jti(), new_jti());
    match backend.db.create_webhook_subscription(name, url, &secret, &filter, company_id).await {
        Ok(id) => json_response(serde_json::json!({ "id": id, "secret": secret }).to_string()),
        Err(e) => write_error("create_webhook", e),
    }
}

/// Lists webhook subscriptions (admin) — never the secret.
async fn list_webhooks_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !ctx.is_member("admin") {
        return (StatusCode::FORBIDDEN, "managing webhooks requires the admin group").into_response();
    }
    match backend.db.list_webhook_subscriptions().await {
        Ok(rows) => json_response(serde_json::json!({ "subscriptions": rows }).to_string()),
        Err(e) => write_error("list_webhooks", e),
    }
}

/// Deactivates a webhook subscription (admin).
async fn deactivate_webhook_handler(State(state): State<AppState>, Path(id): Path<i64>, headers: HeaderMap) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !ctx.is_member("admin") {
        return (StatusCode::FORBIDDEN, "managing webhooks requires the admin group").into_response();
    }
    match backend.db.deactivate_webhook_subscription(id).await {
        Ok(true) => json_response(serde_json::json!({ "deactivated": id }).to_string()),
        Ok(false) => (StatusCode::NOT_FOUND, "no such subscription").into_response(),
        Err(e) => write_error("deactivate_webhook", e),
    }
}

/// Add a runtime record rule (admin only): the DB half of the hybrid ir.rule. Body: `{model, domain,
/// groups?, ops?}` where `domain` is the JSON domain AST, `groups` a CSV (empty/absent = global), and
/// `ops` a CSV subset of r/w/c/d (default `r`). The domain is validated against the model at write time
/// (a rule on an unknown field would otherwise brick every non-superuser read of the model at query
/// time). Reloads the live policy and returns the new rule id. Rules compose through the engine like
/// Odoo's ir.rule: a global rule (no groups) AND-restricts everyone; a group rule OR-grants its groups
/// an additional alternative — so a group rule can widen that group's access (admin authority).
async fn set_rule_handler(State(state): State<AppState>, headers: HeaderMap, Json(body): Json<Json2>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !ctx.is_member("admin") {
        return (StatusCode::FORBIDDEN, "managing access requires the admin group").into_response();
    }
    let Some(model) = str_field(&body, "model") else {
        return (StatusCode::BAD_REQUEST, "'model' is required").into_response();
    };
    let Some(domain) = body.get("domain").filter(|v| !v.is_null()) else {
        return (StatusCode::BAD_REQUEST, "'domain' is required").into_response();
    };
    let domain_json = domain.to_string();
    // Validate the domain against the resolved (custom-field-merged) model up front, so a rule that
    // references an unknown/mistyped field is rejected here (400) instead of failing every read later.
    let resolved = match resolve_model(&state, model) {
        Ok(m) => m,
        Err(r) => return r,
    };
    if let Err(e) = Domain::from_json(&domain_json).and_then(|d| d.validate(&resolved)) {
        return (StatusCode::BAD_REQUEST, format!("invalid rule domain: {e:?}")).into_response();
    }
    let groups = str_field(&body, "groups").unwrap_or("");
    let ops = str_field(&body, "ops").unwrap_or("r");
    let before = access_fingerprint(&backend.db).await;
    match backend.db.set_db_rule(model, groups, ops, &domain_json).await {
        Ok(id) => {
            backend.reload_access(before).await;
            json_response(serde_json::json!({ "rule": id, "model": model }).to_string())
        }
        Err(e) => write_error("set_rule", e),
    }
}

const DEFAULT_LIMIT: i64 = 80;
const MAX_LIMIT: i64 = 500;

async fn list_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let (filter, order, limit, offset) = match parse_list_query(&model, &params) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match backend
        .db
        .list_secured(&model, &ctx, &backend.acls(), &backend.rules(), filter.as_ref(), &order, limit, offset)
        .await
    {
        Ok(page) => {
            let body = serde_json::json!({
                "data": page.data, "total": page.total, "limit": limit, "offset": offset,
            });
            json_response(body.to_string())
        }
        Err(DbError::AccessDenied { .. }) => (StatusCode::FORBIDDEN, "access denied").into_response(),
        Err(DbError::BadInput(msg)) => bad_request(msg),
        Err(DbError::Domain(e)) => bad_request(format!("invalid filter: {e:?}")),
        Err(e) => internal_error("data", e),
    }
}

fn bad_request(msg: String) -> Response {
    (StatusCode::BAD_REQUEST, msg).into_response()
}

/// Parses list query params into (filter, order, limit, offset). Two filter forms (decision D5):
/// suffix operators `field__op=value` (the default, AND-ed) and a `?domain=<json AST>` escape for
/// arbitrary AND/OR/NOT — AND-ed together when both are present.
fn parse_list_query(
    model: &ResolvedModel,
    params: &HashMap<String, String>,
) -> Result<(Option<Domain>, Vec<(String, bool)>, i64, i64), Response> {
    let mut conds: Vec<Domain> = Vec::new();
    if let Some(js) = params.get("domain") {
        match Domain::from_json(js) {
            Ok(d) => conds.push(d),
            Err(e) => return Err(bad_request(format!("invalid domain JSON: {e:?}"))),
        }
    }
    for (key, raw) in params {
        if matches!(key.as_str(), "domain" | "order" | "limit" | "offset") {
            continue;
        }
        let (field, op) = split_suffix(key);
        conds.push(build_leaf(model, field, op, raw).map_err(bad_request)?);
    }
    let filter = conds.into_iter().reduce(|a, b| a.and(b));

    let mut order = Vec::new();
    if let Some(o) = params.get("order") {
        for part in o.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            match part.strip_prefix('-') {
                Some(f) => order.push((f.to_string(), true)),
                None => order.push((part.to_string(), false)),
            }
        }
    }

    let limit = match params.get("limit") {
        Some(s) => s.parse::<i64>().map_err(|_| bad_request("limit must be an integer".into()))?.clamp(1, MAX_LIMIT),
        None => DEFAULT_LIMIT,
    };
    let offset = match params.get("offset") {
        Some(s) => s.parse::<i64>().map_err(|_| bad_request("offset must be an integer".into()))?.max(0),
        None => 0,
    };
    Ok((filter, order, limit, offset))
}

/// Splits `field__op` into (field, op); a bare `field` defaults to the `eq` operator.
fn split_suffix(key: &str) -> (&str, &str) {
    match key.rfind("__") {
        Some(i) => (&key[..i], &key[i + 2..]),
        None => (key, "eq"),
    }
}

/// Builds one leaf condition, coercing the raw string to the field's typed value.
fn build_leaf(model: &ResolvedModel, field: &str, op: &str, raw: &str) -> Result<Domain, String> {
    let kind = if field == "id" {
        FieldKind::Integer
    } else {
        model
            .fields
            .iter()
            .find(|f| f.name == field)
            .map(|f| f.kind)
            .ok_or_else(|| format!("unknown filter field '{field}'"))?
    };
    let operator = match op {
        "eq" => Operator::Eq,
        "ne" => Operator::Ne,
        "gt" => Operator::Gt,
        "gte" => Operator::Ge,
        "lt" => Operator::Lt,
        "lte" => Operator::Le,
        "like" => Operator::Like,
        "ilike" => Operator::ILike,
        "in" => Operator::In,
        other => return Err(format!("unknown operator suffix '__{other}'")),
    };
    let value = if operator == Operator::In {
        Value::List(raw.split(',').map(|s| coerce(&kind, s.trim())).collect::<Result<Vec<_>, _>>()?)
    } else {
        coerce(&kind, raw)?
    };
    Ok(Domain::Cond(Condition { field: field.to_string(), op: operator, value }))
}

/// Coerces a query string to a typed [`Value`] for the field's kind.
fn coerce(kind: &FieldKind, raw: &str) -> Result<Value, String> {
    Ok(match kind {
        FieldKind::Integer | FieldKind::Many2one { .. } | FieldKind::Image => {
            Value::Int(raw.parse().map_err(|_| format!("'{raw}' is not an integer"))?)
        }
        FieldKind::Decimal { .. } => {
            Value::Decimal(raw.parse::<rust_decimal::Decimal>().map_err(|_| format!("'{raw}' is not a number"))?)
        }
        FieldKind::Float => Value::Float(raw.parse::<f64>().map_err(|_| format!("'{raw}' is not a number"))?),
        FieldKind::Bool => Value::Bool(matches!(raw, "true" | "1" | "t" | "yes")),
        // Date/Datetime filters travel as ISO strings; Postgres validates the cast at query time.
        FieldKind::Text | FieldKind::Html | FieldKind::Selection(_) | FieldKind::Date | FieldKind::Datetime => {
            Value::Str(raw.to_string())
        }
        FieldKind::One2many { .. } | FieldKind::Many2many { .. } => {
            return Err("cannot filter on a One2many/Many2many field directly".to_string())
        }
    })
}

async fn get_one_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match backend.db.find_one_secured(&model, &ctx, &backend.acls(), &backend.rules(), id).await {
        Ok(Some(obj)) => {
            json_response(serde_json::to_string(&obj).unwrap_or_else(|_| "{}".to_string()))
        }
        Ok(None) => (StatusCode::NOT_FOUND, "not found or not permitted").into_response(),
        Err(DbError::AccessDenied { .. }) => (StatusCode::FORBIDDEN, "access denied").into_response(),
        Err(e) => internal_error("data", e),
    }
}

fn body_object(body: &Json2) -> Result<&serde_json::Map<String, Json2>, Response> {
    body.as_object()
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "body must be a JSON object").into_response())
}

async fn create_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Json2>,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let obj = match body_object(&body) {
        Ok(o) => o,
        Err(r) => return r,
    };
    match backend.db.insert_secured(&model, &ctx, &backend.acls(), &backend.rules(), obj).await {
        Ok(id) => json_status(StatusCode::CREATED, format!("{{\"id\": {id}}}")),
        Err(e) => write_error("create", e),
    }
}

async fn update_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
    Json(body): Json<Json2>,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let obj = match body_object(&body) {
        Ok(o) => o,
        Err(r) => return r,
    };
    match backend.db.update_secured(&model, &ctx, &backend.acls(), &backend.rules(), id, obj).await {
        Ok(0) => (StatusCode::NOT_FOUND, "not found or not permitted").into_response(),
        Ok(n) => json_status(StatusCode::OK, format!("{{\"updated\": {n}}}")),
        Err(e) => write_error("update", e),
    }
}

/// Runs a registered state-transition action on a record (e.g. confirm a draft order).
async fn action_handler(
    State(state): State<AppState>,
    Path((name, id, action)): Path<(String, i64, String)>,
    headers: HeaderMap,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match backend.db.run_action(&model, &ctx, &backend.acls(), &backend.rules(), id, &action).await {
        Ok(()) => json_response(format!("{{\"ok\":true,\"action\":{}}}", serde_json::to_string(&action).unwrap_or_default())),
        Err(e) => write_error("action", e),
    }
}

/// The generic cross-record service dispatch. Resolves the model (404 if its module isn't served),
/// authenticates, then runs `db.run_service` which gates (ACL + group + visibility) and invokes the
/// module-registered service body. Zero ERP model-name literals — a new service needs no edit here.
async fn service_handler(
    State(state): State<AppState>,
    Path((name, id, service)): Path<(String, i64, String)>,
    headers: HeaderMap,
    body: Option<Json<Json2>>,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let body_map = match body {
        Some(Json(Json2::Object(m))) => m,
        _ => serde_json::Map::new(),
    };
    match backend.db.run_service(&model, &ctx, &backend.acls(), &backend.rules(), id, &service, body_map).await {
        Ok(json) => json_response(json.to_string()),
        Err(e) => write_error("service", e),
    }
}

/// The live record stream: `GET /api/events/stream?models=a,b` (Server-Sent Events). Authenticated;
/// each event is visibility-filtered for THIS caller by the exact read path (Read ACL + record
/// rules + company scope), against a FRESH ACL/rule snapshot per batch (a revoked grant applies to
/// the next batch, not the next reconnect). The stream itself is BOUNDED to the access-token TTL:
/// it closes after ACCESS_TTL and the client transparently reconnects with a fresh bearer — an
/// open stream can never outlive its credential by more than one TTL, matching the rest of the
/// system's revocation bound. Events carry a `txn:id` cursor as the SSE id for exact Last-Event-ID
/// resume (a fresh client streams from "now"); delivery is at-least-once from connect and the
/// payload is a change HINT — the client refetches records through the normal secured reads.
async fn events_stream_handler(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let models: Vec<String> = params
        .get("models")
        .map(|m| m.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();
    // Resume point: the standard Last-Event-ID header ("txn:id"; a bare integer reads as txn 0,
    // replaying the legacy prefix), else "now".
    let resume: Option<(i64, i64)> = headers.get("last-event-id").and_then(|v| v.to_str().ok()).and_then(|v| {
        match v.split_once(':') {
            Some((t, i)) => Some((t.parse().ok()?, i.parse().ok()?)),
            None => Some((0, v.parse().ok()?)),
        }
    });
    let mut rx = backend.events.subscribe();
    let db = backend.db.clone();
    let acl_state = backend.acls.clone();
    let rule_state = backend.rules.clone();

    let stream = async_stream::stream! {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(ACCESS_TTL);
        let mut last: (i64, i64) = match resume {
            Some(c) => c,
            None => match db.latest_event_cursor().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("kigumi-server event stream (cursor) failed: {e:?}");
                    return;
                }
            },
        };
        let wanted = |ev: &StoredEvent| models.is_empty() || models.iter().any(|m| *m == ev.model);
        // Catch-up: everything between the resume point and "now" (paged), then go live. The same
        // loop re-runs when the broadcast reports lag (the client was slower than the buffer).
        loop {
            // -- catch-up pages --
            loop {
                let page = match db.events_after(last, 200).await {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::error!("kigumi-server event stream (catch-up) failed: {e:?}");
                        return;
                    }
                };
                if page.is_empty() {
                    break;
                }
                last = page.last().map(|e| (e.txn, e.id)).unwrap_or(last);
                let mine: Vec<StoredEvent> = page.into_iter().filter(|e| wanted(e)).collect();
                if mine.is_empty() {
                    continue;
                }
                // Fresh snapshots per batch: a revoked ACL/rule applies to the NEXT batch.
                let (acls, rules) = (snapshot(&acl_state), snapshot(&rule_state));
                match db.visible_events(&ctx, &acls, &rules, &mine).await {
                    Ok(shaped) => {
                        for ev in shaped {
                            yield Ok::<_, std::convert::Infallible>(
                                axum::response::sse::Event::default().id(sse_id(&ev)).data(ev.to_string()),
                            );
                        }
                    }
                    // Transient filter failure: skip this page (events are hints), don't kill the stream.
                    Err(e) => tracing::error!("kigumi-server event stream (filter) failed: {e:?}"),
                }
            }
            // -- live batches (bounded by the token TTL: on expiry the client reconnects fresh) --
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Err(_) => return, // TTL reached on a quiet stream
                Ok(Ok(batch)) => {
                    let mine: Vec<StoredEvent> = batch
                        .iter()
                        .filter(|e| (e.txn, e.id) > last && wanted(e))
                        .cloned()
                        .collect();
                    if let Some(max) = batch.last().map(|e| (e.txn, e.id)) {
                        last = last.max(max);
                    }
                    if mine.is_empty() {
                        continue;
                    }
                    let (acls, rules) = (snapshot(&acl_state), snapshot(&rule_state));
                    match db.visible_events(&ctx, &acls, &rules, &mine).await {
                        Ok(shaped) => {
                            for ev in shaped {
                                yield Ok(axum::response::sse::Event::default().id(sse_id(&ev)).data(ev.to_string()));
                            }
                        }
                        Err(e) => tracing::error!("kigumi-server event stream (filter) failed: {e:?}"),
                    }
                }
                // Lagged: the broadcast buffer overtook this client — fall back to a catch-up query.
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => return,
            }
        }
    };
    axum::response::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::new().interval(std::time::Duration::from_secs(15)))
        .into_response()
}

/// The generic MODULE-ROUTE dispatch: `GET|POST /api/x/:route`. Resolves the module-registered
/// route by (name, method), builds the caller context — bearer-authenticated for `auth: true`
/// routes, the GUEST context (uid −1, carrying only the inert `public` group: default-deny under the
/// ACL engine until an adopter adds a `public` ACL) for
/// `auth: false` receivers that verify their sender themselves — and hands the body the query,
/// the parsed-JSON-or-empty body, the EXACT raw bytes (HMAC material) and the lowercased headers.
/// Method mismatch on an existing name → 405 with Allow. Zero module literals.
async fn module_route_handler(
    state: AppState,
    method: RouteMethod,
    route: String,
    headers: HeaderMap,
    query: HashMap<String, String>,
    raw_body: Bytes,
) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let Some(reg) = route_for(&route, method) else {
        let others = route_methods(&route);
        if others.is_empty() {
            return (StatusCode::NOT_FOUND, format!("unknown route: {route}")).into_response();
        }
        // The name exists under other methods. If every registration under it requires auth, an
        // anonymous probe gets a uniform 401 (matching /api/reports) instead of a method map.
        let all_auth = others.iter().all(|m| route_for(&route, *m).map(|r| r.auth).unwrap_or(true));
        if all_auth {
            if let Err(r) = authenticate(backend, &headers).await {
                return r;
            }
        }
        let allow = others
            .iter()
            .map(|m| match m {
                RouteMethod::Get => "GET",
                RouteMethod::Post => "POST",
            })
            .collect::<Vec<_>>()
            .join(", ");
        return (StatusCode::METHOD_NOT_ALLOWED, [("allow", allow)], "method not allowed").into_response();
    };
    let ctx = if reg.auth {
        match authenticate(backend, &headers).await {
            Ok(c) => c,
            Err(r) => return r,
        }
    } else {
        // uid −1 is the reserved guest identity (0 = superuser, real users ≥ 1). It carries only the
        // PUBLIC_GROUP, which grants nothing until an adopter adds a `public` ACL — so an HMAC webhook
        // receiver still hits default-deny before it `.sudo()`s, while a portal route can read exactly
        // the rows a `public` ACL+rule expose. Still company-scoped to shared rows (no company set).
        Ctx::new(-1, vec![PUBLIC_GROUP.to_string()])
    };
    // Headers: lowercased names, duplicates joined with ", ", non-UTF8 values dropped (signature
    // headers are ASCII by construction).
    let mut hdrs = std::collections::BTreeMap::new();
    for (name, value) in headers.iter() {
        if let Ok(v) = value.to_str() {
            hdrs.entry(name.as_str().to_ascii_lowercase())
                .and_modify(|cur: &mut String| {
                    cur.push_str(", ");
                    cur.push_str(v);
                })
                .or_insert_with(|| v.to_string());
        }
    }
    // Body: a JSON object parses into `body`; anything else (forms, raw payloads, empty) is NOT an
    // error — the raw bytes are always available.
    let body_map = match serde_json::from_slice::<Json2>(&raw_body) {
        Ok(Json2::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    let input = RouteInput {
        ctx,
        query: query.into_iter().map(|(k, v)| (k, Json2::String(v))).collect(),
        body: body_map,
        raw_body: raw_body.to_vec(),
        headers: hdrs,
    };
    match backend.db.run_route(reg, input).await {
        Ok(RouteOutput::Json(json)) => (
            [("content-type", "application/json"), ("x-content-type-options", "nosniff")],
            json.to_string(),
        )
            .into_response(),
        Ok(RouteOutput::Text(text)) => (
            StatusCode::OK,
            [("content-type", "text/plain; charset=utf-8"), ("x-content-type-options", "nosniff")],
            text,
        )
            .into_response(),
        Err(e) => write_error("route", e),
    }
}

async fn module_route_get(
    State(state): State<AppState>,
    Path(route): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    raw_body: Bytes,
) -> Response {
    module_route_handler(state, RouteMethod::Get, route, headers, query, raw_body).await
}

async fn module_route_post(
    State(state): State<AppState>,
    Path(route): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    raw_body: Bytes,
) -> Response {
    module_route_handler(state, RouteMethod::Post, route, headers, query, raw_body).await
}

/// The ONE generic read-only ledger-report dispatch: GET /api/reports/:report?<params>. Resolves the
/// module-registered report by name, gates Read on its declared model (in run_ledger_report), and returns
/// the JSON rows. Query params are passed through as strings (e.g. ?account_id=5, ?kind=receivable). Zero
/// ERP report names in the router — a new report needs no edit here.
async fn ledger_report_handler(
    State(state): State<AppState>,
    Path(report): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let pmap: serde_json::Map<String, Json2> =
        params.into_iter().map(|(k, v)| (k, Json2::String(v))).collect();
    match backend.db.run_ledger_report(&ctx, &backend.acls(), &report, pmap).await {
        Ok(rows) => json_response(serde_json::json!({ "rows": rows }).to_string()),
        Err(e) => write_error("report", e),
    }
}

/// Converts a metamodel [`Value`] into JSON for the secured insert path (decimals stay strings to
/// preserve precision, matching the rest of the API).
fn value_to_json(v: &Value) -> Json2 {
    match v {
        Value::Str(s) => Json2::String(s.clone()),
        Value::Int(n) => Json2::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f).map(Json2::Number).unwrap_or(Json2::Null),
        Value::Decimal(d) => Json2::String(d.to_string()),
        Value::Bool(b) => Json2::Bool(*b),
        Value::Null => Json2::Null,
        Value::List(xs) => Json2::Array(xs.iter().map(value_to_json).collect()),
    }
}

/// Renders a registered report for one record as an HTML document. Security is read access to the
/// record: `find_one_secured` applies the ACL, record rules and company scope, so being able to read
/// the record is exactly what lets you print it. Unknown report name → 404.
async fn report_handler(
    State(state): State<AppState>,
    Path((name, id, report)): Path<(String, i64, String)>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let Some(reg) = report_for(&name, &report) else {
        return (StatusCode::NOT_FOUND, "unknown report").into_response();
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let want_pdf = params.get("format").map(|f| f == "pdf").unwrap_or(false);
    match backend.db.find_one_secured(&model, &ctx, &backend.acls(), &backend.rules(), id).await {
        Ok(Some(rec)) => {
            let html = (reg.func)(&rec);
            if !want_pdf {
                return axum::response::Html(html).into_response();
            }
            // PDF is rendered by rasterizing the same HTML — only if a rasterizer is configured.
            // ponytail: re-renders per request; a content-addressed ir.attachment cache lands with a
            // concrete (slow) rasterizer, where the dedup actually pays for itself.
            match &backend.rasterizer {
                None => (StatusCode::NOT_IMPLEMENTED, "PDF rendering is not configured").into_response(),
                Some(r) => match r.render_pdf(&html) {
                    Ok(bytes) => pdf_response(bytes, reg.title, id),
                    Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("PDF render failed: {e}")).into_response(),
                },
            }
        }
        Ok(None) => (StatusCode::NOT_FOUND, "not found or not permitted").into_response(),
        Err(DbError::AccessDenied { .. }) => (StatusCode::FORBIDDEN, "access denied").into_response(),
        Err(e) => internal_error("report", e),
    }
}

/// A downloadable PDF response with a safe filename derived from the report title + record id. The
/// title is a trusted compile-time string; non-alphanumerics are still squashed for the header.
fn pdf_response(bytes: Vec<u8>, title: &str, id: i64) -> Response {
    let safe: String = title.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/pdf".to_string()),
            (axum::http::header::CONTENT_DISPOSITION, format!("inline; filename=\"{safe}-{id}.pdf\"")),
        ],
        bytes,
    )
        .into_response()
}

/// Opens a wizard (transient model): computes its server-side defaults from the open context, creates
/// the scratchpad row under the caller, and returns it for the frontend to contract-render. The model
/// must be `register_wizard!`-bound (else 400). Authorization is the normal create ACL on the model.
async fn open_wizard_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Json2>,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let Some(wizard) = wizard_for(&name) else {
        return (StatusCode::BAD_REQUEST, "not a wizard model").into_response();
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    // The open context (Odoo's active_model / active_id / active_ids) — all optional.
    let wctx = WizardContext {
        active_model: body.get("active_model").and_then(|v| v.as_str()).map(str::to_string),
        active_id: body.get("active_id").and_then(Json2::as_i64),
        active_ids: body
            .get("active_ids")
            .and_then(Json2::as_array)
            .map(|a| a.iter().filter_map(Json2::as_i64).collect())
            .unwrap_or_default(),
    };
    let seed: serde_json::Map<String, Json2> = (wizard.default_get)(&wctx)
        .into_iter()
        .map(|(k, v)| (k.to_string(), value_to_json(&v)))
        .collect();
    let id = match backend.db.insert_secured(&model, &ctx, &backend.acls(), &backend.rules(), &seed).await {
        Ok(id) => id,
        Err(e) => return write_error("open", e),
    };
    match backend.db.find_one_secured(&model, &ctx, &backend.acls(), &backend.rules(), id).await {
        Ok(Some(rec)) => {
            json_status(StatusCode::CREATED, serde_json::to_string(&rec).unwrap_or_else(|_| "{}".to_string()))
        }
        Ok(None) => (StatusCode::NOT_FOUND, "not found or not permitted").into_response(),
        Err(e) => write_error("open", e),
    }
}

async fn delete_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let model = match resolve_model(&state, &name) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    match backend.db.delete_secured(&model, &ctx, &backend.acls(), &backend.rules(), id).await {
        Ok(0) => (StatusCode::NOT_FOUND, "not found or not permitted").into_response(),
        Ok(n) => json_status(StatusCode::OK, format!("{{\"deleted\": {n}}}")),
        Err(e) => write_error("delete", e),
    }
}

fn str_field<'a>(body: &'a Json2, key: &str) -> Option<&'a str> {
    body.get(key).and_then(|v| v.as_str())
}

/// The target user id from a request body's optional `user_id` (defaulting to `default`, the caller).
/// A present-but-non-integer value is a 400 — never silently fall back to the caller.
fn body_user_id(body: &Json2, default: i64) -> Result<i64, Response> {
    match body.get("user_id") {
        None | Some(Json2::Null) => Ok(default),
        Some(v) => v
            .as_i64()
            .ok_or_else(|| (StatusCode::BAD_REQUEST, "user_id must be an integer").into_response()),
    }
}

/// Authorizes acting on another user's behalf: only the `admin` group may target a `uid` other than
/// the caller's own. Prevents a normal user from force-(un)subscribing arbitrary users (IDOR).
fn ensure_self_or_admin(ctx: &Ctx, uid: i64) -> Result<(), Response> {
    if uid == ctx.uid || ctx.is_member("admin") {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "cannot manage another user's subscription").into_response())
    }
}

/// Resolves a served model by exact name, or a 500 (the owning module isn't installed/served). Used
/// for the mail subsystem's internal models (mail.message, mail.activity) the chatter endpoints act on.
fn served_model<'a>(state: &'a AppState, name: &str) -> Result<&'a ResolvedModel, Response> {
    state
        .models
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| internal_error("mail", format!("{name} not served (mail module not installed)")))
}

/// The shared opening of every chatter/activity/follower endpoint: take the data backend,
/// authenticate the caller, gate on READ access to the host record `(name, id)`, and resolve the
/// served mail model to act on. Returns `(backend, ctx, model)` or the `Response` to return. One
/// place for the access decision, so no handler can forget the host-read gate.
async fn chatter_setup<'a>(
    state: &'a AppState,
    headers: &HeaderMap,
    name: &str,
    id: i64,
    model_name: &str,
) -> Result<(&'a DataBackend, Ctx, &'a ResolvedModel), Response> {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = authenticate(backend, headers).await?;
    chatter_gate(state, backend, &ctx, name, id).await?;
    let model = served_model(state, model_name)?;
    Ok((backend, ctx, model))
}

/// Gates a chatter request: the host model must opt into mail, AND the caller must be able to READ
/// the host record — you cannot see or post to the thread of a record you cannot read. The single
/// access chokepoint for both thread endpoints (reuses the secured read path, no bespoke check).
async fn chatter_gate(
    state: &AppState,
    backend: &DataBackend,
    ctx: &Ctx,
    name: &str,
    id: i64,
) -> Result<(), Response> {
    let host = resolve_model(state, name)?;
    if !is_mailed(name) {
        return Err((StatusCode::BAD_REQUEST, format!("model '{name}' has no mail thread")).into_response());
    }
    match backend.db.find_one_secured(&host, ctx, &backend.acls(), &backend.rules(), id).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err((StatusCode::NOT_FOUND, "not found or not permitted").into_response()),
        Err(DbError::AccessDenied { .. }) => Err((StatusCode::FORBIDDEN, "access denied").into_response()),
        Err(e) => Err(internal_error("chatter-gate", e)),
    }
}

/// The polymorphic filter for one record's thread: `res_model = name AND res_id = id`.
fn thread_filter(name: &str, id: i64) -> Domain {
    Domain::Cond(Condition { field: "res_model".into(), op: Operator::Eq, value: Value::Str(name.to_string()) })
        .and(Domain::Cond(Condition { field: "res_id".into(), op: Operator::Eq, value: Value::Int(id) }))
}

/// Strips a header-unsafe string down to printable ASCII (no control chars, no `"`), or a fallback —
/// so a stored filename / mimetype cannot inject into a response header on download.
fn header_safe(s: &str, fallback: &str) -> String {
    let cleaned: String =
        s.chars().filter(|c| c.is_ascii() && !c.is_ascii_control() && *c != '"').collect();
    if cleaned.trim().is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

/// Gates an attachment request: the host model must be served and the host record visible to the
/// caller (read). For a mutation (`write`), the caller must also hold Write on the host model — you
/// modify a record's attachment set only if you can modify the record. The single access chokepoint.
async fn attachment_gate(
    state: &AppState,
    backend: &DataBackend,
    ctx: &Ctx,
    name: &str,
    id: i64,
    write: bool,
) -> Result<(), Response> {
    let host = resolve_model(state, name)?;
    if write && !check_access(Operation::Write, name, ctx, &backend.acls()) {
        return Err((StatusCode::FORBIDDEN, "access denied").into_response());
    }
    match backend.db.find_one_secured(&host, ctx, &backend.acls(), &backend.rules(), id).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err((StatusCode::NOT_FOUND, "not found or not permitted").into_response()),
        Err(DbError::AccessDenied { .. }) => Err((StatusCode::FORBIDDEN, "access denied").into_response()),
        Err(e) => Err(internal_error("attachment-gate", e)),
    }
}

/// Shared opening of the host-anchored attachment endpoints: authenticate, gate on the host record,
/// resolve the served `ir.attachment` model. Returns `(backend, ctx, attachment_model)`.
async fn attachment_setup<'a>(
    state: &'a AppState,
    headers: &HeaderMap,
    name: &str,
    id: i64,
    write: bool,
) -> Result<(&'a DataBackend, Ctx, &'a ResolvedModel), Response> {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = authenticate(backend, headers).await?;
    attachment_gate(state, backend, &ctx, name, id, write).await?;
    let model = served_model(state, "ir.attachment")?;
    Ok((backend, ctx, model))
}

/// `GET /api/:name/:id/attachments` — list a record's attachment metadata (no bytes). Host read gate.
async fn list_attachments_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let (backend, ctx, att) = match attachment_setup(&state, &headers, &name, id, false).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    // The gate IS the access decision; read the (admin-only) attachment model elevated.
    let su = ctx.sudo();
    match backend.db.find_secured(att, &su, &backend.acls(), &backend.rules(), Some(&thread_filter(&name, id))).await {
        Ok(rows) => json_response(serde_json::json!({ "data": rows }).to_string()),
        Err(e) => write_error("attachments", e),
    }
}

/// `POST /api/:name/:id/attachments` — upload a file onto a record. Host WRITE gate. The raw body is
/// the bytes; `X-Filename` and `Content-Type` headers carry the name and mimetype.
async fn upload_attachment_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (backend, ctx, att) = match attachment_setup(&state, &headers, &name, id, true).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty upload").into_response();
    }
    let filename = headers.get("x-filename").and_then(|v| v.to_str().ok()).unwrap_or("file").to_string();
    let mimetype = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_string();
    let sha = sha256_hex(&body);
    // Store the bytes (content-addressed, verified, deduplicated) BEFORE recording the metadata.
    if let Err(e) = backend.blobs.put(&sha, &body).await {
        return internal_error("attachment-put", e);
    }
    let su = ctx.sudo();
    let payload = serde_json::json!({
        "name": filename,
        "res_model": name,
        "res_id": id,
        "mimetype": mimetype,
        "file_size": body.len() as i64,
        "checksum": sha,
    });
    match backend.db.insert_secured(att, &su, &backend.acls(), &backend.rules(), payload.as_object().unwrap()).await {
        Ok(aid) => json_status(
            StatusCode::CREATED,
            serde_json::json!({ "id": aid, "name": filename, "mimetype": mimetype, "file_size": body.len(), "checksum": sha }).to_string(),
        ),
        Err(e) => write_error("attachment", e),
    }
}

/// `GET /api/attachment/:aid/content` — stream an attachment's bytes. Gated by READ on its HOST record.
async fn download_attachment_handler(
    State(state): State<AppState>,
    Path(aid): Path<i64>,
    headers: HeaderMap,
) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let att = match served_model(&state, "ir.attachment") {
        Ok(m) => m,
        Err(r) => return r,
    };
    // Read the attachment row elevated, then gate on READ of the host record it is attached to.
    let su = ctx.sudo();
    let row = match backend.db.find_one_secured(att, &su, &backend.acls(), &backend.rules(), aid).await {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::NOT_FOUND, "attachment not found").into_response(),
        Err(e) => return write_error("attachment", e),
    };
    let res_model = row.get("res_model").and_then(|v| v.as_str()).unwrap_or("");
    let res_id = row.get("res_id").and_then(|v| v.as_i64()).unwrap_or(0);
    if let Err(r) = attachment_gate(&state, backend, &ctx, res_model, res_id, false).await {
        return r;
    }
    let checksum = row.get("checksum").and_then(|v| v.as_str()).unwrap_or("");
    let mimetype = header_safe(row.get("mimetype").and_then(|v| v.as_str()).unwrap_or(""), "application/octet-stream");
    let filename = header_safe(row.get("name").and_then(|v| v.as_str()).unwrap_or(""), "file");
    // Serve INLINE only for a safe allowlist (images / pdf). Anything else — notably a user-uploaded
    // text/html blob — is forced to download (`attachment`) with `nosniff`, so it can never execute as
    // script in the app's origin. The uploader controls the mimetype, so inline-by-default is unsafe.
    let disposition = match mimetype.as_str() {
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "application/pdf" => "inline",
        _ => "attachment",
    };
    match backend.blobs.get(checksum).await {
        Ok(bytes) => (
            [
                ("content-type", mimetype),
                ("content-disposition", format!("{disposition}; filename=\"{filename}\"")),
                ("x-content-type-options", "nosniff".to_string()),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => internal_error("attachment-get", e),
    }
}

/// `DELETE /api/attachment/:aid` — remove an attachment row. Gated by WRITE on its HOST record. The
/// blob is left for GC (it may be shared by another attachment via content-address dedup).
async fn delete_attachment_handler(
    State(state): State<AppState>,
    Path(aid): Path<i64>,
    headers: HeaderMap,
) -> Response {
    let backend = state.data.as_ref().expect("data backend present on data routes");
    let ctx = match authenticate(backend, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let att = match served_model(&state, "ir.attachment") {
        Ok(m) => m,
        Err(r) => return r,
    };
    let su = ctx.sudo();
    let row = match backend.db.find_one_secured(att, &su, &backend.acls(), &backend.rules(), aid).await {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::NOT_FOUND, "attachment not found").into_response(),
        Err(e) => return write_error("attachment", e),
    };
    let res_model = row.get("res_model").and_then(|v| v.as_str()).unwrap_or("");
    let res_id = row.get("res_id").and_then(|v| v.as_i64()).unwrap_or(0);
    if let Err(r) = attachment_gate(&state, backend, &ctx, res_model, res_id, true).await {
        return r;
    }
    match backend.db.delete_secured(att, &su, &backend.acls(), &backend.rules(), aid).await {
        Ok(0) => (StatusCode::NOT_FOUND, "attachment not found").into_response(),
        Ok(_) => json_response(serde_json::json!({ "deleted": 1 }).to_string()),
        Err(e) => write_error("attachment", e),
    }
}

/// `GET /api/:name/:id/messages` — the record's message thread, oldest first (by id).
async fn messages_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let (backend, ctx, mail) = match chatter_setup(&state, &headers, &name, id, "mail.message").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    // The gate (host readable) IS the access decision; read the thread elevated so the framework
    // isn't forced to grant users a blanket read ACL on mail.message (which would leak all threads).
    let su = ctx.sudo();
    let filter = thread_filter(&name, id);
    match backend.db.find_secured(mail, &su, &backend.acls(), &backend.rules(), Some(&filter)).await {
        Ok(rows) => {
            // Embed each message's field-change audit (mail.tracking) so a notification message
            // carries its old→new diffs — one thread payload, comments and audit uniform.
            let ids: Vec<i64> = rows.iter().filter_map(|m| m.get("id").and_then(|v| v.as_i64())).collect();
            // D6 redaction lives in the DB layer (tracking_for_secured): tracking of fields the caller
            // may not read is dropped there, beside the secured record read, so the audit trail can
            // never become a second unguarded read channel for group-restricted values.
            let tracking = match backend.db.tracking_for_secured(&name, &ctx, &ids).await {
                Ok(t) => t,
                Err(e) => {
                    // A DB error here must not be hidden as "no audit"; log it (the messages still return).
                    tracing::error!("kigumi-server messages tracking enrichment failed: {e:?}");
                    Vec::new()
                }
            };
            let mut by_msg: HashMap<i64, Vec<Json2>> = HashMap::new();
            for t in tracking {
                if let Some(mid) = t.get("message_id").and_then(|v| v.as_i64()) {
                    by_msg.entry(mid).or_default().push(t);
                }
            }
            let enriched: Vec<Json2> = rows
                .into_iter()
                .map(|mut m| {
                    if let Some(mid) = m.get("id").and_then(|v| v.as_i64()) {
                        if let Some(obj) = m.as_object_mut() {
                            obj.insert("tracking".into(), Json2::Array(by_msg.remove(&mid).unwrap_or_default()));
                        }
                    }
                    m
                })
                .collect();
            json_response(serde_json::json!({ "data": enriched }).to_string())
        }
        Err(DbError::AccessDenied { .. }) => (StatusCode::FORBIDDEN, "access denied").into_response(),
        Err(e) => internal_error("messages", e),
    }
}

/// `POST /api/:name/:id/message` — post a comment (or log note) to the record's thread. The author
/// is the authenticated caller; the timestamp is the DB clock. `notification` type is reserved for
/// system tracking entries (a later slice), so only `comment`/`note` are accepted here.
async fn post_message_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
    Json(body): Json<Json2>,
) -> Response {
    let (backend, ctx, mail) = match chatter_setup(&state, &headers, &name, id, "mail.message").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let text = match str_field(&body, "body") {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => return (StatusCode::BAD_REQUEST, "message body is required").into_response(),
    };
    let mtype = match str_field(&body, "message_type") {
        None | Some("comment") => "comment",
        Some("note") => "note",
        Some(other) => return (StatusCode::BAD_REQUEST, format!("invalid message_type '{other}'")).into_response(),
    };
    let now = match backend.db.now().await {
        Ok(t) => t,
        Err(e) => return internal_error("clock", e),
    };
    let mut values = serde_json::Map::new();
    values.insert("res_model".into(), Json2::String(name.clone()));
    values.insert("res_id".into(), Json2::Number(id.into()));
    values.insert("author_id".into(), Json2::Number(ctx.uid.into()));
    values.insert("body".into(), Json2::String(text));
    values.insert("message_type".into(), Json2::String(mtype.to_string()));
    values.insert("date".into(), Json2::String(now));
    // The gate authorized this post; author stays the real caller (ctx.uid above), but the insert
    // runs elevated so users need no create ACL on mail.message (see the read path / mail ACLs).
    let su = ctx.sudo();
    match backend.db.insert_secured(mail, &su, &backend.acls(), &backend.rules(), &values).await {
        Ok(mid) => json_status(StatusCode::CREATED, format!("{{\"id\": {mid}}}")),
        Err(e) => write_error("message", e),
    }
}

/// An activity's state DERIVED from its deadline vs the DB's current date (ISO strings compare
/// lexically). The single place this rule lives (Odoo writes it three times).
fn activity_state(deadline: Option<&str>, today: &str) -> &'static str {
    match deadline {
        Some(d) if d < today => "overdue",
        Some(d) if d == today => "today",
        _ => "planned", // future deadline or none
    }
}

/// `GET /api/:name/:id/activities` — open to-dos on the record, each with a derived state.
async fn activities_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let (backend, ctx, act) = match chatter_setup(&state, &headers, &name, id, "mail.activity").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let today = match backend.db.today().await {
        Ok(t) => t,
        Err(e) => return internal_error("clock", e),
    };
    let su = ctx.sudo();
    let filter = thread_filter(&name, id).and(Domain::Cond(Condition {
        field: "active".into(),
        op: Operator::Eq,
        value: Value::Bool(true),
    }));
    match backend.db.find_secured(act, &su, &backend.acls(), &backend.rules(), Some(&filter)).await {
        Ok(rows) => {
            let enriched: Vec<Json2> = rows
                .into_iter()
                .map(|mut a| {
                    let deadline = a.get("date_deadline").and_then(|v| v.as_str()).map(str::to_string);
                    if let Some(obj) = a.as_object_mut() {
                        obj.insert("state".into(), Json2::String(activity_state(deadline.as_deref(), &today).to_string()));
                    }
                    a
                })
                .collect();
            json_response(serde_json::json!({ "data": enriched }).to_string())
        }
        Err(DbError::AccessDenied { .. }) => (StatusCode::FORBIDDEN, "access denied").into_response(),
        Err(e) => internal_error("activities", e),
    }
}

/// `POST /api/:name/:id/activity` — schedule a to-do `{summary, date_deadline?, user_id?}`. The
/// assignee defaults to the caller. Gated on read access to the host.
async fn schedule_activity_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
    Json(body): Json<Json2>,
) -> Response {
    let (backend, ctx, act) = match chatter_setup(&state, &headers, &name, id, "mail.activity").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let summary = match str_field(&body, "summary") {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => return (StatusCode::BAD_REQUEST, "activity summary is required").into_response(),
    };
    // Assignee defaults to the caller; a present-but-non-integer user_id is a client error.
    let assignee = match body_user_id(&body, ctx.uid) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let mut values = serde_json::Map::new();
    values.insert("res_model".into(), Json2::String(name.clone()));
    values.insert("res_id".into(), Json2::Number(id.into()));
    values.insert("summary".into(), Json2::String(summary));
    // An empty/whitespace deadline means "no deadline" (a planned to-do), not a 400.
    if let Some(d) = str_field(&body, "date_deadline").filter(|d| !d.trim().is_empty()) {
        values.insert("date_deadline".into(), Json2::String(d.to_string()));
    }
    values.insert("user_id".into(), Json2::Number(assignee.into()));
    values.insert("active".into(), Json2::Bool(true));
    let su = ctx.sudo();
    match backend.db.insert_secured(act, &su, &backend.acls(), &backend.rules(), &values).await {
        Ok(aid) => json_status(StatusCode::CREATED, format!("{{\"id\": {aid}}}")),
        Err(e) => write_error("activity", e),
    }
}

/// `POST /api/:name/:id/activities/:aid/done` — mark a to-do done (`active` → false). The activity
/// must belong to the gated host record (you can't close another record's to-do via a host you can read).
async fn activity_done_handler(
    State(state): State<AppState>,
    Path((name, id, aid)): Path<(String, i64, i64)>,
    headers: HeaderMap,
) -> Response {
    let (backend, ctx, act) = match chatter_setup(&state, &headers, &name, id, "mail.activity").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let su = ctx.sudo();
    // The activity must exist AND belong to this host record.
    let belongs = match backend.db.find_one_secured(act, &su, &backend.acls(), &backend.rules(), aid).await {
        Ok(Some(a)) => {
            a.get("res_model").and_then(|v| v.as_str()) == Some(name.as_str())
                && a.get("res_id").and_then(|v| v.as_i64()) == Some(id)
        }
        Ok(None) => false,
        Err(e) => return internal_error("activity-done", e),
    };
    if !belongs {
        return (StatusCode::NOT_FOUND, "activity not found on this record").into_response();
    }
    let mut values = serde_json::Map::new();
    values.insert("active".into(), Json2::Bool(false));
    match backend.db.update_secured(act, &su, &backend.acls(), &backend.rules(), aid, &values).await {
        // 0 rows = the activity vanished between the belongs-check and the update (e.g. the host was
        // concurrently deleted, cascading the cleanup). Report it truthfully, don't claim success.
        Ok(0) => (StatusCode::NOT_FOUND, "activity not found on this record").into_response(),
        Ok(_) => json_response("{\"ok\":true}".to_string()),
        Err(e) => write_error("activity-done", e),
    }
}

/// The polymorphic filter for one record's followers, optionally narrowed to a single `user_id`.
fn follower_filter(name: &str, id: i64, user_id: Option<i64>) -> Domain {
    let mut d = thread_filter(name, id);
    if let Some(uid) = user_id {
        d = d.and(Domain::Cond(Condition { field: "user_id".into(), op: Operator::Eq, value: Value::Int(uid) }));
    }
    d
}

/// `GET /api/:name/:id/followers` — the users subscribed to the record's thread.
async fn followers_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let (backend, ctx, foll) = match chatter_setup(&state, &headers, &name, id, "mail.follower").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let su = ctx.sudo();
    let filter = follower_filter(&name, id, None);
    match backend.db.find_secured(foll, &su, &backend.acls(), &backend.rules(), Some(&filter)).await {
        Ok(rows) => json_response(serde_json::json!({ "data": rows }).to_string()),
        Err(DbError::AccessDenied { .. }) => (StatusCode::FORBIDDEN, "access denied").into_response(),
        Err(e) => internal_error("followers", e),
    }
}

/// `POST /api/:name/:id/follow` — subscribe a user (default the caller) to the record. Idempotent:
/// re-following an already-followed record is a success, not a conflict (the composite unique index
/// guarantees one subscription per user per record).
async fn follow_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
    Json(body): Json<Json2>,
) -> Response {
    let (backend, ctx, foll) = match chatter_setup(&state, &headers, &name, id, "mail.follower").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let uid = match body_user_id(&body, ctx.uid) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if let Err(r) = ensure_self_or_admin(&ctx, uid) {
        return r;
    }
    let mut values = serde_json::Map::new();
    values.insert("res_model".into(), Json2::String(name.clone()));
    values.insert("res_id".into(), Json2::Number(id.into()));
    values.insert("user_id".into(), Json2::Number(uid.into()));
    let su = ctx.sudo();
    match backend.db.insert_secured(foll, &su, &backend.acls(), &backend.rules(), &values).await {
        Ok(_) => json_status(StatusCode::CREATED, "{\"ok\":true}".to_string()),
        // Already following — the unique index rejected the duplicate. Idempotent success.
        Err(DbError::Conflict(_)) => json_response("{\"ok\":true,\"already\":true}".to_string()),
        Err(e) => write_error("follow", e),
    }
}

/// `POST /api/:name/:id/unfollow` — unsubscribe a user (default the caller). Idempotent: unfollowing
/// when not a follower is a success (nothing to remove).
async fn unfollow_handler(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    headers: HeaderMap,
    Json(body): Json<Json2>,
) -> Response {
    let (backend, ctx, foll) = match chatter_setup(&state, &headers, &name, id, "mail.follower").await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let uid = match body_user_id(&body, ctx.uid) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if let Err(r) = ensure_self_or_admin(&ctx, uid) {
        return r;
    }
    let su = ctx.sudo();
    let filter = follower_filter(&name, id, Some(uid));
    let ids = match backend.db.find_ids_secured(foll, &su, &backend.acls(), &backend.rules(), Some(&filter)).await {
        Ok(v) => v,
        Err(e) => return internal_error("unfollow", e),
    };
    for fid in ids {
        if let Err(e) = backend.db.delete_secured(foll, &su, &backend.acls(), &backend.rules(), fid).await {
            return write_error("unfollow", e);
        }
    }
    json_response("{\"ok\":true}".to_string())
}

/// Issues an access + (stored) refresh token pair for `uid` with `groups` and company `scope`
/// (active, allowed). The access token bakes in the scope so each request verifies into a
/// company-scoped Ctx with no extra DB round-trip.
/// Mints an access+refresh pair for a session (and stores the refresh jti). Returns the raw tokens, or
/// an error `Response` on a signing/store failure. Callers wrap the result differently: password login
/// returns them as JSON, OIDC hands them to the browser in a redirect fragment.
async fn mint_tokens(
    backend: &DataBackend,
    uid: i64,
    groups: Vec<String>,
    scope: (Option<i64>, Vec<i64>),
) -> Result<(String, String), Response> {
    let (company, companies) = scope;
    let access = backend
        .auth
        .issue_access(uid, groups, company, companies, ACCESS_TTL)
        .map_err(|_| internal_error("token", "issue access"))?;
    let jti = new_jti();
    backend
        .db
        .store_refresh(&jti, uid, REFRESH_TTL as i64)
        .await
        .map_err(|e| internal_error("refresh-store", e))?;
    let refresh = backend
        .auth
        .issue_refresh(uid, &jti, REFRESH_TTL)
        .map_err(|_| internal_error("token", "issue refresh"))?;
    Ok((access, refresh))
}

async fn issue_token_pair(
    backend: &DataBackend,
    uid: i64,
    groups: Vec<String>,
    scope: (Option<i64>, Vec<i64>),
) -> Response {
    match mint_tokens(backend, uid, groups, scope).await {
        Ok((access, refresh)) => {
            let body = serde_json::json!({
                "access_token": access,
                "refresh_token": refresh,
                "token_type": "Bearer",
                "expires_in": ACCESS_TTL,
            });
            json_status(StatusCode::OK, body.to_string())
        }
        Err(resp) => resp,
    }
}

/// The cookie that pins an in-flight OIDC login to the browser that started it (the login-CSRF defense).
const OIDC_STATE_COOKIE: &str = "kigumi_oidc_state";

/// The value of `name` in a request's `Cookie` header, if present.
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(axum::http::header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

/// `GET /auth/oidc/start` — begin SSO by redirecting the browser to the IdP (with PKCE + nonce + state).
/// 404 when `[oidc]` is not configured.
async fn oidc_start_handler(State(state): State<AppState>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on auth routes");
    let Some(oidc) = backend.oidc.as_ref() else {
        return (StatusCode::NOT_FOUND, "OIDC is not configured").into_response();
    };
    match oidc.authorize(&backend.db).await {
        Ok((url, csrf_state)) => {
            // Pin the flow to THIS browser: a cookie the callback must echo. An attacker cannot set this
            // cookie on a victim's browser, so a forged callback (the attacker's own code + state) fails
            // the echo check — the login-CSRF / session-fixation defense. SameSite=Lax so the cookie
            // still rides the top-level GET redirect back from the IdP.
            let cookie = format!(
                "{OIDC_STATE_COOKIE}={csrf_state}; Max-Age=600; Path=/auth/oidc; HttpOnly; Secure; SameSite=Lax"
            );
            ([(axum::http::header::SET_COOKIE, cookie)], Redirect::to(&url)).into_response()
        }
        Err(e) => oidc_error_response("oidc-start", e),
    }
}

/// `GET /auth/oidc/callback` — complete SSO: verify the IdP response, provision or link the user by
/// verified email, mint a session, and redirect to the app with the tokens in the URL fragment.
async fn oidc_callback_handler(State(state): State<AppState>, headers: HeaderMap, Query(params): Query<HashMap<String, String>>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on auth routes");
    let Some(oidc) = backend.oidc.as_ref() else {
        return (StatusCode::NOT_FOUND, "OIDC is not configured").into_response();
    };
    // An IdP error redirect carries `?error=...`; surface it rather than a confusing "missing code".
    if let Some(err) = params.get("error") {
        return (StatusCode::BAD_REQUEST, format!("OIDC provider returned an error: {err}")).into_response();
    }
    let (Some(code), Some(st)) = (params.get("code"), params.get("state")) else {
        return (StatusCode::BAD_REQUEST, "missing code or state").into_response();
    };
    // Login-CSRF defense: the state MUST match the cookie set on THIS browser at /start, checked before
    // the one-shot flow lookup. A callback replayed into a victim's browser (which never received the
    // cookie) is rejected here, so an attacker cannot fixate the victim onto the attacker's identity.
    if cookie_value(&headers, OIDC_STATE_COOKIE).as_deref() != Some(st.as_str()) {
        return (StatusCode::BAD_REQUEST, "invalid or expired login state").into_response();
    }
    let email = match oidc.exchange_and_verify(&backend.db, code, st).await {
        Ok(e) => e,
        Err(e) => return oidc_error_response("oidc-callback", e),
    };
    let user = match backend.db.find_or_create_oidc_user(&email).await {
        Ok(u) => u,
        Err(e) => return internal_error("oidc-provision", e),
    };
    let scope = (user.company_id, user.company_ids);
    match mint_tokens(backend, user.id, user.groups, scope).await {
        Ok((access, refresh)) => {
            // Tokens go in the FRAGMENT: a browser never sends it to a server and it is absent from the
            // Referer, so it stays client-side for the SPA to read from location.hash and then clear.
            // Access/refresh are compact JWTs (URL-safe charset), so no percent-encoding is needed.
            let sep = if oidc.post_login_url.contains('#') { '&' } else { '#' };
            let url = format!(
                "{}{sep}access_token={access}&refresh_token={refresh}&token_type=Bearer&expires_in={ACCESS_TTL}",
                oidc.post_login_url,
            );
            // Clear the one-time state cookie now that the flow is complete.
            let clear = format!("{OIDC_STATE_COOKIE}=; Max-Age=0; Path=/auth/oidc; HttpOnly; Secure; SameSite=Lax");
            ([(axum::http::header::SET_COOKIE, clear)], Redirect::to(&url)).into_response()
        }
        Err(resp) => resp,
    }
}

/// Maps an [`OidcError`] to a client response, logging the upstream detail server-side (via tracing)
/// and never leaking it to the caller.
fn oidc_error_response(context: &str, e: OidcError) -> Response {
    match e {
        OidcError::Db(db) => internal_error(context, db),
        OidcError::Discovery(d) => {
            tracing::error!("kigumi-server {context} oidc discovery: {d}");
            (StatusCode::BAD_GATEWAY, "SSO provider unreachable").into_response()
        }
        OidcError::Exchange(d) => {
            tracing::error!("kigumi-server {context} oidc exchange: {d}");
            (StatusCode::BAD_GATEWAY, "SSO token exchange failed").into_response()
        }
        OidcError::Verify(d) => {
            tracing::warn!("kigumi-server {context} oidc verify: {d}");
            (StatusCode::UNAUTHORIZED, "SSO token verification failed").into_response()
        }
        OidcError::InvalidState => (StatusCode::BAD_REQUEST, "invalid or expired login state").into_response(),
        OidcError::NoEmail => (StatusCode::BAD_REQUEST, "the provider returned no email").into_response(),
        OidcError::UnverifiedEmail => {
            (StatusCode::FORBIDDEN, "the provider has not verified this email").into_response()
        }
    }
}

/// A constant valid argon2 hash, verified against on the unknown-user path so login spends the
/// same argon2 time whether or not the account exists (defeats username enumeration via timing).
fn dummy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_password("kigumi-timing-equalizer").expect("dummy hash"))
}

async fn login_handler(State(state): State<AppState>, Json(body): Json<Json2>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on auth routes");
    let (login, password) = match (str_field(&body, "login"), str_field(&body, "password")) {
        (Some(l), Some(p)) => (l, p),
        _ => return (StatusCode::BAD_REQUEST, "login and password required").into_response(),
    };
    let user = match backend.db.find_user(login).await {
        Ok(u) => u,
        Err(e) => return internal_error("login", e),
    };
    // Always run argon2 (against a dummy hash if the user is unknown) so timing — and the
    // 401 body — are identical for unknown-user and wrong-password (no user enumeration).
    // Throttled: under a verification flood the process sheds (503) rather than thrashing.
    let hash = user.as_ref().map(|u| u.password_hash.as_str()).unwrap_or_else(|| dummy_hash());
    let Some(ok) = kigumi_auth::verify_password_throttled(password, hash) else {
        return (StatusCode::SERVICE_UNAVAILABLE, "authentication is busy, retry shortly").into_response();
    };
    match user {
        Some(u) if ok => {
            let scope = (u.company_id, u.company_ids);
            issue_token_pair(backend, u.id, u.groups, scope).await
        }
        _ => (StatusCode::UNAUTHORIZED, "invalid credentials").into_response(),
    }
}

async fn refresh_handler(State(state): State<AppState>, Json(body): Json<Json2>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on auth routes");
    let token = match str_field(&body, "refresh_token") {
        Some(t) => t,
        None => return (StatusCode::BAD_REQUEST, "refresh_token required").into_response(),
    };
    let claims = match backend.auth.verify_refresh(token) {
        Ok(c) => c,
        Err(_) => return (StatusCode::UNAUTHORIZED, "invalid refresh token").into_response(),
    };
    // Atomically claim (revoke) the presented token. A concurrent replay claims zero rows and is
    // rejected → no double-spend. The token also must belong to the uid it claims.
    match backend.db.claim_refresh(&claims.jti).await {
        Ok(Some(uid)) if uid == claims.uid => {
            let groups = match backend.db.user_groups(uid).await {
                Ok(g) => g,
                Err(e) => return internal_error("groups", e),
            };
            // Re-read company scope too, so reassignments take effect on refresh (like groups).
            let scope = match backend.db.user_scope(uid).await {
                Ok(s) => s,
                Err(e) => return internal_error("scope", e),
            };
            issue_token_pair(backend, uid, groups, scope).await
        }
        Ok(_) => (StatusCode::UNAUTHORIZED, "invalid refresh token").into_response(),
        Err(e) => internal_error("refresh", e),
    }
}

async fn logout_handler(State(state): State<AppState>, Json(body): Json<Json2>) -> Response {
    let backend = state.data.as_ref().expect("data backend present on auth routes");
    // Always 204 — never reveal whether the token was valid.
    if let Some(token) = str_field(&body, "refresh_token") {
        if let Ok(claims) = backend.auth.verify_refresh(token) {
            let _ = backend.db.revoke_refresh(&claims.jti).await;
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use kigumi_core::{resolve, FieldDef, FieldKind, ModelDescriptor};
    use tower::ServiceExt;

    static M: ModelDescriptor = ModelDescriptor {
        name: "sale.order",
        table: "sale_order",
        fields: &[FieldDef {
            name: "state", label: "State",
            kind: FieldKind::Selection(&[("draft", "Draft"), ("done", "Done")]),
            required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None }],
    };

    fn models() -> Vec<ResolvedModel> {
        vec![resolve(&M, &[]).unwrap()]
    }

    async fn fetch(uri: &str) -> (StatusCode, String) {
        let resp = router(models())
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn serves_openapi_spec() {
        let (status, body) = fetch("/openapi.json").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"openapi\""));
        assert!(body.contains("sale.order"));
    }

    #[tokio::test]
    async fn serves_model_list_and_view() {
        let (status, body) = fetch("/api/models").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("sale.order"));

        let (status, body) = fetch("/api/sale.order/view").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"model\": \"sale.order\""));

        let (status, _) = fetch("/api/nope/view").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn view_overrides_relabel_hide_and_lock() {
        let contract = "{\"fields\":[\
            {\"name\":\"state\",\"label\":\"State\",\"widget\":\"selection\",\"readonly\":false},\
            {\"name\":\"secret\",\"label\":\"Secret\",\"widget\":\"text\",\"readonly\":false}],\
            \"list\":{\"columns\":[\
            {\"name\":\"state\",\"label\":\"State\",\"widget\":\"selection\"},\
            {\"name\":\"secret\",\"label\":\"Secret\",\"widget\":\"text\"}]}}";
        let overrides = vec![
            ViewOverride {
                model: "sale.order".into(),
                field: "state".into(),
                label: Some("Status".into()),
                widget: None,
                invisible: false,
                readonly: true,
                invisible_when: Some("{\"field\":\"state\",\"op\":\"=\",\"value\":\"done\"}".into()),
                readonly_when: None,
            },
            ViewOverride {
                model: "sale.order".into(),
                field: "secret".into(),
                label: None,
                widget: None,
                invisible: true,
                readonly: false,
                invisible_when: None,
                readonly_when: None,
            },
        ];
        let out = apply_view_overrides(contract, &overrides);
        let v: Json2 = serde_json::from_str(&out).unwrap();
        let fields = v["fields"].as_array().unwrap();
        // 'secret' is hidden from both fields and columns; 'state' is relabeled and locked.
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0]["name"], "state");
        assert_eq!(fields[0]["label"], "Status");
        assert_eq!(fields[0]["readonly"], true);
        // The conditional domain is injected as a parsed AST the frontend can evaluate.
        assert_eq!(fields[0]["invisible_when"], serde_json::json!({"field":"state","op":"=","value":"done"}));
        let cols = v["list"]["columns"].as_array().unwrap();
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0]["name"], "state");
        assert_eq!(cols[0]["label"], "Status");
        // A no-override contract round-trips unchanged in shape (still parses, still 2 fields).
        let untouched = apply_view_overrides(contract, &[]);
        let v2: Json2 = serde_json::from_str(&untouched).unwrap();
        assert_eq!(v2["fields"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn translations_swap_labels_options_and_columns_for_lang() {
        let contract = "{\"fields\":[\
            {\"name\":\"state\",\"label\":\"State\",\"widget\":\"selection\",\"options\":[\
                {\"value\":\"draft\",\"label\":\"Draft\"},{\"value\":\"done\",\"label\":\"Done\"}]},\
            {\"name\":\"name\",\"label\":\"Name\",\"widget\":\"text\"}],\
            \"list\":{\"columns\":[\
            {\"name\":\"state\",\"label\":\"State\",\"widget\":\"selection\"},\
            {\"name\":\"name\",\"label\":\"Name\",\"widget\":\"text\"}]}}";
        let tr = |field: &str, value: &str, lang: &str, text: &str| Translation {
            model: "sale.order".into(),
            field: field.into(),
            value: value.into(),
            lang: lang.into(),
            text: text.into(),
        };
        let translations = vec![
            tr("state", "", "it", "Stato"),   // field label
            tr("state", "draft", "it", "Bozza"), // one option only
            tr("name", "", "fr", "Nom"),      // a different language
        ];
        let out = apply_translations(contract, &translations, "it");
        let v: Json2 = serde_json::from_str(&out).unwrap();
        let fields = v["fields"].as_array().unwrap();
        assert_eq!(fields[0]["label"], "Stato"); // field label translated
        let opts = fields[0]["options"].as_array().unwrap();
        assert_eq!(opts[0]["label"], "Bozza"); // 'draft' option translated
        assert_eq!(opts[1]["label"], "Done"); // 'done' untranslated -> English fallback
        assert_eq!(fields[1]["label"], "Name"); // 'name' has only an 'fr' entry -> unchanged for 'it'
        // The matching list column is translated too, so form and list agree.
        assert_eq!(v["list"]["columns"].as_array().unwrap()[0]["label"], "Stato");
        // A language with no entries returns the contract unchanged.
        let none = apply_translations(contract, &translations, "de");
        assert_eq!(serde_json::from_str::<Json2>(&none).unwrap()["fields"][0]["label"], "State");
    }

    #[test]
    fn accept_language_takes_primary_subtag() {
        let lang = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert("accept-language", v.parse().unwrap());
            accept_language(&h)
        };
        assert_eq!(lang("it-IT,it;q=0.9,en;q=0.8").as_deref(), Some("it"));
        assert_eq!(lang("FR").as_deref(), Some("fr")); // lowercased
        assert_eq!(lang("  ").as_deref(), None);
        assert_eq!(accept_language(&HeaderMap::new()), None); // header absent
    }
}

#[cfg(test)]
mod query_tests {
    use super::*;
    use kigumi_core::{resolve, FieldDef, ModelDescriptor};

    static M: ModelDescriptor = ModelDescriptor {
        name: "q.model",
        table: "q_model",
        fields: &[
            FieldDef { name: "name", label: "Name", kind: FieldKind::Text, required: true, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
            FieldDef { name: "amount", label: "Amount", kind: FieldKind::Decimal { currency_field: None }, required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
            FieldDef { name: "state", label: "State", kind: FieldKind::Selection(&[("draft", "Draft"), ("done", "Done")]), required: false, stored: true, compute: None, depends: &[], default: None, unique: false, check: None },
        ],
    };
    fn model() -> ResolvedModel {
        resolve(&M, &[]).unwrap()
    }

    #[test]
    fn suffix_split() {
        assert_eq!(split_suffix("amount__gte"), ("amount", "gte"));
        assert_eq!(split_suffix("name"), ("name", "eq")); // bare field defaults to eq
    }

    #[test]
    fn activity_state_derivation() {
        // Derived purely from deadline vs today (ISO strings compare lexically).
        assert_eq!(activity_state(Some("2026-06-16"), "2026-06-17"), "overdue");
        assert_eq!(activity_state(Some("2026-06-17"), "2026-06-17"), "today");
        assert_eq!(activity_state(Some("2026-06-18"), "2026-06-17"), "planned");
        assert_eq!(activity_state(None, "2026-06-17"), "planned"); // no deadline
    }

    #[test]
    fn coerce_by_kind() {
        assert_eq!(coerce(&FieldKind::Integer, "5"), Ok(Value::Int(5)));
        assert_eq!(
            coerce(&FieldKind::Decimal { currency_field: None }, "1.5"),
            Ok(Value::Decimal("1.5".parse::<rust_decimal::Decimal>().unwrap()))
        );
        assert_eq!(coerce(&FieldKind::Bool, "true"), Ok(Value::Bool(true)));
        assert_eq!(coerce(&FieldKind::Text, "x"), Ok(Value::Str("x".to_string())));
        assert!(coerce(&FieldKind::Integer, "nope").is_err());
    }

    #[test]
    fn build_leaf_typed() {
        let m = model();
        let d = build_leaf(&m, "amount", "gte", "100").unwrap();
        assert_eq!(d, Domain::Cond(Condition { field: "amount".into(), op: Operator::Ge, value: Value::Decimal("100".parse().unwrap()) }));
        assert!(build_leaf(&m, "nope", "eq", "1").is_err(), "unknown field rejected");
        assert!(build_leaf(&m, "name", "weird", "1").is_err(), "unknown operator rejected");
    }

    #[test]
    fn parse_query_filter_order_limit() {
        let m = model();
        let mut p = HashMap::new();
        p.insert("state".to_string(), "draft".to_string());
        p.insert("amount__gte".to_string(), "10".to_string());
        p.insert("order".to_string(), "-id".to_string());
        p.insert("limit".to_string(), "5".to_string());
        let (filter, order, limit, offset) = parse_list_query(&m, &p).unwrap();
        assert!(filter.is_some());
        assert_eq!(order, vec![("id".to_string(), true)]);
        assert_eq!(limit, 5);
        assert_eq!(offset, 0);
        // The compiled filter is valid SQL against the model.
        assert!(filter.unwrap().compile(&m).is_ok());
    }

    #[test]
    fn limit_is_clamped() {
        let m = model();
        let mut p = HashMap::new();
        p.insert("limit".to_string(), "100000".to_string());
        let (_, _, limit, _) = parse_list_query(&m, &p).unwrap();
        assert_eq!(limit, MAX_LIMIT);
    }
}
