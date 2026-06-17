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
    // Numbered "New" until confirmed, when `confirm` assigns the SO sequence (like Odoo's draft name).
    #[field(label = "Order Reference", default = "New")]
    name: Text,

    #[field(label = "Customer", required, target = "res.partner")]
    partner_id: Many2one,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    #[field(label = "Order Lines", target = "sale.order.line", inverse = "order_id")]
    line_ids: One2many,

    #[field(label = "Status", required, default = "draft", selection = "draft:Draft,sale:Confirmed,done:Done")]
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
    #[field(label = "Margin", compute = "compute_margin", depends = "line_ids.margin", currency = "currency_id", store)]
    margin: Decimal,
}

/// Product catalog — the simplest sellable unit. Variants/templates (`product.template` +
/// attributes) come later; one flat model is enough for the quote-to-order vertical.
// ponytail: single product model; split into template/variant only when variants are needed.
#[model(name = "product.product", table = "product_product")]
pub struct ProductProduct {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Internal Reference")]
    default_code: Text,

    #[field(label = "Sales Price", default = "0")]
    list_price: Decimal,

    #[field(label = "Cost", default = "0")]
    standard_price: Decimal,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// A line of a sale order: a product, a quantity, a unit price. `price_subtotal` and `margin` are
/// stored same-record computes; the order rolls them up into `amount_total` / `margin` through the
/// aggregate cascade (`recompute_parent`).
#[model(name = "sale.order.line", table = "sale_order_line")]
pub struct SaleOrderLine {
    #[field(label = "Order", required, target = "sale.order")]
    order_id: Many2one,

    #[field(label = "Product", required, target = "product.product")]
    product_id: Many2one,

    // Company-scoped like the order, so direct line access honours multi-company isolation.
    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    #[field(label = "Description")]
    name: Text,

    #[field(label = "Quantity", required, default = "1")]
    product_uom_qty: Decimal,

    #[field(label = "Unit Price", required, default = "0")]
    price_unit: Decimal,

    // Cost captured on the line (an onchange would default it from the product; onchange is deferred).
    // D6 field-level security: cost is manager-only — read and write require `sales.manager`.
    #[field(label = "Cost", default = "0", groups = "sales.manager")]
    purchase_price: Decimal,

    #[field(label = "Subtotal", compute = "compute_line_subtotal", depends = "product_uom_qty,price_unit", store)]
    price_subtotal: Decimal,

    #[field(label = "Margin", compute = "compute_line_margin", depends = "price_unit,purchase_price,product_uom_qty", store)]
    margin: Decimal,
}

/// `amount_total` of an order = exact sum of its lines' subtotals.
fn compute_amount(i: &ComputeInput) -> Value {
    Value::Decimal(i.sum_decimal("line_ids", "price_subtotal"))
}
/// `margin` of an order = exact sum of its lines' margins.
fn compute_margin(i: &ComputeInput) -> Value {
    Value::Decimal(i.sum_decimal("line_ids", "margin"))
}
/// A line's subtotal = quantity × unit price (exact money).
fn compute_line_subtotal(i: &ComputeInput) -> Value {
    Value::Decimal(i.decimal("product_uom_qty") * i.decimal("price_unit"))
}
/// A line's margin = (unit price − cost) × quantity. Computed from the raw inputs, NOT from
/// `price_subtotal`, because every same-record compute reads the pre-write snapshot (no chaining).
fn compute_line_margin(i: &ComputeInput) -> Value {
    Value::Decimal((i.decimal("price_unit") - i.decimal("purchase_price")) * i.decimal("product_uom_qty"))
}
meshble::register_compute!("compute_amount", compute_amount);
meshble::register_compute!("compute_margin", compute_margin);
meshble::register_compute!("compute_line_subtotal", compute_line_subtotal);
meshble::register_compute!("compute_line_margin", compute_line_margin);

/// Resolves the module's complete model from the catalog (base + auto-registered extensions).
pub fn resolved_sale_order() -> ResolvedModel {
    resolve_registered("sale.order").expect("sale.order resolution")
}

/// Access control. `sales.user` runs orders and their lines; products are read by everyone in
/// sales and maintained by `sales.manager`.
pub static ACLS: &[Acl] = &[
    Acl { model: "sale.order", group: "sales.user", read: true, write: true, create: true, delete: false },
    Acl { model: "sale.order.line", group: "sales.user", read: true, write: true, create: true, delete: true },
    Acl { model: "product.product", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "product.product", group: "sales.manager", read: true, write: true, create: true, delete: true },
];

fn not_done() -> Domain {
    Domain::field("state").ne("done")
}
fn small_orders() -> Domain {
    Domain::field("amount_total").lt(10_000_i64)
}
fn done_state() -> Domain {
    Domain::field("state").eq("done")
}
// Lines inherit their order's visibility (parity with the rules above), traversing order_id → the
// order. Without these, direct line access would leak rows the caller can't see on the order itself.
fn line_parent_not_done() -> Domain {
    Domain::field("order_id.state").ne("done")
}
fn line_parent_small() -> Domain {
    Domain::field("order_id.amount_total").lt(10_000_i64)
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
    RecordRule { model: "sale.order", groups: &[], ops: &[Operation::Read], domain: RuleDomain::Static(not_done) },
    RecordRule {
        model: "sale.order",
        groups: &["sales.user"],
        ops: &[Operation::Read],
        domain: RuleDomain::Static(small_orders),
    },
    // Same restrictions on the lines, reached through their order.
    RecordRule { model: "sale.order.line", groups: &[], ops: &[Operation::Read], domain: RuleDomain::Static(line_parent_not_done) },
    RecordRule {
        model: "sale.order.line",
        groups: &["sales.user"],
        ops: &[Operation::Read],
        domain: RuleDomain::Static(line_parent_small),
    },
];

meshble::register_acls!(ACLS);
meshble::register_rules!(RECORD_RULES);

/// `confirm`: a draft order becomes a confirmed sale and is assigned its SO number from the sequence.
fn confirm_order(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "draft" => Ok(ActionOutcome::new()
            .set("state", Value::Str("sale".to_string()))
            .assign_sequence("name", "SO")),
        s => Err(format!("can only confirm a draft order (state is '{s}')")),
    }
}
meshble::register_action!("sale.order", "confirm", confirm_order, &["sales.user"]);

/// `done`: a confirmed sale is locked as done (its total then becomes read-only via the UI rule).
fn set_done(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "sale" => Ok(ActionOutcome::new().set("state", Value::Str("done".to_string()))),
        s => Err(format!("can only finish a confirmed order (state is '{s}')")),
    }
}
meshble::register_action!("sale.order", "done", set_done, &["sales.user"]);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_compatible_with_framework() {
        // The framework declares FRAMEWORK_VERSION (0.1.0); the module accepts ">=0.1, <0.2".
        assert!(check_compat(&MANIFEST, FRAMEWORK_VERSION).is_ok());
    }

    #[test]
    fn field_groups_macro_registers_restriction() {
        // `#[field(groups = "sales.manager")]` on purchase_price emits a FieldGroupRegistration;
        // unrestricted fields return None (D6).
        assert_eq!(field_required_groups("sale.order.line", "purchase_price"), Some(&["sales.manager"][..]));
        assert_eq!(field_required_groups("sale.order.line", "price_unit"), None);
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
        assert_eq!(d.fields.len(), 7); // name, partner_id, company_id, line_ids, state, currency_id, amount_total
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
        // D7: the contract also carries the list columns and the form's actions.
        assert!(contract.contains("\"list\": { \"columns\": ["), "list columns emitted");
        assert!(contract.contains("\"actions\": ["), "actions emitted");
        assert!(contract.contains("\"name\": \"confirm\""), "confirm action present");
        assert!(contract.contains("\"groups\": [\"sales.user\"]"), "action groups present");
    }
}
