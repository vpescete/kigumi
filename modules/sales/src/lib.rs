//! Application module `sales`: `sale.order` + `sale_margin` extension.
//! Defined by hand in the walking skeleton; in phase 2 it will be `#[model]` / `#[extend]`.

use meshble::prelude::*;

/// Module manifest: its own version + compatibility range with the framework.
/// Equivalent to Odoo's `__manifest__.py`, but with verifiable versions.
pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "sales",
    version: "1.0.0",
    framework: ">=0.1, <0.2",
    depends: &[ModuleDep { name: "base", req: "^1.0" }],
    summary: "Sales order management",
};
meshble::register_module!(MANIFEST);

/// The "base" of sale.order — now declared with `#[model]` (phase 2).
/// The macro generates `ModelDescriptor` + `impl Model`; the field "types" are the DSL.
#[model(name = "sale.order", table = "sale_order")]
pub struct SaleOrder {
    #[field(label = "Order Reference", required)]
    name: Text,

    #[field(label = "Customer", required, target = "res.partner")]
    partner_id: Many2one,

    #[field(label = "Order Lines", target = "sale.order.line", inverse = "order_id")]
    line_ids: One2many,

    #[field(label = "Status", required, selection = "draft:Draft,sale:Confirmed,done:Done")]
    state: Selection,

    #[field(label = "Currency", required, target = "res.currency")]
    currency_id: Many2one,

    #[field(label = "Total", compute = "compute_amount", depends = "line_ids.price_subtotal", currency = "currency_id", store)]
    amount_total: Decimal,
}

/// `sale_margin` extension: adds `margin` via `#[extend]`, WITHOUT touching the base.
/// It auto-registers in the catalog (phase 3) — no wiring in `resolved_sale_order`.
#[extend("sale.order")]
pub struct SaleMargin {
    #[field(label = "Margin", compute = "compute_margin", depends = "amount_total", currency = "currency_id", store)]
    margin: Decimal,
}

/// Resolves the module's complete model from the catalog (base + auto-registered extensions).
pub fn resolved_sale_order() -> ResolvedModel {
    resolve_registered("sale.order").expect("sale.order resolution")
}

/// Access control: `sales.user` can read/write/create sale orders, but not delete.
pub static ACLS: &[Acl] = &[Acl {
    model: "sale.order",
    group: "sales.user",
    read: true,
    write: true,
    create: true,
    delete: false,
}];

fn not_done() -> Domain {
    Domain::field("state").ne("done")
}
fn small_orders() -> Domain {
    Domain::field("amount_total").lt(10_000_i64)
}
fn done_state() -> Domain {
    Domain::field("state").eq("done")
}

/// UI rules: the total becomes read-only once the order is "done". Emitted into the UI contract
/// as a portable domain AST the frontend evaluates client-side.
pub static UI_RULES: &[FieldRule] = &[FieldRule {
    field: "amount_total",
    rule: UiRule::Readonly,
    domain: done_state,
}];

/// Row-level rules: everyone is restricted to non-"done" orders; juniors only see small ones.
pub static RECORD_RULES: &[RecordRule] = &[
    RecordRule { model: "sale.order", groups: &[], ops: &[Operation::Read], domain: not_done },
    RecordRule {
        model: "sale.order",
        groups: &["sales.user"],
        ops: &[Operation::Read],
        domain: small_orders,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_compatible_with_framework() {
        // The framework declares FRAMEWORK_VERSION (0.1.0); the module accepts ">=0.1, <0.2".
        assert!(check_compat(&MANIFEST, FRAMEWORK_VERSION).is_ok());
    }

    #[test]
    fn catalog_resolves_all_models() {
        // Reference a base symbol so the base crate is linked into this test binary and its
        // models self-register (the inventory linkage requirement).
        let _ = meshble_mod_base::MANIFEST;
        // base (res.partner, res.currency) + sales (sale.order) are all registered.
        let names = registered_model_names();
        for expected in ["res.partner", "res.currency", "sale.order"] {
            assert!(names.contains(&expected), "missing {expected}");
        }
        assert!(resolve_all_registered().is_ok());
    }

    #[test]
    fn margin_extension_merged() {
        let m = resolved_sale_order();
        assert_eq!(m.fields.len(), SaleOrder::descriptor().fields.len() + 1);
        assert!(m.fields.iter().any(|f| f.name == "margin"));
    }

    #[test]
    fn macro_generates_correct_descriptor() {
        // The macro must produce the SAME descriptor as the hand-written version.
        let d = SaleOrder::descriptor();
        assert_eq!(d.name, "sale.order");
        assert_eq!(d.fields.len(), 6);
        let total = d.fields.iter().find(|f| f.name == "amount_total").unwrap();
        assert!(total.stored, "computed with `store` must be stored");
        assert_eq!(total.compute, Some("compute_amount"));
        assert_eq!(total.depends, &["line_ids.price_subtotal"]);
        let lines = d.fields.iter().find(|f| f.name == "line_ids").unwrap();
        assert!(!lines.has_column(), "one2many has no column");
    }

    #[test]
    fn ddl_and_ui_generated() {
        let m = resolved_sale_order();
        let ddl = to_ddl(&m);
        assert!(!ddl.contains("line_ids"), "one2many has no column");
        assert!(ddl.contains("partner_id bigint REFERENCES res_partner(id) NOT NULL"));
        let contract = to_ui_contract(&m, UI_RULES).unwrap();
        assert!(contract.contains("\"widget\": \"monetary\""));
        // The dynamic readonly rule is emitted as a portable domain AST.
        assert!(contract.contains("\"readonly_when\": {\"field\":\"state\",\"op\":\"=\",\"value\":\"done\"}"));
    }
}
