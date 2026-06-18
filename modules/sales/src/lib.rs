//! Application module `sales`: `sale.order` + `sale_margin` extension.
//! Defined by hand in the walking skeleton; in phase 2 it will be `#[model]` / `#[extend]`.

use meshble::prelude::*;

/// Module manifest: its own version + compatibility range with the framework.
/// Equivalent to Odoo's `__manifest__.py`, but with verifiable versions.
pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "sales",
    version: "1.0.0",
    framework: ">=0.1, <0.2",
    depends: &[ModuleDep { name: "base", req: "^1.0" }, ModuleDep { name: "mail", req: "^1.0" }],
    summary: "Sales order management",
};
meshble::register_module!(MANIFEST);

// `sale.order` opts into the mail subsystem: it gains a chatter thread (messages now; tracking,
// followers and activities in later slices), and the framework cleans that thread up on delete.
meshble::register_mailed!("sale.order");

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

    #[field(label = "Status", required, default = "draft", tracked, selection = "draft:Draft,sale:Confirmed,done:Done")]
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

/// A product category (Odoo's `product.category`): a hierarchical grouping of products. Self-
/// referential (a category has an optional parent).
#[model(name = "product.category", table = "product_category")]
pub struct ProductCategory {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Parent Category", target = "product.category")]
    parent_id: Many2one,
}

/// A unit of measure (Odoo's `uom.uom`, simplified): a named unit with a ratio to its category's
/// reference unit and a rounding precision. Products are sold/stocked in a UoM.
#[model(name = "uom.uom", table = "uom_uom")]
pub struct UomUom {
    #[field(label = "Unit of Measure", required)]
    name: Text,

    #[field(label = "Type", default = "reference", selection = "bigger:Bigger than the reference,reference:Reference for this category,smaller:Smaller than the reference")]
    uom_type: Selection,

    #[field(label = "Ratio", default = "1")]
    factor: Float,

    #[field(label = "Rounding Precision", default = "0.01")]
    rounding: Float,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// Product template (Odoo's `product.template`): the SHARED definition of a product — the fields
/// every variant has in common. Variants (`product.product`) inherit these via `_inherits`, so N
/// variants share ONE template row with no duplication and no template→variant sync.
#[model(name = "product.template", table = "product_template")]
pub struct ProductTemplate {
    #[field(label = "Name", required)]
    name: Text,

    // Named `product_type` (not `type`: a SQL reserved word and a Rust keyword).
    #[field(label = "Type", default = "consu", selection = "consu:Goods,service:Service")]
    product_type: Selection,

    #[field(label = "Product Category", target = "product.category")]
    categ_id: Many2one,

    #[field(label = "Unit of Measure", target = "uom.uom")]
    uom_id: Many2one,

    #[field(label = "Sales Price", default = "0", tracked)]
    list_price: Decimal,

    #[field(label = "Cost", default = "0", tracked)]
    standard_price: Decimal,

    // Rich text (Odoo's product description is HTML): sanitized on write, rendered as an html widget.
    #[field(label = "Description")]
    description: Html,

    // A product image: an ir.attachment whose bytes live in the blob store. Set after uploading the
    // image as an attachment to the product; the FE renders it via the attachment content endpoint.
    // Delegated to the variant, which inherits the template image unless it sets its own.
    #[field(label = "Image")]
    image: Image,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

// Retrofit: a product (template) has a chatter thread; price changes are tracked as audit entries.
meshble::register_mailed!("product.template");

/// Product variant (Odoo's `product.product`): a sellable unit that `_inherits` its product.template
/// through the required `product_tmpl_id` FK, transparently exposing the template's name/price/etc.
/// while carrying variant-specific fields (internal reference, barcode, tags). Creating a variant
/// without a template auto-creates one (the write-split); referenced by `sale.order.line.product_id`.
#[model(name = "product.product", table = "product_product", inherits = "product.template", via = "product_tmpl_id")]
pub struct ProductProduct {
    #[field(label = "Product Template", required, target = "product.template")]
    product_tmpl_id: Many2one,

    #[field(label = "Internal Reference")]
    default_code: Text,

    #[field(label = "Barcode")]
    barcode: Text,

    // On-read display name: the (inherited) template name with the internal reference appended. Derived
    // on every read — `name` is delegated from the template, `default_code` is the variant's own field.
    #[field(label = "Display Name", compute = "product_display_name", depends = "name,default_code")]
    display_name: Text,

    // The variant's OWN active flag, intentionally shadowing product.template.active: archiving one
    // variant (the generator does this when a combination is no longer selected) must not touch the
    // shared template or the other variants. Read/written on product.product, never delegated.
    #[field(label = "Active", default = "true")]
    active: Bool,

    // First-class Many2many: variant tags through a junction table.
    #[field(label = "Tags", target = "product.tag", relation = "product_product_tag_rel", column = "product_id", target_column = "tag_id")]
    tag_ids: Many2many,

    // The exact attribute combination this variant represents: the set of `product.template.attribute.value`
    // rows (one per attribute line). The variant-generation engine sets this; the combo key derived from
    // it is how regeneration recognises an existing variant (so it is kept/reactivated, never duplicated).
    #[field(label = "Attribute Values", target = "product.template.attribute.value", relation = "variant_ptav_rel", column = "product_id", target_column = "ptav_id")]
    product_template_attribute_value_ids: Many2many,
}

/// A product tag/label (the comodel of `product.product.tag_ids`).
#[model(name = "product.tag", table = "product_tag")]
pub struct ProductTag {
    #[field(label = "Name", required, unique)]
    name: Text,
}

/// A product attribute (Odoo's `product.attribute`): a configurable dimension of a product, e.g.
/// "Color" or "Size". Its values (`product.attribute.value`) are combined across a template's
/// attribute lines to generate variants.
#[model(name = "product.attribute", table = "product_attribute")]
pub struct ProductAttribute {
    #[field(label = "Attribute", required)]
    name: Text,

    // `always` values multiply into variants; `no_variant` values are informational only and are
    // excluded from the cartesian product (Odoo also has `dynamic`, dropped for v1).
    #[field(label = "Variant Creation", default = "always", selection = "always:Instantly,no_variant:Never (option)")]
    create_variant: Selection,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// A possible value of an attribute (Odoo's `product.attribute.value`), e.g. "Red" of "Color".
#[model(name = "product.attribute.value", table = "product_attribute_value")]
pub struct ProductAttributeValue {
    #[field(label = "Value", required)]
    name: Text,

    #[field(label = "Attribute", required, target = "product.attribute")]
    attribute_id: Many2one,

    // Deterministic ordering of values within an attribute → a stable, order-independent combo key.
    #[field(label = "Sequence", default = "10")]
    sequence: Integer,
}

/// A template's attribute line (Odoo's `product.template.attribute.line`): on THIS template, which
/// attribute is configured and which of its values are selected. One line per (template, attribute);
/// the engine reads `value_ids` to build the cartesian product.
#[model(name = "product.template.attribute.line", table = "product_template_attribute_line")]
pub struct ProductTemplateAttributeLine {
    #[field(label = "Product Template", required, target = "product.template")]
    product_tmpl_id: Many2one,

    #[field(label = "Attribute", required, target = "product.attribute")]
    attribute_id: Many2one,

    // The selected values for this attribute on this template (a subset of the attribute's values).
    // The engine reads this through the standard M2M projection (it consumes these descriptor column
    // names — `line_id`/`value_id` — never hand-written SQL), so the junction naming is internal.
    #[field(label = "Values", target = "product.attribute.value", relation = "ptal_value_rel", column = "line_id", target_column = "value_id")]
    value_ids: Many2many,
}

/// The per-template instance of a chosen value (Odoo's `product.template.attribute.value`): the join
/// row tying a generated variant to one cell of its combination. `product_tmpl_id` is denormalized
/// for fast diff queries. Odoo's `price_extra`/`ptav_active` are deferred (additive later).
///
/// Engine-managed, NOT user input: it is read-only over the API (the generation engine creates these
/// elevated, after the manager gate, like the mail subsystem). That, plus the engine's per-template
/// advisory lock and a composite-unique index added in the engine slice, keeps it free of duplicate
/// `(attribute_line_id, product_attribute_value_id)` cells that would split one combo across two ids.
#[model(name = "product.template.attribute.value", table = "product_template_attribute_value")]
pub struct ProductTemplateAttributeValue {
    // The 3 structural FKs are engine-LOCKED via D6 (`groups = "base.system"`, a group no user holds):
    // only the generation engine (sudo) may set them, so a manager editing `price_extra` (write ACL)
    // physically cannot mutate them — check_writable_fields rejects them on every secured write path.
    #[field(label = "Product Template", required, target = "product.template", groups = "base.system")]
    product_tmpl_id: Many2one,

    #[field(label = "Attribute Line", required, target = "product.template.attribute.line", groups = "base.system")]
    attribute_line_id: Many2one,

    #[field(label = "Attribute Value", required, target = "product.attribute.value", groups = "base.system")]
    product_attribute_value_id: Many2one,

    // The per-template-per-value price surcharge — the ONE field a manager may edit. Materialized into
    // product.product.price_extra (sum over a variant's PTAVs) by the engine and a PTAV-write hook.
    #[field(label = "Extra Price", default = "0")]
    price_extra: Decimal,
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

    // Related (read-only mirror): the order's customer, resolved from order_id.partner_id.
    #[field(label = "Customer", target = "res.partner", related = "order_id.partner_id")]
    order_partner_id: Many2one,

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
/// A variant's display name: the (inherited) template name, with the internal reference in parentheses
/// when one is set. On-read — reads the delegated `name` and the variant's own `default_code`.
fn product_display_name(i: &ComputeInput) -> Value {
    let name = i.str("name");
    let code = i.str("default_code");
    Value::Str(if code.is_empty() { name.to_string() } else { format!("{name} ({code})") })
}
meshble::register_compute!("product_display_name", product_display_name);
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
    // Catalog reference data (categories, units): read by everyone in sales, maintained by managers.
    Acl { model: "product.category", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "product.category", group: "sales.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "uom.uom", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "uom.uom", group: "sales.manager", read: true, write: true, create: true, delete: true },
    // Templates mirror variants: everyone in sales reads, managers maintain. A manager creating a
    // variant auto-creates its template, so the create/write ACLs must match product.product's.
    Acl { model: "product.template", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "product.template", group: "sales.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "product.product", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "product.product", group: "sales.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "product.tag", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "product.tag", group: "sales.manager", read: true, write: true, create: true, delete: true },
    // Attribute configuration is user input: everyone in sales reads, managers maintain the attributes,
    // their values, and a template's attribute lines.
    Acl { model: "product.attribute", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "product.attribute", group: "sales.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "product.attribute.value", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "product.attribute.value", group: "sales.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "product.template.attribute.line", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "product.template.attribute.line", group: "sales.manager", read: true, write: true, create: true, delete: true },
    // PTAV is the engine's generated join row: the engine (sudo) creates/deletes the cells, and a
    // manager may WRITE only `price_extra` (the 3 structural FKs are D6-locked to base.system, so a
    // manager write cannot touch them). No user create/delete — combos stay engine-owned.
    Acl { model: "product.template.attribute.value", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "product.template.attribute.value", group: "sales.manager", read: true, write: true, create: false, delete: false },
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
    fn module_closure_and_ownership() {
        // Link base and mail so their manifests/models are registered in this test binary.
        let _ = (&meshble_mod_base::MANIFEST, &meshble_mod_mail::MANIFEST);
        // Installing sales pulls in its dependency closure, deps first (base, then mail, then sales).
        assert_eq!(module_closure("sales").unwrap(), vec!["base", "mail", "sales"]);
        assert_eq!(module_closure("base").unwrap(), vec!["base"]);
        assert!(module_closure("nope").is_err(), "unknown module errors");
        // Each model maps to its owning module (the migration/serve gate).
        assert_eq!(module_of("sale.order"), Some("sales"));
        assert_eq!(module_of("product.product"), Some("sales"));
        assert_eq!(module_of("res.partner"), Some("base"));
    }

    #[test]
    fn many2many_macro_builds_descriptor() {
        // `#[field(target=…, relation=…, column=…, target_column=…)] tag_ids: Many2many` builds the
        // Many2many kind (no column on the model — the membership lives in the junction).
        let m = resolve_registered("product.product").unwrap();
        let tags = m.fields.iter().find(|f| f.name == "tag_ids").unwrap();
        assert!(!tags.has_column(), "Many2many has no column on the model");
        assert!(matches!(
            tags.kind,
            FieldKind::Many2many { target: "product.tag", relation: "product_product_tag_rel", column: "product_id", target_column: "tag_id" }
        ));
    }

    #[test]
    fn ptav_structural_fks_are_engine_locked_price_extra_open() {
        // The 3 combo-identity FKs require the engine-only `base.system` group (no user holds it →
        // only sudo writes them, via the generation engine); price_extra is ungated (manager-writable).
        for f in ["product_tmpl_id", "attribute_line_id", "product_attribute_value_id"] {
            assert_eq!(
                field_required_groups("product.template.attribute.value", f),
                Some(&["base.system"][..]),
                "{f} must be engine-locked"
            );
        }
        assert_eq!(field_required_groups("product.template.attribute.value", "price_extra"), None);
        // The manager has WRITE on the model (so it can edit price_extra), but not create/delete.
        let mgr = ACLS.iter().find(|a| a.model == "product.template.attribute.value" && a.group == "sales.manager").unwrap();
        assert!(mgr.write && !mgr.create && !mgr.delete, "manager edits price_extra, engine owns combos");
    }

    #[test]
    fn product_image_is_an_attachment_fk_with_image_widget() {
        let m = resolve_registered("product.template").unwrap();
        let img = m.fields.iter().find(|f| f.name == "image").unwrap();
        assert!(matches!(img.kind, FieldKind::Image));
        assert!(img.has_column(), "Image is a stored FK column");
        let ddl = to_ddl(&m);
        assert!(ddl.contains("image bigint REFERENCES meshble_attachment(id)"), "image FK in DDL: {ddl}");
        let contract = to_ui_contract(&m, &[]).unwrap();
        assert!(contract.contains("\"name\": \"image\""), "image in contract");
        assert!(contract.contains("\"widget\": \"image\""), "image widget");
    }

    #[test]
    fn variant_models_shape() {
        // Pin the field names/relations the generation engine hardcodes (later slices), so a rename
        // breaks here loudly rather than silently mis-wiring the engine.
        let attr = resolve_registered("product.attribute").unwrap();
        assert!(matches!(
            attr.fields.iter().find(|f| f.name == "create_variant").unwrap().kind,
            FieldKind::Selection(&[("always", _), ("no_variant", _)])
        ));

        let val = resolve_registered("product.attribute.value").unwrap();
        assert!(matches!(val.fields.iter().find(|f| f.name == "attribute_id").unwrap().kind, FieldKind::Many2one { target: "product.attribute" }));
        assert!(matches!(val.fields.iter().find(|f| f.name == "sequence").unwrap().kind, FieldKind::Integer));

        let line = resolve_registered("product.template.attribute.line").unwrap();
        let value_ids = line.fields.iter().find(|f| f.name == "value_ids").unwrap();
        assert!(!value_ids.has_column(), "value_ids is a junction-backed M2M");
        assert!(matches!(
            value_ids.kind,
            FieldKind::Many2many { target: "product.attribute.value", relation: "ptal_value_rel", column: "line_id", target_column: "value_id" }
        ));

        let ptav = resolve_registered("product.template.attribute.value").unwrap();
        for (f, t) in [("product_tmpl_id", "product.template"), ("attribute_line_id", "product.template.attribute.line"), ("product_attribute_value_id", "product.attribute.value")] {
            let fd = ptav.fields.iter().find(|x| x.name == f).unwrap();
            assert!(fd.required, "{f} is required");
            assert!(matches!(fd.kind, FieldKind::Many2one { target } if target == t));
        }

        // The variant's combo link — a SECOND Many2many on product.product (besides tag_ids).
        let prod = resolve_registered("product.product").unwrap();
        let combo = prod.fields.iter().find(|f| f.name == "product_template_attribute_value_ids").unwrap();
        assert!(matches!(
            combo.kind,
            FieldKind::Many2many { target: "product.template.attribute.value", relation: "variant_ptav_rel", column: "product_id", target_column: "ptav_id" }
        ));
    }

    #[test]
    fn related_field_macro_registers() {
        // `#[field(related = "order_id.partner_id")]` emits a RelatedRegistration; non-related fields
        // return None.
        assert_eq!(related_path("sale.order.line", "order_partner_id"), Some("order_id.partner_id"));
        assert_eq!(related_path("sale.order.line", "price_unit"), None);
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
