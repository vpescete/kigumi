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

/// The domain source of a [`RecordRule`]. A compile-time module rule uses `Static` (a thunk, because
/// a [`Domain`] is not const-constructible); a runtime DB-loaded rule (D12) uses `Owned`, holding a
/// domain parsed at load time. The engine treats both identically — only where the domain comes from
/// differs, so static and DB rules merge into one list with no special-casing.
#[derive(Clone)]
pub enum RuleDomain {
    Static(fn() -> Domain),
    Owned(Domain),
}

impl RuleDomain {
    /// Materializes the rule's domain (calls the thunk, or clones the owned value).
    pub fn resolve(&self) -> Domain {
        match self {
            RuleDomain::Static(f) => f(),
            RuleDomain::Owned(d) => d.clone(),
        }
    }
}

/// Row-level rule (the `ir.rule` analog). `groups` empty = global (applies to everyone).
#[derive(Clone)]
pub struct RecordRule {
    pub model: &'static str,
    pub groups: &'static [&'static str],
    pub ops: &'static [Operation],
    pub domain: RuleDomain,
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
    /// The active company (used to default `company_id` on create).
    pub company_id: Option<i64>,
    /// Companies the caller may access. EMPTY means "unrestricted" (the M2 stub, until res.users
    /// assigns per-user companies in M6); a non-empty set activates same-company data isolation.
    pub allowed_company_ids: Vec<i64>,
}

impl Ctx {
    pub fn new(uid: i64, groups: Vec<String>) -> Self {
        Ctx { uid, groups, su: false, company_id: None, allowed_company_ids: Vec::new() }
    }
    /// Returns an elevated copy that bypasses access control. Explicit and greppable.
    pub fn sudo(&self) -> Ctx {
        Ctx { su: true, ..self.clone() }
    }
    /// Sets the active company and the allowed set (the multi-company scope).
    pub fn in_companies(mut self, active: i64, allowed: Vec<i64>) -> Ctx {
        self.company_id = Some(active);
        self.allowed_company_ids = allowed;
        self
    }
    pub fn is_member(&self, group: &str) -> bool {
        self.groups.iter().any(|g| g == group)
    }
    /// True iff this is an elevated (superuser) context. Read-only view of the private `su` flag.
    pub fn is_su(&self) -> bool {
        self.su
    }
    /// True iff the caller is subject to company scoping — i.e. ANY non-superuser. A non-su caller is
    /// always company-restricted: with an allowed set they see those companies (plus shared rows);
    /// with an EMPTY set they see only shared (NULL-company) rows — default-deny, never "see
    /// everything". Only `sudo` is unrestricted. (Before M7 an empty set meant unrestricted; that
    /// god-mode stub is now closed, mirroring Odoo where `res.users.company_id` is required so an
    /// "unassigned, sees-all" user cannot exist and only the superuser bypasses company scoping.)
    pub fn company_scoped(&self) -> bool {
        !self.su
    }
}

/// Field-level access restriction (D6 / Odoo's `Field.groups`): the groups required to access one
/// `model.field`. Registered out-of-band (emitted by `#[field(groups = "...")]`) so it adds no
/// column to the metamodel and no churn to existing `FieldDef` literals — the same side-registry
/// pattern as external tables. Read AND write are gated by the same set, at the DB boundary.
pub struct FieldGroupRegistration {
    pub model: &'static str,
    pub field: &'static str,
    pub groups: &'static [&'static str],
}
inventory::collect!(FieldGroupRegistration);

/// The groups required to access `model.field`, or None when the field is unrestricted. Delegation-
/// aware (`_inherits`): a restriction on an inherited field lives on the PARENT model, so when the
/// field is not restricted directly on `model` we fall back to the parent — otherwise an inherited
/// field would expose its restricted value through the child (read/order/filter). Recurses the chain.
/// Shadow-aware: a name the child declares as its OWN column (e.g. `product.product.active` over
/// `product.template.active`) does NOT borrow the parent's restriction — only genuinely delegated
/// fields fall back, so a group added to the parent never silently gates the child's own column.
pub fn field_required_groups(model: &str, field: &str) -> Option<&'static [&'static str]> {
    if let Some(groups) = inventory::iter::<FieldGroupRegistration>
        .into_iter()
        .find(|r| r.model == model && r.field == field)
        .map(|r| r.groups)
    {
        return Some(groups);
    }
    if let Some((parent, _via)) = crate::inherits_of(model) {
        // The field is delegated only if the child has no own column for it (a shadow keeps its column).
        let is_own_column = crate::resolve_registered(model)
            .map(|m| m.fields.iter().any(|f| f.name == field))
            .unwrap_or(false);
        if !is_own_column {
            return field_required_groups(parent, field);
        }
    }
    None
}

/// Whether `ctx` may access (read or write) `model.field`. Default-allow when the field has no group
/// restriction; superuser always allowed; otherwise the caller must be in at least one required
/// group. Mirrors Odoo, which gates read and write by the same `Field.groups` set at the ORM boundary.
pub fn field_accessible(model: &str, field: &str, ctx: &Ctx) -> bool {
    if ctx.is_su() {
        return true;
    }
    match field_required_groups(model, field) {
        None => true,
        Some(groups) => groups.iter().any(|g| ctx.is_member(g)),
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
            globals.push(r.domain.resolve());
        } else if r.groups.iter().any(|g| ctx.is_member(g)) {
            group_rules.push(r.domain.resolve());
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
        RecordRule { model: "sale.order", groups: &[], ops: &[Operation::Read], domain: RuleDomain::Static(not_done) },
        // Group rule: juniors only see small orders.
        RecordRule {
            model: "sale.order",
            groups: &["sales.user"],
            ops: &[Operation::Read],
            domain: RuleDomain::Static(small_orders),
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
