//! Application module `account`: a headless double-entry general ledger.
//! Slice 1 (M16.1): the chart of accounts (`account.account`) + journals (`account.journal`).

use meshble::prelude::*;
use rust_decimal::Decimal;

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

// account.move opts into the mail subsystem: a journal entry carries a chatter audit trail, and its
// state transitions are tracked.
meshble::register_mailed!("account.move");

/// A journal entry / invoice (Odoo's `account.move`): the document that groups the debit/credit lines.
/// Mailed (audit trail); numbered "/" until posted. The balanced-entry invariant lives in `check_balanced`.
#[model(name = "account.move", table = "account_move")]
pub struct AccountMove {
    #[field(label = "Number", default = "/")]
    name: Text,

    #[field(label = "Type", required, default = "entry", selection = "entry:Journal Entry,out_invoice:Customer Invoice,in_invoice:Vendor Bill,out_refund:Customer Credit Note,in_refund:Vendor Refund")]
    move_type: Selection,

    #[field(label = "Date")]
    date: Date,

    // Due date for aging (M-aged): when the open balance is expected. Seeded = invoice date at creation
    // (no payment-terms engine yet); null rows age as "current".
    #[field(label = "Due Date")]
    invoice_date_due: Date,

    // Odoo's field is `ref`, a Rust keyword; the internal name is `reference`.
    #[field(label = "Reference")]
    reference: Text,

    #[field(label = "Journal", required, target = "account.journal")]
    journal_id: Many2one,

    #[field(label = "Partner", target = "res.partner")]
    partner_id: Many2one,

    #[field(label = "Status", required, default = "draft", tracked, selection = "draft:Draft,posted:Posted,cancel:Cancelled")]
    state: Selection,

    #[field(label = "Currency", target = "res.currency")]
    currency_id: Many2one,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,

    #[field(label = "Journal Items", target = "account.move.line", inverse = "move_id")]
    line_ids: One2many,

    // Entry total = Σ debit (== Σ credit when balanced) — the invoice/document amount. Stored aggregate.
    #[field(label = "Total", compute = "compute_move_total", depends = "line_ids.debit", currency = "currency_id", store)]
    amount_total: Decimal,

    // Settlement (M-pay): the open balance still due and the payment status. `amount_residual` is seeded
    // = amount_total when an invoice is created, then decremented by each registered payment; it is a
    // plain stored field (not a compute) because it tracks money received, not the GL line sum.
    #[field(label = "Payment Status", default = "not_paid", selection = "not_paid:Not Paid,partial:Partially Paid,paid:Paid")]
    payment_state: Selection,

    #[field(label = "Amount Due", currency = "currency_id", default = "0")]
    amount_residual: Decimal,

    #[field(label = "Reconciled", default = "false")]
    reconciled: Bool,
}

/// A journal item (Odoo's `account.move.line`): one posting to a GL account. A line is a debit XOR a
/// credit (two Decimal columns, Odoo's model); `balance` = debit − credit is derived on read.
#[model(name = "account.move.line", table = "account_move_line")]
pub struct AccountMoveLine {
    #[field(label = "Journal Entry", required, target = "account.move")]
    move_id: Many2one,

    #[field(label = "Account", required, target = "account.account")]
    account_id: Many2one,

    #[field(label = "Partner", target = "res.partner")]
    partner_id: Many2one,

    #[field(label = "Label")]
    name: Text,

    #[field(label = "Debit", default = "0")]
    debit: Decimal,

    #[field(label = "Credit", default = "0")]
    credit: Decimal,

    #[field(label = "Balance", compute = "compute_line_balance", depends = "debit,credit")]
    balance: Decimal,

    #[field(label = "Date")]
    date: Date,

    #[field(label = "Company", target = "res.company")]
    company_id: Many2one,
}

/// A line's signed balance, derived on read: debit − credit (same-record, never stored).
fn compute_line_balance(l: &ComputeInput) -> Value {
    Value::Decimal(l.decimal("debit") - l.decimal("credit"))
}
meshble::register_compute!("compute_line_balance", compute_line_balance);

/// A move's total = Σ of its lines' debit (equals Σ credit when balanced).
fn compute_move_total(m: &ComputeInput) -> Value {
    Value::Decimal(m.sum_decimal("line_ids", "debit"))
}
meshble::register_compute!("compute_move_total", compute_move_total);

/// The balanced-entry invariant (Odoo's `@api.constrains`): a move's total debit must equal its total
/// credit. Runs in-tx after the move + its lines are written; an empty move (Σ = 0) is balanced. This
/// is the canonical cross-record constraint a single-row SQL CHECK cannot express. NOTE: enforced on
/// move-level writes (create / nested line_ids); a posted move is additionally frozen in M16.3, which
/// is what guarantees the GL-level invariant "posted ⇒ balanced".
fn check_balanced(m: &ComputeInput) -> Result<(), String> {
    let debit: Decimal = m.sum_decimal("line_ids", "debit");
    let credit: Decimal = m.sum_decimal("line_ids", "credit");
    if debit != credit {
        return Err(format!("unbalanced journal entry: total debit {debit} != total credit {credit}"));
    }
    Ok(())
}
meshble::register_constraint!("account.move", &["line_ids"], check_balanced);

/// Multi-company coherence (Odoo's `_check_company`): a move must not mix companies. When both the
/// move and one of its lines carry an explicit company, they must match — so a multi-company user
/// cannot slip a company-B line into a company-A entry via the nested `line_ids` path.
/// KNOWN LIMITATION (deferred): a line's `account_id` pointing to a foreign-company GL account is NOT
/// caught here — the constraint sees the account id, not the account's company (a ConstraintFn has no
/// DB access). Closing that needs a company-aware FK validation or an account record rule.
fn check_line_companies(m: &ComputeInput) -> Result<(), String> {
    let Some(Value::Int(move_company)) = m.get("company_id") else {
        return Ok(()); // a shared (company-less) move imposes no per-line company
    };
    for line in m.children("line_ids") {
        if let Some(Value::Int(line_company)) = line.get("company_id") {
            if line_company != move_company {
                return Err(format!(
                    "a journal item belongs to another company ({line_company}) than its entry ({move_company})"
                ));
            }
        }
    }
    Ok(())
}
meshble::register_constraint!("account.move", &["line_ids"], check_line_companies);

/// Access control. `account.user` (accountant) reads accounts + journals and edits accounts, and runs
/// moves + their lines; configuration — creating accounts, all journal maintenance, deleting moves —
/// is reserved to `account.manager`.
pub static ACLS: &[Acl] = &[
    Acl { model: "account.account", group: "account.user", read: true, write: true, create: false, delete: false },
    Acl { model: "account.account", group: "account.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "account.journal", group: "account.user", read: true, write: false, create: false, delete: false },
    Acl { model: "account.journal", group: "account.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "account.move", group: "account.user", read: true, write: true, create: true, delete: false },
    Acl { model: "account.move", group: "account.manager", read: true, write: true, create: true, delete: true },
    Acl { model: "account.move.line", group: "account.user", read: true, write: true, create: true, delete: true },
    Acl { model: "account.move.line", group: "account.manager", read: true, write: true, create: true, delete: true },
];

/// `button_draft`: reset a posted or cancelled entry to draft (for correction). Posting is the
/// cross-record `Db::post_move` (it reads the journal sequence), but un-posting is a pure state flip.
fn reset_to_draft(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "posted" | "cancel" => Ok(ActionOutcome::new().set("state", Value::Str("draft".to_string()))),
        s => Err(format!("only a posted or cancelled entry can be reset to draft (state is '{s}')")),
    }
}
meshble::register_action!("account.move", "button_draft", reset_to_draft, &["account.user"]);

/// `button_cancel`: cancel a draft or posted entry.
fn cancel_move(i: &ActionInput) -> Result<ActionOutcome, String> {
    match i.str("state") {
        "draft" | "posted" => Ok(ActionOutcome::new().set("state", Value::Str("cancel".to_string()))),
        s => Err(format!("cannot cancel an entry in state '{s}'")),
    }
}
meshble::register_action!("account.move", "button_cancel", cancel_move, &["account.user"]);

/// Posted-entry immutability: a posted move's journal items are frozen — no write, create or delete
/// (only sudo, or un-posting first, can touch them). This is what guarantees the GL invariant
/// "posted ⇒ balanced": a balanced posted entry cannot be silently unbalanced afterwards. Read stays
/// unrestricted (posted entries must remain visible). Reaching `move_id.state` covers BOTH the direct
/// line path and the nested `line_ids` path (child writes re-check the child's record rules).
fn line_move_not_posted() -> Domain {
    Domain::field("move_id.state").ne("posted")
}

pub static RECORD_RULES: &[RecordRule] = &[
    RecordRule { model: "account.move.line", groups: &[], ops: &[Operation::Write], domain: RuleDomain::Static(line_move_not_posted) },
    RecordRule { model: "account.move.line", groups: &[], ops: &[Operation::Create], domain: RuleDomain::Static(line_move_not_posted) },
    RecordRule { model: "account.move.line", groups: &[], ops: &[Operation::Delete], domain: RuleDomain::Static(line_move_not_posted) },
];

meshble::register_acls!(ACLS);
meshble::register_rules!(RECORD_RULES);

// Form layout: the journal entry header in one group, its lines in a notebook page.
meshble::register_view!(
    "account.move",
    &[FieldGroup {
        title: None,
        fields: &[
            FieldSlot { name: "name", full: true },
            FieldSlot { name: "move_type", full: false },
            FieldSlot { name: "state", full: false },
            FieldSlot { name: "journal_id", full: false },
            FieldSlot { name: "date", full: false },
            FieldSlot { name: "reference", full: false },
            FieldSlot { name: "partner_id", full: false },
            FieldSlot { name: "currency_id", full: false },
            FieldSlot { name: "company_id", full: false },
            FieldSlot { name: "amount_total", full: false },
        ],
    }],
    &[NotebookPage { title: "Journal items", fields: &["line_ids"] }]
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
        // The macro-generated descriptors resolve cleanly (names, tables, field counts).
        let a = AccountAccount::descriptor();
        assert_eq!(a.name, "account.account");
        assert_eq!(a.fields.len(), 6); // code, name, account_type, reconcile, company_id, active
        let j = AccountJournal::descriptor();
        assert_eq!(j.name, "account.journal");
        assert_eq!(j.fields.len(), 7); // name, code, journal_type, company_id, default_account_id, sequence_code, active
    }
}
