//! Application module `account`: a headless double-entry general ledger.
//! Slice 1 (M16.1): the chart of accounts (`account.account`) + journals (`account.journal`).

use meshble::prelude::*;

/// Module manifest: own version + framework compatibility range + module dependencies.
pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "account",
    version: "1.0.0",
    framework: ">=0.1, <0.2",
    depends: &[ModuleDep { name: "base", req: "^1.0" }, ModuleDep { name: "mail", req: "^1.0" }],
    summary: "Double-entry general ledger",
};
meshble::register_module!(MANIFEST);

/// A general-ledger account (Odoo's `account.account`): one line of the chart of accounts. Its
/// `account_type` drives downstream behavior (receivable/payable ledgers, income/expense, tax).
#[model(name = "account.account", table = "account_account")]
pub struct AccountAccount {
    #[field(label = "Code", required)]
    code: Text,

    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Type", required, default = "current_asset", selection = "receivable:Receivable,payable:Payable,bank_cash:Bank & Cash,current_asset:Current Asset,fixed_asset:Fixed Asset,current_liability:Current Liability,equity:Equity,income:Income,expense:Expense,tax:Tax")]
    account_type: Selection,

    #[field(label = "Allow Reconciliation", default = "false")]
    reconcile: Bool,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// A journal (Odoo's `account.journal`): where moves are booked; its `code`/`sequence_code` drive the
/// numbering of posted entries.
#[model(name = "account.journal", table = "account_journal")]
pub struct AccountJournal {
    #[field(label = "Name", required)]
    name: Text,

    #[field(label = "Code", required)]
    code: Text,

    // `type` is a Rust keyword, so the field is `journal_type` (Odoo's internal selection values).
    #[field(label = "Type", required, default = "general", selection = "sale:Sales,purchase:Purchase,cash:Cash,bank:Bank,general:Miscellaneous")]
    journal_type: Selection,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    #[field(label = "Default Account", target = "account.account")]
    default_account_id: Many2one,

    // The ir.sequence code used to number this journal's posted moves (e.g. "INV" -> INV/00001).
    #[field(label = "Sequence Code")]
    sequence_code: Text,

    #[field(label = "Active", default = "true")]
    active: Bool,
}

/// Access control. `account.user` (accountant) reads accounts + journals and edits accounts;
/// configuration — creating accounts, and all journal maintenance — is reserved to `account.manager`.
pub static ACLS: &[Acl] = &[
    Acl { model: "account.account", group: "account.user", read: true, write: true, create: false, delete: false },
    Acl { model: "account.account", group: "account.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "account.journal", group: "account.user", read: true, write: false, create: false, delete: false },
    Acl { model: "account.journal", group: "account.manager", read: true, write: true, create: true, delete: true },
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
        // The macro-generated descriptors resolve cleanly (names, tables, field counts).
        let a = AccountAccount::descriptor();
        assert_eq!(a.name, "account.account");
        assert_eq!(a.fields.len(), 6); // code, name, account_type, reconcile, company_id, active
        let j = AccountJournal::descriptor();
        assert_eq!(j.name, "account.journal");
        assert_eq!(j.fields.len(), 7); // name, code, journal_type, company_id, default_account_id, sequence_code, active
    }
}
