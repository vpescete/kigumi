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

    // Invoicing seam: confirm sets it To Invoice; `create_invoice` flips it to Invoiced (no account.move
    // in v1 — the full account module fills in the real posting behind this exact field/action).
    #[field(label = "Invoice Status", required, default = "no", tracked, selection = "no:Nothing to Invoice,to_invoice:To Invoice,invoiced:Fully Invoiced")]
    invoice_status: Selection,

    #[field(label = "Currency", required, target = "res.currency")]
    currency_id: Many2one,

    // Optional pricelist; `apply_pricelist` resolves each line's unit price against it (same currency).
    #[field(label = "Pricelist", target = "product.pricelist")]
    pricelist_id: Many2one,

    // Optional payment term; `create_sale_invoice` sets the invoice due date to today + term.days.
    #[field(label = "Payment Terms", target = "account.payment.term")]
    payment_term_id: Many2one,

    // Optional fiscal position; `apply_taxes` remaps each line's taxes through it before computing.
    #[field(label = "Fiscal Position", target = "account.fiscal.position")]
    fiscal_position_id: Many2one,

    // The amount split, each an exact aggregate over the lines (One2many cascade).
    #[field(label = "Untaxed Amount", compute = "compute_amount_untaxed", depends = "line_ids.price_subtotal", currency = "currency_id", store)]
    amount_untaxed: Decimal,

    #[field(label = "Taxes", compute = "compute_amount_tax", depends = "line_ids.price_tax", currency = "currency_id", store)]
    amount_tax: Decimal,

    #[field(label = "Total", compute = "compute_amount", depends = "line_ids.price_total", currency = "currency_id", store)]
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

    // Traceability (Odoo's `tracking`): whether stock of this product is tracked by lot, by unique serial
    // number, or not at all. A tracked product's moves must carry a lot/serial.
    #[field(label = "Tracking", default = "none", selection = "none:No Tracking,lot:By Lots,serial:By Unique Serial Number")]
    tracking: Selection,

    #[field(label = "Sales Price", default = "0", tracked)]
    list_price: Decimal,

    #[field(label = "Cost", default = "0", tracked)]
    standard_price: Decimal,

    // Default customer taxes; an order line seeds its `tax_ids` from these when the product is picked.
    #[field(label = "Customer Taxes", target = "account.tax", relation = "product_template_tax_rel", column = "product_id", target_column = "tax_id")]
    taxes_id: Many2many,

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

    // The variant's price surcharge = SUM of its combo's PTAV price_extra. The compute engine can't
    // aggregate over a Many2many on read, so this is MATERIALIZED (stored) by the generation engine and
    // a PTAV-price_extra-edit hook — the M2M analogue of recompute_columns_on. Engine-maintained.
    #[field(label = "Variant Extra Price", default = "0", groups = "base.system")]
    price_extra: Decimal,

    // The variant's effective sales price = template list_price (delegated) + the variant surcharge.
    // Same-record on-read compute (both inputs are on the record: list_price delegated, price_extra own).
    #[field(label = "Effective Price", compute = "variant_lst_price", depends = "list_price,price_extra")]
    lst_price: Decimal,

    // On-hand quantity across internal stock locations. Materialized by the stock module's validate
    // mechanism (raw SQL, which bypasses the secured-write readonly guard); visible but never
    // hand-edited. Stays 0 until stock is installed and stock is moved.
    #[field(label = "On Hand", default = "0", readonly)]
    qty_available: Decimal,

    // The variant's OWN active flag, intentionally shadowing product.template.active: archiving one
    // variant (the generator does this when a combination is no longer selected) must not touch the
    // shared template or the other variants. Read/written on product.product, never delegated.
    #[field(label = "Active", default = "true")]
    active: Bool,

    // First-class Many2many: variant tags through a junction table.
    #[field(label = "Tags", target = "product.tag", relation = "product_product_tag_rel", column = "product_id", target_column = "tag_id")]
    tag_ids: Many2many,

    // The exact attribute combination this variant represents: the set of `product.template.attribute.value`
    // rows (one per attribute line). Engine-LOCKED (groups = base.system): only the generation engine
    // (sudo) sets it — a manager writing it would corrupt the combo identity AND leave the materialized
    // price_extra stale, so the lock closes both. The combo key derived from it is how regeneration
    // recognises an existing variant (so it is kept/reactivated, never duplicated).
    #[field(label = "Attribute Values", target = "product.template.attribute.value", relation = "variant_ptav_rel", column = "product_id", target_column = "ptav_id", groups = "base.system")]
    product_template_attribute_value_ids: Many2many,
}

/// A product tag/label (the comodel of `product.product.tag_ids`).
#[model(name = "product.tag", table = "product_tag")]
pub struct ProductTag {
    #[field(label = "Name", required, unique)]
    name: Text,
}

/// A pricelist (Odoo's `product.pricelist`): a named set of price rules in one currency. A sale order
/// references a pricelist; `apply_pricelist` resolves each line's unit price against its items.
#[model(name = "product.pricelist", table = "product_pricelist")]
pub struct ProductPricelist {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Currency", required, target = "res.currency")]
    currency_id: Many2one,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// A pricelist rule (Odoo's `product.pricelist.item`). `applied_on` scopes the rule (most specific
/// wins: variant > product > category > global); `compute_price` is a fixed price or a percentage
/// discount off `base` (the variant's sales price or cost). `min_quantity` + the date window gate it.
/// The flat subset: no formula/markup, no pricelist chaining, single currency (no FX) — those are later.
#[model(name = "product.pricelist.item", table = "product_pricelist_item")]
pub struct ProductPricelistItem {
    #[field(label = "Pricelist", required, target = "product.pricelist")]
    pricelist_id: Many2one,

    // Scope, most-specific first. Odoo's 4 levels, kept as sortable string keys.
    #[field(label = "Applied On", default = "3_global", selection = "0_product_variant:Variant,1_product:Product,2_product_category:Category,3_global:All Products")]
    applied_on: Selection,

    #[field(label = "Category", target = "product.category")]
    categ_id: Many2one,

    #[field(label = "Product", target = "product.template")]
    product_tmpl_id: Many2one,

    #[field(label = "Variant", target = "product.product")]
    product_id: Many2one,

    #[field(label = "Min. Quantity", default = "0")]
    min_quantity: Decimal,

    #[field(label = "Compute Price", default = "fixed", selection = "fixed:Fixed Price,percentage:Discount")]
    compute_price: Selection,

    #[field(label = "Fixed Price", default = "0")]
    fixed_price: Decimal,

    #[field(label = "Discount %", default = "0")]
    percent_price: Decimal,

    #[field(label = "Based On", default = "list_price", selection = "list_price:Sales Price,standard_price:Cost")]
    base: Selection,

    #[field(label = "Start Date")]
    date_start: Date,

    #[field(label = "End Date")]
    date_end: Date,
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

/// A tax (Odoo's `account.tax`, minimal subset). A sale/purchase line references one tax via `tax_id`;
/// the line's stored `tax_rate` is what drives its tax computation (a stored compute can't read a
/// related row at write time). The full account module later ADOPTS this model via `#[extend]` (same
/// name, no migration). v1: single tax per line, percentage only, round-per-line.
#[model(name = "account.tax", table = "account_tax")]
pub struct AccountTax {
    #[field(label = "Tax Name", required)]
    name: Text,

    #[field(label = "Tax Scope", default = "sale", selection = "sale:Sales,purchase:Purchases,none:None")]
    type_tax_use: Selection,

    // `division` = price-included percentage (the price already contains the tax). The engine in
    // `apply_taxes` back-computes the net so subtotal + tax == the gross price exactly.
    #[field(label = "Tax Computation", default = "percent", selection = "percent:Percentage of Price,fixed:Fixed,division:Percentage of Price Tax Included")]
    amount_type: Selection,

    #[field(label = "Amount", default = "0")]
    amount: Decimal,

    // Apply order within a line's tax set; lower runs first (matters for compounding).
    #[field(label = "Sequence", default = "10")]
    sequence: Integer,

    // The tax is included in the unit price (the engine extracts it) rather than added on top.
    #[field(label = "Included in Price", default = "false")]
    price_include: Bool,

    // After this tax computes, fold its amount into the base of the taxes that follow (compound taxes).
    #[field(label = "Affect Base of Subsequent", default = "false")]
    include_base_amount: Bool,

    // Reporting/rollup bucket. The invoice emits one GL tax line per group.
    #[field(label = "Tax Group", target = "account.tax.group")]
    tax_group_id: Many2one,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// A tax group (Odoo's `account.tax.group`): the rollup bucket for taxes on an invoice — the GL emits one
/// tax line per group, and reports total tax per group. Kept in the sales module alongside `account.tax`.
#[model(name = "account.tax.group", table = "account_tax_group")]
pub struct AccountTaxGroup {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Sequence", default = "10")]
    sequence: Integer,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// A fiscal position (Odoo's `account.fiscal.position`): a set of tax-rewrite rules applied per order
/// (e.g. domestic VAT to export 0%). `apply_taxes` remaps each line's source taxes to their destination
/// before computing. v1: order-level only (a partner default and country auto-apply need a partner FK +
/// res.country, both deferred). Kept in sales so the order FK stays inside the sales dependency set.
#[model(name = "account.fiscal.position", table = "account_fiscal_position")]
pub struct AccountFiscalPosition {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    #[field(label = "Mappings", target = "account.fiscal.position.tax", inverse = "position_id")]
    tax_ids: One2many,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// Partner accounting defaults (Odoo's `property_*` fields), added to `res.partner` via #[extend] from
/// the sales module. They hold an account.payment.term / account.fiscal.position id but are plain
/// Integers, NOT Many2one: a real FK would form a cycle (payment.term -> res.company -> res.partner ->
/// payment.term), so referential integrity is traded for the partner-default convenience. An order with
/// no explicit payment term / fiscal position falls back to its partner's default (resolved by id).
#[extend("res.partner")]
pub struct PartnerAccounting {
    #[field(label = "Customer Payment Terms", default = "0")]
    property_payment_term_id: Integer,

    #[field(label = "Fiscal Position", default = "0")]
    property_account_position_id: Integer,
}

/// One source-to-destination tax rewrite within a fiscal position. A NULL destination drops the source
/// tax entirely (e.g. an export position removing domestic VAT).
#[model(name = "account.fiscal.position.tax", table = "account_fiscal_position_tax")]
pub struct AccountFiscalPositionTax {
    #[field(label = "Fiscal Position", required, target = "account.fiscal.position")]
    position_id: Many2one,

    #[field(label = "Tax on Product", required, target = "account.tax")]
    tax_src_id: Many2one,

    #[field(label = "Tax to Apply", target = "account.tax")]
    tax_dest_id: Many2one,
}

/// A payment term (Odoo's `account.payment.term`, single-line subset): the invoice due date is the
/// invoice date plus `days`. Referenced by `sale.order`; an absent term means due == invoice date.
/// Kept in the sales module alongside `account.tax` so `sale.order` can reference it without the
/// account module being in its dependency closure (the FK target table must exist when the column is added).
#[model(name = "account.payment.term", table = "account_payment_term")]
pub struct AccountPaymentTerm {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Days", default = "0")]
    days: Integer,

    #[field(label = "Active", default = "true")]
    active: Bool,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,
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

    #[field(label = "Disc.%", default = "0")]
    discount: Decimal,

    // Taxes are a Many2many (the user's selection, the audit + re-derivation source). `apply_taxes` reads
    // it (a stored compute cannot), runs the engine, and materializes the per-tax breakdown into
    // `tax_line_ids` + a back-compat blended `tax_rate`. The line computes read the breakdown One2many
    // (which IS loadable in a stored compute), falling back to `tax_rate` for un-applied legacy rows.
    #[field(label = "Taxes", target = "account.tax", relation = "sale_order_line_tax_rel", column = "line_id", target_column = "tax_id")]
    tax_ids: Many2many,

    #[field(label = "Tax Breakdown", target = "sale.order.line.tax", inverse = "line_id")]
    tax_line_ids: One2many,

    // Legacy single-tax reference + blended effective rate (kept for back-compat; superseded by tax_ids).
    #[field(label = "Tax", target = "account.tax")]
    tax_id: Many2one,

    #[field(label = "Tax Rate %", default = "0")]
    tax_rate: Decimal,

    #[field(label = "Subtotal", compute = "compute_line_subtotal", depends = "product_uom_qty,price_unit,discount,tax_line_ids.tax_amount", store)]
    price_subtotal: Decimal,

    #[field(label = "Tax", compute = "compute_line_tax", depends = "product_uom_qty,price_unit,discount,tax_rate,tax_line_ids.tax_amount", store)]
    price_tax: Decimal,

    #[field(label = "Total", compute = "compute_line_total", depends = "product_uom_qty,price_unit,discount,tax_rate,tax_line_ids.tax_amount", store)]
    price_total: Decimal,

    #[field(label = "Margin", compute = "compute_line_margin", depends = "price_unit,purchase_price,product_uom_qty,discount", store)]
    margin: Decimal,
}

/// One materialized per-tax row of a sale order line (the output of `apply_taxes`' engine): the tax that
/// applied, its rollup group, the base it was computed on, and the resulting amount. The line's stored
/// computes aggregate `tax_amount` over these rows; the invoice rolls them up per group into GL lines.
#[model(name = "sale.order.line.tax", table = "sale_order_line_tax")]
pub struct SaleOrderLineTax {
    #[field(label = "Line", required, target = "sale.order.line")]
    line_id: Many2one,

    #[field(label = "Sequence", default = "10")]
    sequence: Integer,

    #[field(label = "Tax", target = "account.tax")]
    tax_id: Many2one,

    #[field(label = "Tax Group", target = "account.tax.group")]
    tax_group_id: Many2one,

    #[field(label = "Base", default = "0")]
    base_amount: Decimal,

    #[field(label = "Tax Amount", default = "0")]
    tax_amount: Decimal,

    #[field(label = "Included in Price", default = "false")]
    is_price_include: Bool,
}

use rust_decimal::Decimal;
/// (1 - discount/100) — the net factor a line discount applies to its gross amount.
fn net_factor(i: &ComputeInput) -> Decimal {
    Decimal::ONE - i.decimal("discount") / Decimal::from(100)
}
/// A line's net amount = quantity × unit price × (1 - discount%). The shared base of subtotal/tax/total.
fn line_net(i: &ComputeInput) -> Decimal {
    i.decimal("product_uom_qty") * i.decimal("price_unit") * net_factor(i)
}

/// `amount_total` of an order = exact sum of its lines' taxed totals.
fn compute_amount(i: &ComputeInput) -> Value {
    Value::Decimal(i.sum_decimal("line_ids", "price_total"))
}
/// Untaxed amount = exact sum of the lines' (discounted) subtotals.
fn compute_amount_untaxed(i: &ComputeInput) -> Value {
    Value::Decimal(i.sum_decimal("line_ids", "price_subtotal"))
}
/// Taxes = exact sum of the lines' tax amounts.
fn compute_amount_tax(i: &ComputeInput) -> Value {
    Value::Decimal(i.sum_decimal("line_ids", "price_tax"))
}
/// `margin` of an order = exact sum of its lines' margins.
fn compute_margin(i: &ComputeInput) -> Value {
    Value::Decimal(i.sum_decimal("line_ids", "margin"))
}
/// The tax this line carries: the sum of its materialized breakdown rows (`apply_taxes` output) when any
/// exist, else a same-record FALLBACK of net × tax_rate% so an un-applied legacy row (tax_rate set, no
/// breakdown) keeps its old numbers. Reads only loadable inputs (own scalars + One2many children).
fn line_tax_amount(i: &ComputeInput) -> Decimal {
    if i.count("tax_line_ids") == 0 {
        line_net(i) * (i.decimal("tax_rate") / Decimal::from(100))
    } else {
        i.sum_decimal("tax_line_ids", "tax_amount")
    }
}
/// The portion of a line's tax that is INCLUDED in the price (price-included taxes). Subtracted from the
/// gross net to get the subtotal. Zero for exclusive taxes and for legacy fallback lines (no breakdown).
fn line_included_tax(i: &ComputeInput) -> Decimal {
    i.children("tax_line_ids")
        .iter()
        .filter(|c| matches!(c.get("is_price_include"), Some(Value::Bool(true))))
        .map(|c| match c.get("tax_amount") {
            Some(Value::Decimal(d)) => *d,
            Some(Value::Int(n)) => Decimal::from(*n),
            _ => Decimal::ZERO,
        })
        .sum()
}
/// A line's untaxed subtotal = the discounted net, less any price-INCLUDED tax (exclusive/legacy: the
/// included portion is 0, so subtotal == net, byte-identical to before).
fn compute_line_subtotal(i: &ComputeInput) -> Value {
    Value::Decimal(line_net(i) - line_included_tax(i))
}
/// A line's tax amount (breakdown sum, or the legacy net × rate% fallback).
fn compute_line_tax(i: &ComputeInput) -> Value {
    Value::Decimal(line_tax_amount(i))
}
/// A line's taxed total = subtotal + tax. For a price-included line this equals the gross net (the price
/// already contained the tax); for an exclusive/legacy line it is net + tax, as before.
fn compute_line_total(i: &ComputeInput) -> Value {
    Value::Decimal(line_net(i) - line_included_tax(i) + line_tax_amount(i))
}
/// A line's margin = (unit price − cost) × quantity × (1 - discount%). From the raw inputs (no chaining).
fn compute_line_margin(i: &ComputeInput) -> Value {
    Value::Decimal((i.decimal("price_unit") - i.decimal("purchase_price")) * i.decimal("product_uom_qty") * net_factor(i))
}
/// A variant's display name: the (inherited) template name, with the internal reference in parentheses
/// when one is set. On-read — reads the delegated `name` and the variant's own `default_code`.
fn product_display_name(i: &ComputeInput) -> Value {
    let name = i.str("name");
    let code = i.str("default_code");
    Value::Str(if code.is_empty() { name.to_string() } else { format!("{name} ({code})") })
}
meshble::register_compute!("product_display_name", product_display_name);
/// A variant's effective sales price: the (inherited) template list_price plus the variant's own
/// materialized surcharge. On-read, same-record (both inputs are on the variant record).
fn variant_lst_price(i: &ComputeInput) -> Value {
    Value::Decimal(i.decimal("list_price") + i.decimal("price_extra"))
}
meshble::register_compute!("variant_lst_price", variant_lst_price);
meshble::register_compute!("compute_amount", compute_amount);
meshble::register_compute!("compute_amount_untaxed", compute_amount_untaxed);
meshble::register_compute!("compute_amount_tax", compute_amount_tax);
meshble::register_compute!("compute_margin", compute_margin);
meshble::register_compute!("compute_line_subtotal", compute_line_subtotal);
meshble::register_compute!("compute_line_tax", compute_line_tax);
meshble::register_compute!("compute_line_total", compute_line_total);
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
    // Tax breakdown: materialized by apply_taxes under the caller, read for the line rollup (full CRUD).
    Acl { model: "sale.order.line.tax", group: "sales.user", read: true, write: true, create: true, delete: true },
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
    // Pricelists: everyone in sales reads (to apply them); managers maintain the rules.
    Acl { model: "product.pricelist", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "product.pricelist", group: "sales.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "product.pricelist.item", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "product.pricelist.item", group: "sales.manager", read: true, write: true, create: true, delete: true },
    // Taxes: read by everyone in sales (referenced on lines), maintained by managers.
    Acl { model: "account.tax", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "account.tax", group: "sales.manager", read: true, write: true, create: true, delete: true },
    // Tax groups + fiscal positions: read by everyone in sales (referenced), configured by managers.
    Acl { model: "account.tax.group", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "account.tax.group", group: "sales.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "account.fiscal.position", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "account.fiscal.position", group: "sales.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "account.fiscal.position.tax", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "account.fiscal.position.tax", group: "sales.manager", read: true, write: true, create: true, delete: true },
    // Payment terms: read by everyone in sales (referenced on orders), maintained by managers.
    Acl { model: "account.payment.term", group: "sales.user", read: true, write: false, create: false, delete: false },
    Acl { model: "account.payment.term", group: "sales.manager", read: true, write: true, create: true, delete: true },
    // Purchase orders + lines. v1 pragmatic: managed by the sales groups (a small team); dedicated
    // purchase.user/manager groups are a later refinement.
    Acl { model: "purchase.order", group: "sales.user", read: true, write: true, create: true, delete: false },
    Acl { model: "purchase.order", group: "sales.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "purchase.order.line", group: "sales.user", read: true, write: true, create: true, delete: true },
    Acl { model: "purchase.order.line.tax", group: "sales.user", read: true, write: true, create: true, delete: true },
    // Discount wizard (transient): opened, edited and applied by anyone in sales; the GC cron reclaims
    // the scratchpad rows, so no delete right is granted.
    Acl { model: "sale.order.discount", group: "sales.user", read: true, write: true, create: true, delete: false },
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
            .set("invoice_status", Value::Str("to_invoice".to_string()))
            .assign_sequence("name", "SO")),
        s => Err(format!("can only confirm a draft order (state is '{s}')")),
    }
}
meshble::register_action!("sale.order", "confirm", confirm_order, &["sales.user"]);

// Invoicing is now a cross-record service method that posts a real account.move (see
// `Db::create_sale_invoice` + `POST /api/sale.order/:id/create_invoice`), not a pure state action —
// `confirm` still flips invoice_status to "to_invoice", and create_invoice flips it to "invoiced" as a
// side effect of generating the move.

/// `done`: a confirmed sale is locked as done (its total then becomes read-only via the UI rule).
fn set_done(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "sale" => Ok(ActionOutcome::new().set("state", Value::Str("done".to_string()))),
        s => Err(format!("can only finish a confirmed order (state is '{s}')")),
    }
}
meshble::register_action!("sale.order", "done", set_done, &["sales.user"]);

/// A purchase order (Odoo's `purchase.order`): the buy-side mirror of sale.order, sharing the line
/// tax/total computes and the order amount aggregates. `confirm` assigns a PO number.
#[model(name = "purchase.order", table = "purchase_order")]
pub struct PurchaseOrder {
    #[field(label = "Order Reference", default = "New")]
    name: Text,

    #[field(label = "Vendor", required, target = "res.partner")]
    partner_id: Many2one,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    #[field(label = "Order Lines", target = "purchase.order.line", inverse = "order_id")]
    line_ids: One2many,

    #[field(label = "Status", required, default = "draft", selection = "draft:Draft,purchase:Confirmed,done:Done")]
    state: Selection,

    // Billing seam (mirror of sale.order): confirm sets it To Invoice; create_vendor_bill posts the bill
    // and flips it to Invoiced.
    #[field(label = "Billing Status", required, default = "no", selection = "no:Nothing to Bill,to_invoice:To Bill,invoiced:Fully Billed")]
    invoice_status: Selection,

    // Optional fiscal position; apply_purchase_taxes remaps each line's taxes through it before computing.
    #[field(label = "Fiscal Position", target = "account.fiscal.position")]
    fiscal_position_id: Many2one,

    #[field(label = "Currency", required, target = "res.currency")]
    currency_id: Many2one,

    #[field(label = "Untaxed Amount", compute = "compute_amount_untaxed", depends = "line_ids.price_subtotal", currency = "currency_id", store)]
    amount_untaxed: Decimal,

    #[field(label = "Taxes", compute = "compute_amount_tax", depends = "line_ids.price_tax", currency = "currency_id", store)]
    amount_tax: Decimal,

    #[field(label = "Total", compute = "compute_amount", depends = "line_ids.price_total", currency = "currency_id", store)]
    amount_total: Decimal,
}

/// A purchase order line — the same shape as sale.order.line (same field names → it reuses the line
/// tax/total compute functions verbatim).
#[model(name = "purchase.order.line", table = "purchase_order_line")]
pub struct PurchaseOrderLine {
    #[field(label = "Order", required, target = "purchase.order")]
    order_id: Many2one,

    #[field(label = "Product", required, target = "product.product")]
    product_id: Many2one,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    #[field(label = "Description")]
    name: Text,

    #[field(label = "Quantity", required, default = "1")]
    product_uom_qty: Decimal,

    #[field(label = "Unit Price", required, default = "0")]
    price_unit: Decimal,

    #[field(label = "Disc.%", default = "0")]
    discount: Decimal,

    // Taxes mirror sale.order.line: a Many2many source set + a materialized breakdown One2many that the
    // shared line computes read (apply_purchase_taxes fills them); legacy tax_id/tax_rate kept.
    #[field(label = "Taxes", target = "account.tax", relation = "purchase_order_line_tax_rel", column = "line_id", target_column = "tax_id")]
    tax_ids: Many2many,

    #[field(label = "Tax Breakdown", target = "purchase.order.line.tax", inverse = "line_id")]
    tax_line_ids: One2many,

    #[field(label = "Tax", target = "account.tax")]
    tax_id: Many2one,

    #[field(label = "Tax Rate %", default = "0")]
    tax_rate: Decimal,

    #[field(label = "Subtotal", compute = "compute_line_subtotal", depends = "product_uom_qty,price_unit,discount,tax_line_ids.tax_amount", store)]
    price_subtotal: Decimal,

    #[field(label = "Tax", compute = "compute_line_tax", depends = "product_uom_qty,price_unit,discount,tax_rate,tax_line_ids.tax_amount", store)]
    price_tax: Decimal,

    #[field(label = "Total", compute = "compute_line_total", depends = "product_uom_qty,price_unit,discount,tax_rate,tax_line_ids.tax_amount", store)]
    price_total: Decimal,
}

/// One materialized per-tax row of a purchase order line (the buy-side mirror of sale.order.line.tax).
#[model(name = "purchase.order.line.tax", table = "purchase_order_line_tax")]
pub struct PurchaseOrderLineTax {
    #[field(label = "Line", required, target = "purchase.order.line")]
    line_id: Many2one,

    #[field(label = "Sequence", default = "10")]
    sequence: Integer,

    #[field(label = "Tax", target = "account.tax")]
    tax_id: Many2one,

    #[field(label = "Tax Group", target = "account.tax.group")]
    tax_group_id: Many2one,

    #[field(label = "Base", default = "0")]
    base_amount: Decimal,

    #[field(label = "Tax Amount", default = "0")]
    tax_amount: Decimal,

    #[field(label = "Included in Price", default = "false")]
    is_price_include: Bool,
}

/// `confirm`: a draft purchase order becomes confirmed and gets its PO number (the buy-side mirror of
/// the sale confirm; Odoo requires a double validation for large POs — deferred).
fn confirm_purchase(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "draft" => Ok(ActionOutcome::new()
            .set("state", Value::Str("purchase".to_string()))
            .set("invoice_status", Value::Str("to_invoice".to_string()))
            .assign_sequence("name", "PO")),
        s => Err(format!("can only confirm a draft purchase order (state is '{s}')")),
    }
}
meshble::register_action!("purchase.order", "confirm", confirm_purchase, &["sales.user"]);

/// `done`: a confirmed purchase order is locked as received/done.
fn done_purchase(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "purchase" => Ok(ActionOutcome::new().set("state", Value::Str("done".to_string()))),
        s => Err(format!("can only finish a confirmed purchase order (state is '{s}')")),
    }
}
meshble::register_action!("purchase.order", "done", done_purchase, &["sales.user"]);

/// A discount wizard (Odoo's `sale.order.discount`): a transient scratchpad that applies a percentage
/// discount to every line of its target order. Opened with `order_id` seeded from the active record;
/// the `apply_discount` service method (slice 3) writes `discount` onto the lines.
#[model(name = "sale.order.discount", table = "sale_order_discount")]
pub struct SaleOrderDiscount {
    #[field(label = "Order", required, target = "sale.order")]
    order_id: Many2one,

    #[field(label = "Discount %", default = "0")]
    discount: Decimal,

    // GC timestamp: migration gives this a DEFAULT now(); the transient cron reclaims aged rows.
    #[field(label = "Created")]
    create_date: Datetime,
}
meshble::register_transient!("sale.order.discount");
meshble::register_wizard!("sale.order.discount", default_get_discount);

/// `default_get` for the discount wizard: seed `order_id` from the open context's active record. With
/// no active record the seed is empty (the required `order_id` then makes the open fail — by design).
fn default_get_discount(ctx: &WizardContext) -> Vec<(&'static str, Value)> {
    match ctx.active_id {
        Some(id) => vec![("order_id", Value::Int(id))],
        None => vec![],
    }
}

/// HTML-escapes a string for safe inclusion in a rendered report. Stored content (a line description,
/// the order reference) is untrusted, so it must be escaped or it is a stored-XSS vector — the minimal
/// entity set that neutralizes element/attribute injection.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Reads a JSON field as a display string (numbers and strings as-is, missing/null as a dash).
fn field_str(v: &serde_json::Value, key: &str) -> String {
    match v.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => "-".to_string(),
    }
}

/// The `quotation` report for sale.order: an HTML document with the order header, the line table and
/// the untaxed/tax/total summary. Reads only the fields `find_one_secured` returns (lines inlined) —
/// no recompute, no extra reads. Relations show as ids in v1 (display-name fields are a later add).
fn render_quotation(rec: &serde_json::Value) -> String {
    let order_ref = esc(&field_str(rec, "name"));
    let rows: String = rec
        .get("line_ids")
        .and_then(|v| v.as_array())
        .map(|lines| {
            lines
                .iter()
                .map(|l| {
                    format!(
                        "<tr><td>{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td></tr>",
                        esc(&field_str(l, "name")),
                        esc(&field_str(l, "product_uom_qty")),
                        esc(&field_str(l, "price_unit")),
                        esc(&field_str(l, "discount")),
                        esc(&field_str(l, "price_subtotal")),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let untaxed = esc(&field_str(rec, "amount_untaxed"));
    let tax = esc(&field_str(rec, "amount_tax"));
    let total = esc(&field_str(rec, "amount_total"));
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Quotation {order_ref}</title>\
         <style>body{{font-family:system-ui,sans-serif;margin:2rem;color:#111}}h1{{font-size:1.4rem}}\
         table{{width:100%;border-collapse:collapse;margin-top:1rem}}th,td{{padding:.4rem .6rem;border-bottom:1px solid #ddd;text-align:left}}\
         .r{{text-align:right}}tfoot td{{font-weight:600;border-top:2px solid #333}}</style></head>\
         <body><h1>Quotation {order_ref}</h1>\
         <table><thead><tr><th>Description</th><th class=\"r\">Qty</th><th class=\"r\">Unit Price</th><th class=\"r\">Disc.%</th><th class=\"r\">Subtotal</th></tr></thead>\
         <tbody>{rows}</tbody>\
         <tfoot>\
         <tr><td colspan=\"4\" class=\"r\">Untaxed</td><td class=\"r\">{untaxed}</td></tr>\
         <tr><td colspan=\"4\" class=\"r\">Tax</td><td class=\"r\">{tax}</td></tr>\
         <tr><td colspan=\"4\" class=\"r\">Total</td><td class=\"r\">{total}</td></tr>\
         </tfoot></table></body></html>"
    )
}
meshble::register_report!("sale.order", "quotation", "Quotation", render_quotation);

// Form layouts: a product variant and a sales order, grouped and tabbed instead of dumped in
// declaration order.
meshble::register_view!(
    "product.product",
    &[
        FieldGroup {
            title: None,
            fields: &[
                FieldSlot { name: "name", full: true },
                FieldSlot { name: "product_tmpl_id", full: false },
                FieldSlot { name: "default_code", full: false },
                FieldSlot { name: "barcode", full: false },
                FieldSlot { name: "active", full: false },
            ],
        },
        FieldGroup {
            title: Some("Pricing"),
            fields: &[
                FieldSlot { name: "list_price", full: false },
                FieldSlot { name: "standard_price", full: false },
                FieldSlot { name: "lst_price", full: false },
            ],
        },
        FieldGroup {
            title: Some("Classification"),
            fields: &[
                FieldSlot { name: "categ_id", full: false },
                FieldSlot { name: "uom_id", full: false },
                FieldSlot { name: "product_type", full: false },
                FieldSlot { name: "qty_available", full: false },
                FieldSlot { name: "tag_ids", full: true },
            ],
        },
        FieldGroup {
            title: Some("Description"),
            fields: &[FieldSlot { name: "image", full: true }, FieldSlot { name: "description", full: true }],
        },
    ],
    &[NotebookPage { title: "Attributes", fields: &["product_template_attribute_value_ids"] }]
);

meshble::register_view!(
    "sale.order",
    &[
        FieldGroup {
            title: None,
            fields: &[
                FieldSlot { name: "name", full: true },
                FieldSlot { name: "partner_id", full: false },
                FieldSlot { name: "currency_id", full: false },
                FieldSlot { name: "pricelist_id", full: false },
                FieldSlot { name: "company_id", full: false },
                FieldSlot { name: "state", full: false },
                FieldSlot { name: "invoice_status", full: false },
            ],
        },
        FieldGroup {
            title: Some("Amounts"),
            fields: &[
                FieldSlot { name: "amount_untaxed", full: false },
                FieldSlot { name: "amount_tax", full: false },
                FieldSlot { name: "amount_total", full: false },
                FieldSlot { name: "margin", full: false },
            ],
        },
    ],
    &[NotebookPage { title: "Order lines", fields: &["line_ids"] }]
);

meshble::register_view!(
    "purchase.order",
    &[
        FieldGroup {
            title: None,
            fields: &[
                FieldSlot { name: "name", full: true },
                FieldSlot { name: "partner_id", full: false },
                FieldSlot { name: "currency_id", full: false },
                FieldSlot { name: "company_id", full: false },
                FieldSlot { name: "state", full: false },
            ],
        },
        FieldGroup {
            title: Some("Amounts"),
            fields: &[
                FieldSlot { name: "amount_untaxed", full: false },
                FieldSlot { name: "amount_tax", full: false },
                FieldSlot { name: "amount_total", full: false },
            ],
        },
    ],
    &[NotebookPage { title: "Order lines", fields: &["line_ids"] }]
);

// Inline-table columns for the order lines: the customer-facing fields, in order. The order/company/
// cost/margin fields are intentionally omitted so the inline grid stays readable.
meshble::register_view!(
    "sale.order.line",
    &[FieldGroup {
        title: None,
        fields: &[
            FieldSlot { name: "product_id", full: false },
            FieldSlot { name: "name", full: false },
            FieldSlot { name: "product_uom_qty", full: false },
            FieldSlot { name: "price_unit", full: false },
            FieldSlot { name: "discount", full: false },
            FieldSlot { name: "price_subtotal", full: false },
            FieldSlot { name: "price_tax", full: false },
            FieldSlot { name: "price_total", full: false },
        ],
    }],
    &[]
);

meshble::register_view!(
    "purchase.order.line",
    &[FieldGroup {
        title: None,
        fields: &[
            FieldSlot { name: "product_id", full: false },
            FieldSlot { name: "name", full: false },
            FieldSlot { name: "product_uom_qty", full: false },
            FieldSlot { name: "price_unit", full: false },
            FieldSlot { name: "discount", full: false },
            FieldSlot { name: "price_subtotal", full: false },
            FieldSlot { name: "price_tax", full: false },
            FieldSlot { name: "price_total", full: false },
        ],
    }],
    &[]
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotation_report_renders_lines_and_escapes_stored_content() {
        let rec = serde_json::json!({
            "name": "SO/00001",
            "amount_untaxed": "180.00", "amount_tax": "0", "amount_total": "180.00",
            "line_ids": [
                { "name": "Widget <script>", "product_uom_qty": "2", "price_unit": "100", "discount": "10", "price_subtotal": "180.00" }
            ]
        });
        let html = render_quotation(&rec);
        assert!(html.contains("Quotation SO/00001"), "header carries the order reference");
        assert!(html.contains("180.00"), "totals are rendered");
        // The crux: untrusted stored content is HTML-escaped (no stored-XSS through a line description).
        assert!(html.contains("Widget &lt;script&gt;"), "stored content is escaped");
        assert!(!html.contains("Widget <script>"), "no unescaped markup leaks through");
    }

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
        // On the VARIANT: the combo M2M and the materialized price_extra are engine-locked too — only
        // the generation engine writes them. lst_price (the effective price) stays readable.
        assert_eq!(field_required_groups("product.product", "product_template_attribute_value_ids"), Some(&["base.system"][..]));
        assert_eq!(field_required_groups("product.product", "price_extra"), Some(&["base.system"][..]));
        assert_eq!(field_required_groups("product.product", "lst_price"), None);
    }

    #[test]
    fn on_hand_is_readonly_and_visible() {
        // qty_available (on-hand) is materialized by the stock validate mechanism: visible to everyone
        // (no group gate) but read-only, so it is shown yet never hand-edited.
        assert!(field_is_readonly("product.product", "qty_available"), "on-hand is read-only");
        assert!(field_required_groups("product.product", "qty_available").is_none(), "on-hand is not group-gated");
        assert!(!field_is_readonly("product.product", "lst_price"), "a normal field is writable");
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
    fn line_discount_and_tax_computes() {
        use meshble::prelude::{Children, ComputeInput};
        use rust_decimal::Decimal;
        use std::collections::BTreeMap;
        use std::str::FromStr;
        let mut v: BTreeMap<String, Value> = BTreeMap::new();
        v.insert("product_uom_qty".into(), Value::Decimal(Decimal::from(2)));
        v.insert("price_unit".into(), Value::Decimal(Decimal::from(100)));
        v.insert("discount".into(), Value::Decimal(Decimal::from(10)));
        v.insert("tax_rate".into(), Value::Decimal(Decimal::from(22)));
        v.insert("purchase_price".into(), Value::Decimal(Decimal::from(60)));
        let children = Children::new();
        let i = ComputeInput::new(&v, &children);
        // net = 2 * 100 * (1 - 10%) = 180
        assert_eq!(compute_line_subtotal(&i), Value::Decimal(Decimal::from(180)));
        assert_eq!(compute_line_tax(&i), Value::Decimal(Decimal::from_str("39.60").unwrap())); // 180 * 22%
        assert_eq!(compute_line_total(&i), Value::Decimal(Decimal::from_str("219.60").unwrap())); // 180 * 1.22
        assert_eq!(compute_line_margin(&i), Value::Decimal(Decimal::from(72))); // (100-60)*2*0.9
    }

    #[test]
    fn macro_generates_correct_descriptor() {
        // The macro must produce the SAME descriptor as the hand-written version.
        let d = SaleOrder::descriptor();
        assert_eq!(d.name, "sale.order");
        // name, partner_id, company_id, line_ids, state, invoice_status, currency_id, pricelist_id, payment_term_id, fiscal_position_id, amount_untaxed, amount_tax, amount_total
        assert_eq!(d.fields.len(), 13);
        let total = d.fields.iter().find(|f| f.name == "amount_total").unwrap();
        assert!(total.stored, "computed with `store` must be stored");
        assert_eq!(total.compute, Some("compute_amount"));
        assert_eq!(total.depends, &["line_ids.price_total"]);
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
