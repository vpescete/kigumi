//! Application module `stock`: a headless inventory ledger.
//! Slice 1 (M17.1): the structural data — locations, warehouses and quants (on-hand per
//! product + location). Movements and the quant-update mechanism land in M17.2.

use meshble::prelude::*;

/// Module manifest: own version + framework compatibility range + module dependencies.
pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "stock",
    version: "1.0.0",
    framework: ">=0.1, <0.2",
    depends: &[
        ModuleDep { name: "base", req: "^1.0" },
        ModuleDep { name: "sales", req: "^1.0" },
        ModuleDep { name: "mail", req: "^1.0" },
    ],
    summary: "Inventory — locations, quants, pickings and moves",
};
meshble::register_module!(MANIFEST);

/// A stock location (Odoo's `stock.location`): a place stock sits. `usage` drives behavior — only
/// `internal` counts as real on-hand; supplier/customer/inventory are virtual (infinite) source/sinks.
#[model(name = "stock.location", table = "stock_location")]
pub struct StockLocation {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Type", required, default = "internal", selection = "internal:Internal,supplier:Vendor,customer:Customer,inventory:Inventory Loss,transit:Transit")]
    usage: Selection,

    #[field(label = "Parent Location", target = "stock.location")]
    parent_id: Many2one,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// A warehouse (Odoo's `stock.warehouse`): an internal location with a short code. v1 keeps a single
/// main stock location per warehouse (no input/output/pack sub-locations).
#[model(name = "stock.warehouse", table = "stock_warehouse")]
pub struct StockWarehouse {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Code", required)]
    code: Text,

    #[field(label = "Stock Location", target = "stock.location")]
    location_id: Many2one,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// On-hand quantity of a product at a location (Odoo's `stock.quant`): the materialized stock level,
/// updated atomically when a move is done. Unique on (product_id, location_id) — enforced by a
/// migration index (`ensure_stock_indexes`), since composite uniques aren't a field attribute.
#[model(name = "stock.quant", table = "stock_quant")]
pub struct StockQuant {
    #[field(label = "Product", required, target = "product.product")]
    product_id: Many2one,

    #[field(label = "Location", required, target = "stock.location")]
    location_id: Many2one,

    #[field(label = "Quantity", default = "0")]
    quantity: Decimal,

    // How much of `quantity` is claimed by draft transfers (reserve_picking). available = quantity -
    // reserved_quantity; validating a move frees the reservation it held as the goods leave.
    #[field(label = "Reserved", default = "0")]
    reserved_quantity: Decimal,
}

// stock.picking carries a chatter thread (transfer history) and a tracked state.
meshble::register_mailed!("stock.picking");

/// A transfer (Odoo's `stock.picking`): a document grouping moves from a source to a destination
/// location. `validate` (the cross-record service method) sets its moves done and updates the quants.
#[model(name = "stock.picking", table = "stock_picking")]
pub struct StockPicking {
    #[field(label = "Reference", default = "/")]
    name: Text,

    #[field(label = "Type", required, default = "internal", selection = "receipt:Receipt,delivery:Delivery,internal:Internal Transfer")]
    picking_type: Selection,

    #[field(label = "Partner", target = "res.partner")]
    partner_id: Many2one,

    #[field(label = "Source Location", required, target = "stock.location")]
    location_id: Many2one,

    #[field(label = "Destination", required, target = "stock.location")]
    location_dest_id: Many2one,

    #[field(label = "Status", required, default = "draft", tracked, selection = "draft:Draft,done:Done,cancel:Cancelled")]
    state: Selection,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    // When a transfer is validated short, the unfulfilled remainder spills into a new draft transfer
    // linked back here.
    #[field(label = "Back Order of", target = "stock.picking")]
    backorder_id: Many2one,

    #[field(label = "Moves", target = "stock.move", inverse = "picking_id")]
    move_ids: One2many,
}

/// One product movement within a transfer (Odoo's `stock.move`): moves `product_uom_qty` of a product
/// from a source to a destination location. Locations default from the picking; done when validated.
#[model(name = "stock.move", table = "stock_move")]
pub struct StockMove {
    #[field(label = "Transfer", required, target = "stock.picking")]
    picking_id: Many2one,

    #[field(label = "Product", required, target = "product.product")]
    product_id: Many2one,

    // The quantity in the MOVE's unit of measure (product_uom_id). The quant always stores the product
    // REFERENCE unit, so reserve/validate convert: qty_ref = product_uom_qty * uom factor.
    #[field(label = "Quantity", required, default = "0")]
    product_uom_qty: Decimal,

    // The move's unit of measure. Absent => the product reference unit (factor 1, no conversion).
    #[field(label = "Unit of Measure", target = "uom.uom")]
    product_uom_id: Many2one,

    // The quantity actually processed at validation, in the MOVE unit. 0 means "process the full ordered
    // quantity" (the all-or-nothing default); a smaller value validates a partial transfer and backorders
    // the rest.
    #[field(label = "Done", default = "0")]
    quantity_done: Decimal,

    // How much on-hand this move has claimed at its source via reserve_picking, in the product REFERENCE
    // unit (it mirrors the quant). Validating the move frees this exact amount from the source quant's
    // reserved_quantity as the goods move out.
    #[field(label = "Reserved", default = "0")]
    reserved_qty: Decimal,

    #[field(label = "Source Location", required, target = "stock.location")]
    location_id: Many2one,

    #[field(label = "Destination", required, target = "stock.location")]
    location_dest_id: Many2one,

    #[field(label = "Status", required, default = "draft", selection = "draft:Draft,done:Done")]
    state: Selection,
}

/// Access control. `stock.user` (operator) runs transfers and their moves; configuration — locations,
/// warehouses, and editing quants directly — is reserved to `stock.manager`. Quants are normally
/// maintained by the move-done mechanism, not by hand.
pub static ACLS: &[Acl] = &[
    Acl { model: "stock.location", group: "stock.user", read: true, write: false, create: false, delete: false },
    Acl { model: "stock.location", group: "stock.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "stock.warehouse", group: "stock.user", read: true, write: false, create: false, delete: false },
    Acl { model: "stock.warehouse", group: "stock.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "stock.quant", group: "stock.user", read: true, write: false, create: false, delete: false },
    Acl { model: "stock.quant", group: "stock.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "stock.picking", group: "stock.user", read: true, write: true, create: true, delete: false },
    Acl { model: "stock.picking", group: "stock.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "stock.move", group: "stock.user", read: true, write: true, create: true, delete: true },
    Acl { model: "stock.move", group: "stock.manager", read: true, write: true, create: true, delete: true },
];

/// A done transfer's moves are frozen — no write, create or delete (only sudo or reverting can touch
/// them), the stock analogue of a posted journal entry. Covers the direct and nested move paths.
fn move_picking_not_done() -> Domain {
    Domain::field("picking_id.state").ne("done")
}

pub static RECORD_RULES: &[RecordRule] = &[
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Write], domain: RuleDomain::Static(move_picking_not_done) },
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Create], domain: RuleDomain::Static(move_picking_not_done) },
    RecordRule { model: "stock.move", groups: &[], ops: &[Operation::Delete], domain: RuleDomain::Static(move_picking_not_done) },
];

meshble::register_acls!(ACLS);
meshble::register_rules!(RECORD_RULES);

// Form views: how each model is laid out on a form. The header carries identity + status; the moves
// of a transfer live in a notebook page (the One2many the frontend renders inline).
meshble::register_view!(
    "stock.picking",
    &[
        FieldGroup {
            title: None,
            fields: &[
                FieldSlot { name: "name", full: true },
                FieldSlot { name: "picking_type", full: false },
                FieldSlot { name: "state", full: false },
                FieldSlot { name: "partner_id", full: false },
                FieldSlot { name: "company_id", full: false },
            ],
        },
        FieldGroup {
            title: Some("Locations"),
            fields: &[
                FieldSlot { name: "location_id", full: false },
                FieldSlot { name: "location_dest_id", full: false },
            ],
        },
    ],
    &[NotebookPage { title: "Moves", fields: &["move_ids"] }]
);

meshble::register_view!(
    "stock.move",
    &[FieldGroup {
        title: None,
        fields: &[
            FieldSlot { name: "picking_id", full: false },
            FieldSlot { name: "product_id", full: false },
            FieldSlot { name: "product_uom_qty", full: false },
            FieldSlot { name: "state", full: false },
            FieldSlot { name: "location_id", full: false },
            FieldSlot { name: "location_dest_id", full: false },
        ],
    }],
    &[]
);

meshble::register_view!(
    "stock.location",
    &[FieldGroup {
        title: None,
        fields: &[
            FieldSlot { name: "name", full: true },
            FieldSlot { name: "usage", full: false },
            FieldSlot { name: "parent_id", full: false },
            FieldSlot { name: "company_id", full: false },
            FieldSlot { name: "active", full: false },
        ],
    }],
    &[]
);

meshble::register_view!(
    "stock.warehouse",
    &[FieldGroup {
        title: None,
        fields: &[
            FieldSlot { name: "name", full: true },
            FieldSlot { name: "code", full: false },
            FieldSlot { name: "location_id", full: false },
            FieldSlot { name: "company_id", full: false },
            FieldSlot { name: "active", full: false },
        ],
    }],
    &[]
);

meshble::register_view!(
    "stock.quant",
    &[FieldGroup {
        title: None,
        fields: &[
            FieldSlot { name: "product_id", full: false },
            FieldSlot { name: "location_id", full: false },
            FieldSlot { name: "quantity", full: false },
        ],
    }],
    &[]
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_compatible_with_framework() {
        assert!(check_compat(&MANIFEST, FRAMEWORK_VERSION).is_ok());
    }

    #[test]
    fn models_resolve() {
        assert_eq!(StockLocation::descriptor().name, "stock.location");
        assert_eq!(StockLocation::descriptor().fields.len(), 5);
        assert_eq!(StockWarehouse::descriptor().name, "stock.warehouse");
        assert_eq!(StockQuant::descriptor().fields.len(), 4);
    }

    #[test]
    fn stock_views_reference_real_fields() {
        for model in ["stock.picking", "stock.move", "stock.location", "stock.warehouse", "stock.quant"] {
            let m = resolve_registered(model).unwrap();
            let names: Vec<&str> = m.fields.iter().map(|f| f.name).collect();
            let v = view_for(model).unwrap_or_else(|| panic!("{model} has no form view"));
            for g in v.groups {
                for s in g.fields {
                    assert!(names.contains(&s.name), "{model} view slot '{}' is not a real field", s.name);
                }
            }
            for p in v.pages {
                for f in p.fields {
                    assert!(names.contains(f), "{model} view page field '{f}' is not a real field");
                }
            }
        }
    }
}
