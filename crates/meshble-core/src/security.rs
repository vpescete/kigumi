//! Security engine: model-level access control (ACL) + row-level record rules.
//!
//! Both are declarative data (like Odoo's `ir.model.access` and `ir.rule`), but record rules
//! are typed [`Domain`]s compiled to parameterized SQL — not `safe_eval`'d strings. `sudo` is an
//! explicit, typed escalation on [`Ctx`], not an easy-to-misuse method that silently bypasses checks.

use crate::Domain;

/// A CRUD operation subject to access control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Read,
    Write,
    Create,
    Delete,
}

/// Model-level access grant for one group (the `ir.model.access` analog).
#[derive(Clone, Copy, Debug)]
pub struct Acl {
    pub model: &'static str,
    pub group: &'static str,
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub delete: bool,
}

impl Acl {
    fn grants(&self, op: Operation) -> bool {
        match op {
            Operation::Read => self.read,
            Operation::Write => self.write,
            Operation::Create => self.create,
            Operation::Delete => self.delete,
        }
    }
}

/// Row-level rule (the `ir.rule` analog). `groups` empty = global (applies to everyone).
/// `domain` is a thunk because a [`Domain`] is not const-constructible.
#[derive(Clone, Copy)]
pub struct RecordRule {
    pub model: &'static str,
    pub groups: &'static [&'static str],
    pub ops: &'static [Operation],
    pub domain: fn() -> Domain,
}

/// Evaluation context: who is acting, and whether checks are bypassed.
///
/// The superuser flag is a PRIVATE field: external code cannot forge an elevated context with a
/// struct literal, so the only way to bypass access control is the greppable [`Ctx::sudo`].
#[derive(Clone, Debug)]
pub struct Ctx {
    pub uid: i64,
    pub groups: Vec<String>,
    su: bool,
}

impl Ctx {
    pub fn new(uid: i64, groups: Vec<String>) -> Self {
        Ctx { uid, groups, su: false }
    }
    /// Returns an elevated copy that bypasses access control. Explicit and greppable.
    pub fn sudo(&self) -> Ctx {
        Ctx { uid: self.uid, groups: self.groups.clone(), su: true }
    }
    pub fn is_member(&self, group: &str) -> bool {
        self.groups.iter().any(|g| g == group)
    }
}

/// Returns true if `ctx` may perform `op` on `model`. Access is granted if ANY of the user's
/// groups grants it (union semantics, like Odoo). Superuser is always allowed.
pub fn check_access(op: Operation, model: &str, ctx: &Ctx, acls: &[Acl]) -> bool {
    if ctx.su {
        return true;
    }
    acls.iter()
        .any(|a| a.model == model && ctx.is_member(a.group) && a.grants(op))
}

/// Combines the record rules applicable to (`op`, `model`, `ctx`) into a single restricting
/// [`Domain`], or `None` when nothing restricts (superuser, or no applicable rule).
///
/// Semantics (Odoo-compatible): global rules (no group) are ALL required → AND; the user's
/// applicable group rules are alternatives → OR; the two are then AND-ed together.
pub fn record_rule_domain(
    op: Operation,
    model: &str,
    ctx: &Ctx,
    rules: &[RecordRule],
) -> Option<Domain> {
    if ctx.su {
        return None;
    }
    let applicable = rules
        .iter()
        .filter(|r| r.model == model && r.ops.contains(&op));

    let mut globals: Vec<Domain> = Vec::new();
    let mut group_rules: Vec<Domain> = Vec::new();
    for r in applicable {
        if r.groups.is_empty() {
            globals.push((r.domain)());
        } else if r.groups.iter().any(|g| ctx.is_member(g)) {
            group_rules.push((r.domain)());
        }
    }

    let mut parts: Vec<Domain> = Vec::new();
    if let Some(d) = fold(globals, Domain::and) {
        parts.push(d);
    }
    if let Some(d) = fold(group_rules, Domain::or) {
        parts.push(d);
    }
    fold(parts, Domain::and)
}

/// Folds a list of domains with `combine`, returning `None` for an empty list.
fn fold(mut domains: Vec<Domain>, combine: fn(Domain, Domain) -> Domain) -> Option<Domain> {
    let mut acc = domains.pop()?;
    while let Some(d) = domains.pop() {
        acc = combine(d, acc);
    }
    Some(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    static ACLS: &[Acl] = &[Acl {
        model: "sale.order",
        group: "sales.user",
        read: true,
        write: true,
        create: true,
        delete: false,
    }];

    fn small_orders() -> Domain {
        Domain::field("amount_total").lt(10000_i64)
    }
    fn not_done() -> Domain {
        Domain::field("state").ne("done")
    }

    static RULES: &[RecordRule] = &[
        // Global rule: nobody reads "done" orders in this view.
        RecordRule { model: "sale.order", groups: &[], ops: &[Operation::Read], domain: not_done },
        // Group rule: juniors only see small orders.
        RecordRule {
            model: "sale.order",
            groups: &["sales.user"],
            ops: &[Operation::Read],
            domain: small_orders,
        },
    ];

    fn junior() -> Ctx {
        Ctx::new(7, vec!["sales.user".to_string()])
    }

    #[test]
    fn acl_grants_and_denies_per_operation() {
        let ctx = junior();
        assert!(check_access(Operation::Read, "sale.order", &ctx, ACLS));
        assert!(check_access(Operation::Write, "sale.order", &ctx, ACLS));
        assert!(!check_access(Operation::Delete, "sale.order", &ctx, ACLS));
    }

    #[test]
    fn acl_denies_unknown_group() {
        let ctx = Ctx::new(9, vec!["other".to_string()]);
        assert!(!check_access(Operation::Read, "sale.order", &ctx, ACLS));
    }

    #[test]
    fn sudo_bypasses_acl_and_rules() {
        let ctx = junior().sudo();
        assert!(check_access(Operation::Delete, "sale.order", &ctx, ACLS));
        assert!(record_rule_domain(Operation::Read, "sale.order", &ctx, RULES).is_none());
    }

    #[test]
    fn rules_combine_global_and_group() {
        let d = record_rule_domain(Operation::Read, "sale.order", &junior(), RULES).unwrap();
        // global (not_done) AND group (small_orders)
        let expected = not_done().and(small_orders());
        assert_eq!(d, expected);
    }

    #[test]
    fn only_global_applies_without_matching_group() {
        // A user in no rule-bearing group is still bound by the global rule, nothing more.
        let ctx = Ctx::new(1, vec!["other".to_string()]);
        let d = record_rule_domain(Operation::Read, "sale.order", &ctx, RULES).unwrap();
        assert_eq!(d, not_done());
    }

    #[test]
    fn no_rules_for_operation_means_no_restriction() {
        // No rule targets Write → unrestricted.
        assert!(record_rule_domain(Operation::Write, "sale.order", &junior(), RULES).is_none());
    }

    #[test]
    fn sudo_is_the_only_elevation_path() {
        // A freshly built context is never a superuser; only sudo() elevates. External crates
        // cannot construct `Ctx { su: true, .. }` at all because `su` is private.
        let normal = Ctx::new(1, vec![]);
        assert!(!check_access(Operation::Delete, "anything", &normal, &[]));
        let elevated = normal.sudo();
        assert!(check_access(Operation::Delete, "anything", &elevated, &[]));
    }
}
