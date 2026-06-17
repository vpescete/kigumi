//! Base module: foundational models that other modules reference.
//! It is the root of the dependency graph (no dependencies of its own).

use meshble::prelude::*;

/// Module manifest. `base` depends on nothing and anchors the catalog.
pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "base",
    version: "1.0.0",
    framework: ">=0.1, <0.2",
    depends: &[],
    summary: "Foundational models",
};
meshble::register_module!(MANIFEST);

/// Partner (company or individual), referenced by many other models.
#[model(name = "res.partner", table = "res_partner")]
pub struct ResPartner {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Email")]
    email: Text,
}

/// Currency used by monetary fields.
#[model(name = "res.currency", table = "res_currency")]
pub struct ResCurrency {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Symbol", required)]
    symbol: Text,
}

/// Base ACLs: the everyday `user` group can read the foundational reference data.
pub static ACLS: &[Acl] = &[
    Acl { model: "res.partner", group: "user", read: true, write: false, create: false, delete: false },
    Acl { model: "res.currency", group: "user", read: true, write: false, create: false, delete: false },
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
    fn partner_model_generates_table() {
        let m = resolve_registered("res.partner").expect("res.partner");
        let ddl = to_ddl(&m);
        assert!(ddl.contains("CREATE TABLE res_partner"));
        assert!(ddl.contains("name text NOT NULL"));
    }
}
