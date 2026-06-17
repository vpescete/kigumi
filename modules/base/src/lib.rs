//! Base module: the foundational models other modules build on — currency, partner (contacts),
//! and company. Root of the dependency graph (no dependencies of its own).
//!
//! Multi-company: `res.company` is the unit of data isolation; transactional models carry a
//! `company_id` (added per model, e.g. sale.order). Partners are shared (no company_id) to avoid a
//! circular FK with res.company; company-specific contacts can be modelled later behind a deferred
//! FK. The active-company filtering rule lives in the security layer (Ctx.company + a record rule).

use meshble::prelude::*;

/// Module manifest. `base` depends on nothing and anchors the catalog.
pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "base",
    version: "1.0.0",
    framework: ">=0.1, <0.2",
    depends: &[],
    summary: "Foundational models: currency, partner, company",
};
meshble::register_module!(MANIFEST);

/// Currency used by monetary fields. Global (shared across companies).
#[model(name = "res.currency", table = "res_currency")]
pub struct ResCurrency {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Code", required, unique)]
    code: Text,

    #[field(label = "Symbol", required)]
    symbol: Text,

    #[field(label = "Decimal Places", default = "2", check = "decimal_places >= 0")]
    decimal_places: Integer,

    #[field(label = "Rounding")]
    rounding: Decimal,

    #[field(label = "Symbol Position", default = "after", selection = "before:Before amount,after:After amount")]
    position: Selection,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// Partner: a company or an individual — customers, suppliers, contacts. Referenced widely.
#[model(name = "res.partner", table = "res_partner")]
pub struct ResPartner {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Is a Company")]
    is_company: Bool,

    /// Contact hierarchy (a contact's parent company). Self-referential.
    #[field(label = "Related Company", target = "res.partner")]
    parent_id: Many2one,

    #[field(label = "Email")]
    email: Text,

    #[field(label = "Phone")]
    phone: Text,

    #[field(label = "Street")]
    street: Text,

    #[field(label = "City")]
    city: Text,

    #[field(label = "ZIP")]
    zip: Text,

    #[field(label = "Country Code")]
    country_code: Text,

    #[field(label = "Currency", target = "res.currency")]
    currency_id: Many2one,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// Company: the unit of multi-company data isolation. Has a linked partner (its own contact record).
#[model(name = "res.company", table = "res_company")]
pub struct ResCompany {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Currency", required, target = "res.currency")]
    currency_id: Many2one,

    #[field(label = "Partner", target = "res.partner")]
    partner_id: Many2one,

    #[field(label = "Active")]
    active: Bool,
}

/// Base ACLs: the everyday `user` group reads the foundational reference data.
pub static ACLS: &[Acl] = &[
    Acl { model: "res.currency", group: "user", read: true, write: false, create: false, delete: false },
    Acl { model: "res.partner", group: "user", read: true, write: true, create: true, delete: false },
    Acl { model: "res.company", group: "user", read: true, write: false, create: false, delete: false },
];
meshble::register_acls!(ACLS);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_manifest_is_compatible() {
        assert!(check_compat(&MANIFEST, FRAMEWORK_VERSION).is_ok());
    }

    #[test]
    fn models_resolve_and_generate_tables() {
        for (name, table) in
            [("res.currency", "res_currency"), ("res.partner", "res_partner"), ("res.company", "res_company")]
        {
            let m = resolve_registered(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            let ddl = to_ddl(&m);
            assert!(ddl.contains(&format!("CREATE TABLE {table}")), "{name} DDL");
        }
    }

    #[test]
    fn company_references_currency_and_partner() {
        let m = resolve_registered("res.company").unwrap();
        let ddl = to_ddl(&m);
        assert!(ddl.contains("REFERENCES res_currency"));
        assert!(ddl.contains("REFERENCES res_partner"));
    }
}
