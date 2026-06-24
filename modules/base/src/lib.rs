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

/// A dated exchange rate (Odoo's `res.currency.rate`): `rate` units of `currency_id` per 1 unit of the
/// base/company currency, effective from `name` (the date). Conversion uses the latest rate on or before
/// the target date; the base currency simply has no rate rows (implicitly 1.0).
#[model(name = "res.currency.rate", table = "res_currency_rate")]
pub struct ResCurrencyRate {
    #[field(label = "Currency", required, target = "res.currency")]
    currency_id: Many2one,

    #[field(label = "Date", required)]
    name: Date,

    #[field(label = "Rate", required, default = "1")]
    rate: Decimal,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,
}

/// A country (Odoo's `res.country`): ISO master data referenced by partner / company addresses (and
/// later by fiscal-position auto-apply). Reference data — read by all, maintained by admin, seeded.
#[model(name = "res.country", table = "res_country")]
pub struct ResCountry {
    #[field(label = "Country Name", required)]
    name: Text,

    // ISO 3166-1 alpha-2 (IT, FR, …).
    #[field(label = "Country Code")]
    code: Text,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// A state / province within a country (Odoo's `res.country.state`).
#[model(name = "res.country.state", table = "res_country_state")]
pub struct ResCountryState {
    #[field(label = "State Name", required)]
    name: Text,

    #[field(label = "State Code")]
    code: Text,

    #[field(label = "Country", required, target = "res.country")]
    country_id: Many2one,

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

    // Free-text legacy country (kept for back-compat); country_id is the structured reference.
    #[field(label = "Country Code")]
    country_code: Text,

    #[field(label = "Country", target = "res.country")]
    country_id: Many2one,

    #[field(label = "State", target = "res.country.state")]
    state_id: Many2one,

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

    // Fiscal lock (M-lock): no journal entry dated on or before this date can be posted (set by an
    // admin via a sudo write). Optional — null means no lock.
    #[field(label = "Lock Date")]
    fiscalyear_lock_date: Date,

    #[field(label = "Active")]
    active: Bool,
}

/// Group (the `res.groups` analog): a named access group. READ-ONLY in the metamodel — authoritative
/// membership lives in the auth subsystem; this seeded list exists so the UI can show/relate groups
/// (pickers, filters). Seeded at migrate from the groups referenced by registered ACLs/rules.
#[model(name = "res.groups", table = "res_groups")]
pub struct ResGroups {
    #[field(label = "Name", required, unique)]
    name: Text,
}

/// User (the `res.users` analog): a READ-ONLY projection of the auth subsystem's `meshble_user`
/// table. An EXTERNAL table — the metamodel never migrates it (the auth subsystem owns it). The
/// password hash is simply not a field here, so it is never selected; credentials, refresh tokens
/// and the company scope stay in the auth subsystem. Exposes identity for the UI (user lists/pickers).
#[model(name = "res.users", table = "meshble_user")]
pub struct ResUsers {
    #[field(label = "Login", required, unique)]
    login: Text,

    #[field(label = "Groups")]
    groups: Text,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,
}
meshble::register_external!("res.users");

/// Attachment (Odoo's `ir.attachment`): a file attached to any record via a polymorphic
/// `(res_model, res_id)` link (no FK — like the mail thread). The bytes live in the content-addressed
/// blob store keyed by `checksum` (sha256); this row holds only metadata. Generic CRUD is admin-only —
/// uploads/downloads go through the gated `/attachments` endpoints, which run elevated after a host
/// access check (read to list/download, write to upload/delete).
#[model(name = "ir.attachment", table = "meshble_attachment")]
pub struct IrAttachment {
    #[field(label = "Name", required)]
    name: Text,

    /// The host record this file is attached to (polymorphic, no FK — like the mail thread).
    #[field(label = "Resource Model")]
    res_model: Text,

    #[field(label = "Resource ID")]
    res_id: Integer,

    #[field(label = "Mime Type")]
    mimetype: Text,

    #[field(label = "File Size")]
    file_size: Integer,

    /// sha256 of the content — the blob-store key. Identical bytes deduplicate to one blob.
    #[field(label = "Checksum")]
    checksum: Text,
}

/// Base ACLs: the everyday `user` group reads the foundational reference data and the group list;
/// the user directory (`res.users`) is admin-only (read-only — account changes go through auth).
/// `ir.attachment` generic CRUD is admin-only: end users reach files through the gated `/attachments`
/// endpoints (which check host access and run elevated), never the raw model.
pub static ACLS: &[Acl] = &[
    Acl { model: "res.currency", group: "user", read: true, write: false, create: false, delete: false },
    // Country master data: read by everyone, maintained by admin (seeded).
    Acl { model: "res.country", group: "user", read: true, write: false, create: false, delete: false },
    Acl { model: "res.country", group: "admin", read: true, write: true, create: true, delete: true },
    Acl { model: "res.country.state", group: "user", read: true, write: false, create: false, delete: false },
    Acl { model: "res.country.state", group: "admin", read: true, write: true, create: true, delete: true },
    Acl { model: "res.partner", group: "user", read: true, write: true, create: true, delete: false },
    Acl { model: "res.company", group: "user", read: true, write: false, create: false, delete: false },
    Acl { model: "res.groups", group: "user", read: true, write: false, create: false, delete: false },
    Acl { model: "res.users", group: "admin", read: true, write: false, create: false, delete: false },
    Acl { model: "ir.attachment", group: "admin", read: true, write: true, create: true, delete: true },
];
meshble::register_acls!(ACLS);

// Form layout: a partner with contact, address and accounting groups.
meshble::register_view!(
    "res.partner",
    &[
        FieldGroup {
            title: None,
            fields: &[
                FieldSlot { name: "name", full: true },
                FieldSlot { name: "is_company", full: false },
                FieldSlot { name: "parent_id", full: false },
                FieldSlot { name: "active", full: false },
            ],
        },
        FieldGroup {
            title: Some("Contact"),
            fields: &[FieldSlot { name: "email", full: false }, FieldSlot { name: "phone", full: false }],
        },
        FieldGroup {
            title: Some("Address"),
            fields: &[
                FieldSlot { name: "street", full: true },
                FieldSlot { name: "city", full: false },
                FieldSlot { name: "zip", full: false },
                FieldSlot { name: "country_code", full: false },
            ],
        },
        FieldGroup { title: Some("Accounting"), fields: &[FieldSlot { name: "currency_id", full: false }] },
    ],
    &[]
);

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
