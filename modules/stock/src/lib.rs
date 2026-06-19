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
}

/// Access control. `stock.user` (operator) reads/writes the operational data; configuration — locations,
/// warehouses, and editing quants directly — is reserved to `stock.manager`. Quants are normally
/// maintained by the move-done mechanism, not by hand.
pub static ACLS: &[Acl] = &[
    Acl { model: "stock.location", group: "stock.user", read: true, write: false, create: false, delete: false },
    Acl { model: "stock.location", group: "stock.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "stock.warehouse", group: "stock.user", read: true, write: false, create: false, delete: false },
    Acl { model: "stock.warehouse", group: "stock.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "stock.quant", group: "stock.user", read: true, write: false, create: false, delete: false },
    Acl { model: "stock.quant", group: "stock.manager", read: true, write: true, create: true, delete: true },
];
meshble::register_acls!(ACLS);

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
        assert_eq!(StockQuant::descriptor().fields.len(), 3);
    }
}
