//! Facade for the Kigumi framework. Application modules depend only on this crate.

// Re-export of inventory so that macros can emit `::kigumi::inventory::submit!`
// without every module having to add the dependency.
pub use kigumi_core::inventory;

/// Everything needed to define a module: `use kigumi::prelude::*;`
pub mod prelude {
    pub use kigumi_core::{
        action_for, check_access, check_compat, compute_fn, compute_stored, computed_fields,
        delegated_fields, external_tables, field_accessible, field_is_readonly, field_required_groups,
        inherits_of, is_mailed, is_transient, json_string, mailed_models, migration_plan, module_closure, module_of,
        check_constraints, has_constraints, has_read_computes, compute_on_read, transient_models,
        record_rule_domain, registered_acls, registered_group_names, registered_model_names,
        registered_rules, related_path, resolve, resolve_all_registered, resolve_module_set,
        resolve_modules, resolve_registered, tracked_fields, validate_depends, Acl, AclRegistration,
        ActionFn, ActionInput, ActionOutcome, ActionRegistration, Children, ComputeFn, ComputeInput,
        ComputeRegistration, ConstraintFn, ConstraintRegistration, Condition, Ctx, DelegatedField, Domain, DomainError, ExternalTable,
        FieldBuilder, FieldDef, FieldExtension, FieldGroupRegistration, FieldKind,
        InheritsRegistration, MailedRegistration, MigrationTarget, Model, ModelDescriptor,
        ModelRegistration, ModuleDep, ModuleManifest, ModuleRegistration, Operation, Operator,
        ReadonlyFieldRegistration, RecordRule, RecordRuleRegistration, RelatedRegistration,
        ResolutionError, ResolvedModel, RuleDomain, Sql, TrackedFieldRegistration, TransientRegistration,
        Value, FRAMEWORK_VERSION,
        wizard_for, WizardContext, WizardDefaultGet, WizardRegistration,
        report_for, reports_for, ReportFn, ReportRegistration,
        view_for, FieldGroup, FieldSlot, FormView, NotebookPage,
    };
    pub use kigumi_macros::{extend, model};
    pub use kigumi_schema::{openapi, to_ddl, to_ui_contract, FieldRule, UiRule};
    // The cross-record service seam (DB-typed, defined in kigumi-db) — registered via register_service!.
    // `DbError` is the service result's error type, so a module needs no direct kigumi-db dependency.
    pub use kigumi_db::{
        ledger_report_for, ledger_report_names, route_for, route_methods, service_for, services_for,
        validate_routes, write_triggers_for, BoxServiceFut, DbError, LedgerReportFn,
        LedgerReportRegistration, RouteFn, RouteInput, RouteMethod, RouteOutput, RouteRegistration,
        ServiceCtx, ServiceFn, ServiceInput, ServiceOutput, ServiceRegistration, WriteTriggerFn,
        WriteTriggerRegistration,
    };
}

/// Marks a field as a `related` field (Odoo `related=`): a non-stored, read-only mirror of the value
/// reached by `path` (e.g. "order_id.currency_id"). Usually emitted by `#[field(related = "...")]`.
/// `kigumi::register_related!("sale.order.line", "order_currency_id", "order_id.currency_id");`
#[macro_export]
macro_rules! register_related {
    ($model:expr, $field:expr, $path:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::RelatedRegistration { model: $model, field: $field, path: $path }
        }
    };
}

/// Registers a compute function by name, so the engine runs it on write for fields declaring it.
/// Use at module top level: `kigumi::register_compute!("compute_total", compute_total);`
#[macro_export]
macro_rules! register_compute {
    ($name:expr, $func:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ComputeRegistration { name: $name, func: $func }
        }
    };
}

/// Registers a cross-record constraint (Odoo `@api.constrains`): `func` runs in the write transaction
/// after the record + its children are written, and returns `Err(msg)` to reject (roll back) the write.
/// `fields` are the triggers (empty = every write); list the WRITTEN fields and One2many field names
/// that drive the invariant, plus any stored computed field it reads (those also trigger on update).
/// Use at module top level: `kigumi::register_constraint!("account.move", &["line_ids"], check_balanced);`
#[macro_export]
macro_rules! register_constraint {
    ($model:expr, $fields:expr, $func:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ConstraintRegistration { model: $model, fields: $fields, func: $func }
        }
    };
}

/// Registers a module's manifest in the global catalog, so `resolve_modules` can see it.
/// Use at module top level: `kigumi::register_module!(MANIFEST);`
#[macro_export]
macro_rules! register_module {
    ($manifest:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ModuleRegistration { manifest: || $manifest, crate_path: ::core::module_path!() }
        }
    };
}

/// Marks a model's table as owned outside the metamodel (Odoo's `_auto = False`): the model is
/// resolved/served normally but migration never creates or alters its table. For models mapped onto
/// a pre-existing table (e.g. `res.users` onto the auth subsystem's `kigumi_user`) or a SQL view.
/// Use at module top level: `kigumi::register_external!("res.users");`
#[macro_export]
macro_rules! register_external {
    ($model:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ExternalTable { model: $model }
        }
    };
}

/// Restricts a model field to the given groups (D6 field-level security): read AND write of that
/// field require membership in at least one group; superuser bypasses. Usually emitted automatically
/// by `#[field(groups = "...")]`, but can be declared by hand:
/// `kigumi::register_field_groups!("res.users", "login", &["admin"]);`
#[macro_export]
macro_rules! register_field_groups {
    ($model:expr, $field:expr, $groups:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::FieldGroupRegistration { model: $model, field: $field, groups: $groups }
        }
    };
}

/// Registers a module's ACLs so a server collects them via `registered_acls()`.
/// Use at module top level: `kigumi::register_acls!(ACLS);` where `ACLS: &'static [Acl]`.
#[macro_export]
macro_rules! register_acls {
    ($acls:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::AclRegistration { acls: || $acls }
        }
    };
}

/// Registers a module's record rules so a server collects them via `registered_rules()`.
/// Use at module top level: `kigumi::register_rules!(RULES);` where `RULES: &'static [RecordRule]`.
#[macro_export]
macro_rules! register_rules {
    ($rules:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::RecordRuleRegistration { rules: || $rules }
        }
    };
}

/// Registers a state-transition action on a model, runnable via `POST /api/<model>/<id>/action/<name>`.
/// `kigumi::register_action!("sale.order", "confirm", confirm_order, &["sales.user"]);`
#[macro_export]
macro_rules! register_action {
    ($model:expr, $name:expr, $func:expr, $groups:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ActionRegistration { model: $model, name: $name, func: $func, groups: $groups }
        }
    };
}

/// Registers a cross-record SERVICE on a model, runnable via `POST /api/<model>/<id>/service/<name>`.
/// The transactional twin of `register_action!`: `func` is a `async fn(&mut ServiceCtx, ServiceInput)
/// -> Result<ServiceOutput, DbError>` that owns multi-record logic through the secured `ServiceCtx`.
/// `write_gate` is `true` for a mutating service, `false` for a read-only one (a report). The macro
/// emits the one `Box::pin` that adapts the async fn to the stored fn pointer.
/// `kigumi::register_service!("sale.order.discount", "apply_discount", apply_discount, true, &["sales.user"]);`
#[macro_export]
macro_rules! register_service {
    ($model:expr, $name:expr, $func:path, $write_gate:expr, $groups:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ServiceRegistration {
                model: $model,
                name: $name,
                func: |cx, inp| ::std::boxed::Box::pin($func(cx, inp)),
                write_gate: $write_gate,
                groups: $groups,
            }
        }
    };
}

/// Registers a read-only LEDGER REPORT (record-less aggregate query, returning JSON rows) runnable via
/// `GET /api/reports/<name>` — distinct from `register_report!`, which is the per-record document/PDF
/// engine. `func` is an `async fn(&PgPool, params) -> Result<Vec<Json>, DbError>`; `read_model` is the
/// model whose Read ACL gates it. The macro emits the one `Box::pin` adapting the async fn to the fn ptr.
/// `kigumi::register_ledger_report!("trial_balance", "account.account", trial_balance, &[]);`
#[macro_export]
macro_rules! register_ledger_report {
    ($name:expr, $read_model:expr, $func:path, $groups:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::LedgerReportRegistration {
                name: $name,
                read_model: $read_model,
                func: |pool, params| ::std::boxed::Box::pin($func(pool, params)),
                groups: $groups,
            }
        }
    };
}

/// Registers a MODULE HTTP ROUTE on the generic dispatch `GET|POST /api/x/<name>` — bespoke module
/// endpoints (an inbound-webhook receiver, a custom search) without the module ever depending on the
/// server crate or axum. `method` is a RouteMethod variant name (Get|Post); `auth: false` runs the
/// body under the GUEST context (uid −1, no groups — the default-deny ACL blocks every secured call,
/// so verify the sender yourself, e.g. an HMAC over raw_body, then elevate via `.sudo()`).
/// `kigumi::register_route!("stripe-hook", Post, false, &[], stripe_hook);`
#[macro_export]
macro_rules! register_route {
    ($name:expr, $method:ident, $auth:expr, $groups:expr, $func:path) => {
        $crate::inventory::submit! {
            $crate::prelude::RouteRegistration {
                name: $name,
                method: $crate::prelude::RouteMethod::$method,
                auth: $auth,
                groups: $groups,
                func: |db, input| ::std::boxed::Box::pin($func(db, input)),
            }
        }
    };
}

/// Registers an in-tx WRITE TRIGGER: `func` runs on the caller's transaction after a secured write to
/// `model` when one of `watch` columns changed (empty = every write), for a stored effect the `depends`
/// recompute can't express on read — e.g. a Many2many aggregate summed across a join. `func` is an
/// `async fn(&mut Transaction, id, changed_cols) -> Result<(), DbError>`; a returned `Err` rolls the write
/// back. The macro emits the one `Box::pin` adapting the async fn to the stored fn pointer.
/// `kigumi::register_write_trigger!("product.template.attribute.value", &["price_extra"], recompute);`
#[macro_export]
macro_rules! register_write_trigger {
    ($model:expr, $watch:expr, $func:path) => {
        $crate::inventory::submit! {
            $crate::prelude::WriteTriggerRegistration {
                model: $model,
                watch: $watch,
                func: |tx, id, changed| ::std::boxed::Box::pin($func(tx, id, changed)),
            }
        }
    };
}

/// Opts a model into the mail subsystem (chatter): it gains a thread of messages, followers and
/// activities via the `(res_model, res_id)` link, and the framework cleans that thread up when the
/// record is deleted. One line, no mixin: `kigumi::register_mailed!("sale.order");`
#[macro_export]
macro_rules! register_mailed {
    ($model:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::MailedRegistration { model: $model }
        }
    };
}

/// Marks a model as transient (Odoo's `TransientModel`): a wizard scratchpad whose rows are
/// ephemeral. The model is served + secured like any model, but an hourly GC cron reclaims rows by
/// age, and migration gives its `create_date` column a `DEFAULT now()` so every insert is stamped.
/// The model must declare a nullable `create_date: Datetime`. One line:
/// `kigumi::register_transient!("sale.order.discount");`
#[macro_export]
macro_rules! register_transient {
    ($model:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::TransientRegistration { model: $model }
        }
    };
}

/// Registers a report on a model: a pure render fn producing an HTML document for one record, exposed
/// at `GET /api/<model>/<id>/report/<name>` (secured by read access to the record) and listed in the
/// model's UI contract. `name` is the URL segment, `title` the human label. Use at module top level:
/// `kigumi::register_report!("sale.order", "quotation", "Quotation", render_quotation);`
#[macro_export]
macro_rules! register_report {
    ($model:expr, $name:expr, $title:expr, $func:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ReportRegistration { model: $model, name: $name, title: $title, func: $func }
        }
    };
}

/// Registers a model's form layout (Odoo's form arch, minimal): titled groups of scalar fields plus a
/// notebook of tabbed pages. Emitted in the model's UI contract so the frontend renders a real view
/// instead of dumping fields in declaration order. Use at module top level:
/// `kigumi::register_view!("product.product", &[ FieldGroup { .. } ], &[ NotebookPage { .. } ]);`
#[macro_export]
macro_rules! register_view {
    ($model:expr, $groups:expr, $pages:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::FormView { model: $model, groups: $groups, pages: $pages }
        }
    };
}

/// Registers a wizard: binds a transient `model` to its `default_get` (the server-side seed computed
/// from the open context), exposing `POST /api/<model>/open`. The model must also be
/// `register_transient!`-marked. Apply logic is a dedicated per-wizard service method + endpoint
/// (like `apply_pricelist`), not part of this registration. Use at module top level:
/// `kigumi::register_wizard!("sale.order.discount", default_get_discount);`
#[macro_export]
macro_rules! register_wizard {
    ($model:expr, $default_get:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::WizardRegistration { model: $model, default_get: $default_get }
        }
    };
}

/// Declares delegation inheritance (Odoo's `_inherits`): `model` exposes `parent`'s stored scalar
/// fields through its required Many2one `via` FK. Usually emitted by `#[model(inherits=…, via=…)]`:
/// `kigumi::register_inherits!("product.product", "product.template", "product_tmpl_id");`
#[macro_export]
macro_rules! register_inherits {
    ($model:expr, $parent:expr, $via:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::InheritsRegistration { model: $model, parent: $parent, via: $via }
        }
    };
}

/// Marks a field as tracked (Odoo's `tracking=True`): a change to it on a mailed model records a
/// `notification` message + a typed `mail.tracking` row in the chatter. Usually emitted by
/// `#[field(tracked)]`, but can be declared by hand:
/// `kigumi::register_tracked!("sale.order", "state");`
#[macro_export]
macro_rules! register_tracked {
    ($model:expr, $field:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::TrackedFieldRegistration { model: $model, field: $field }
        }
    };
}
