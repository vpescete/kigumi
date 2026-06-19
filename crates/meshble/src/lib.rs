//! Facade for the Meshble framework. Application modules depend only on this crate.

// Re-export of inventory so that macros can emit `::meshble::inventory::submit!`
// without every module having to add the dependency.
pub use meshble_core::inventory;

/// Everything needed to define a module: `use meshble::prelude::*;`
pub mod prelude {
    pub use meshble_core::{
        action_for, check_access, check_compat, compute_fn, compute_stored, computed_fields,
        delegated_fields, external_tables, field_accessible, field_required_groups, inherits_of,
        is_mailed, is_transient, json_string, mailed_models, migration_plan, module_closure, module_of,
        check_constraints, has_constraints, has_read_computes, compute_on_read, transient_models,
        record_rule_domain, registered_acls, registered_group_names, registered_model_names,
        registered_rules, related_path, resolve, resolve_all_registered, resolve_module_set,
        resolve_modules, resolve_registered, tracked_fields, validate_depends, Acl, AclRegistration,
        ActionFn, ActionInput, ActionOutcome, ActionRegistration, Children, ComputeFn, ComputeInput,
        ComputeRegistration, ConstraintFn, ConstraintRegistration, Condition, Ctx, DelegatedField, Domain, DomainError, ExternalTable,
        FieldBuilder, FieldDef, FieldExtension, FieldGroupRegistration, FieldKind,
        InheritsRegistration, MailedRegistration, MigrationTarget, Model, ModelDescriptor,
        ModelRegistration, ModuleDep, ModuleManifest, ModuleRegistration, Operation, Operator,
        RecordRule, RecordRuleRegistration, RelatedRegistration, ResolutionError, ResolvedModel,
        RuleDomain, Sql, TrackedFieldRegistration, TransientRegistration, Value, FRAMEWORK_VERSION,
        wizard_for, WizardContext, WizardDefaultGet, WizardRegistration,
        report_for, reports_for, ReportFn, ReportRegistration,
        view_for, FieldGroup, FieldSlot, FormView, NotebookPage,
    };
    pub use meshble_macros::{extend, model};
    pub use meshble_schema::{openapi, to_ddl, to_ui_contract, FieldRule, UiRule};
}

/// Marks a field as a `related` field (Odoo `related=`): a non-stored, read-only mirror of the value
/// reached by `path` (e.g. "order_id.currency_id"). Usually emitted by `#[field(related = "...")]`.
/// `meshble::register_related!("sale.order.line", "order_currency_id", "order_id.currency_id");`
#[macro_export]
macro_rules! register_related {
    ($model:expr, $field:expr, $path:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::RelatedRegistration { model: $model, field: $field, path: $path }
        }
    };
}

/// Registers a compute function by name, so the engine runs it on write for fields declaring it.
/// Use at module top level: `meshble::register_compute!("compute_total", compute_total);`
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
/// Use at module top level: `meshble::register_constraint!("account.move", &["line_ids"], check_balanced);`
#[macro_export]
macro_rules! register_constraint {
    ($model:expr, $fields:expr, $func:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ConstraintRegistration { model: $model, fields: $fields, func: $func }
        }
    };
}

/// Registers a module's manifest in the global catalog, so `resolve_modules` can see it.
/// Use at module top level: `meshble::register_module!(MANIFEST);`
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
/// a pre-existing table (e.g. `res.users` onto the auth subsystem's `meshble_user`) or a SQL view.
/// Use at module top level: `meshble::register_external!("res.users");`
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
/// `meshble::register_field_groups!("res.users", "login", &["admin"]);`
#[macro_export]
macro_rules! register_field_groups {
    ($model:expr, $field:expr, $groups:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::FieldGroupRegistration { model: $model, field: $field, groups: $groups }
        }
    };
}

/// Registers a module's ACLs so a server collects them via `registered_acls()`.
/// Use at module top level: `meshble::register_acls!(ACLS);` where `ACLS: &'static [Acl]`.
#[macro_export]
macro_rules! register_acls {
    ($acls:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::AclRegistration { acls: || $acls }
        }
    };
}

/// Registers a module's record rules so a server collects them via `registered_rules()`.
/// Use at module top level: `meshble::register_rules!(RULES);` where `RULES: &'static [RecordRule]`.
#[macro_export]
macro_rules! register_rules {
    ($rules:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::RecordRuleRegistration { rules: || $rules }
        }
    };
}

/// Registers a state-transition action on a model, runnable via `POST /api/<model>/<id>/action/<name>`.
/// `meshble::register_action!("sale.order", "confirm", confirm_order, &["sales.user"]);`
#[macro_export]
macro_rules! register_action {
    ($model:expr, $name:expr, $func:expr, $groups:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::ActionRegistration { model: $model, name: $name, func: $func, groups: $groups }
        }
    };
}

/// Opts a model into the mail subsystem (chatter): it gains a thread of messages, followers and
/// activities via the `(res_model, res_id)` link, and the framework cleans that thread up when the
/// record is deleted. One line, no mixin: `meshble::register_mailed!("sale.order");`
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
/// `meshble::register_transient!("sale.order.discount");`
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
/// `meshble::register_report!("sale.order", "quotation", "Quotation", render_quotation);`
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
/// `meshble::register_view!("product.product", &[ FieldGroup { .. } ], &[ NotebookPage { .. } ]);`
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
/// `meshble::register_wizard!("sale.order.discount", default_get_discount);`
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
/// `meshble::register_inherits!("product.product", "product.template", "product_tmpl_id");`
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
/// `meshble::register_tracked!("sale.order", "state");`
#[macro_export]
macro_rules! register_tracked {
    ($model:expr, $field:expr) => {
        $crate::inventory::submit! {
            $crate::prelude::TrackedFieldRegistration { model: $model, field: $field }
        }
    };
}
