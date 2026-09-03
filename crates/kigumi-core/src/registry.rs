//! Module catalog: models and extensions AUTO-REGISTER via `inventory`,
//! and the resolver merges them with no manual wiring.
//!
//! It is the equivalent of Odoo's `_inherit`, but at build/link time and with clean
//! boundaries: extensions are separate data, merged with conflict checks (`resolve`), not a
//! monkey-patch that mutates a class at runtime.

use crate::{
    resolve, resolve_module_set, validate_depends_with_extra, Acl, FieldDef, FieldKind,
    ModelDescriptor, ModuleManifest, RecordRule, ResolutionError, ResolvedModel, FRAMEWORK_VERSION,
};

/// Registration of a base model (emitted by `#[model]`).
pub struct ModelRegistration {
    pub name: &'static str,
    pub module: &'static str,
    pub descriptor: fn() -> &'static ModelDescriptor,
}
inventory::collect!(ModelRegistration);

/// Registration of a field extension (emitted by `#[extend]`).
pub struct FieldExtension {
    pub target: &'static str,
    pub module: &'static str,
    pub fields: fn() -> &'static [FieldDef],
}
inventory::collect!(FieldExtension);

/// Registration of a module manifest (emitted by `register_module!`). `crate_path` is the Rust
/// module path of the registering crate (`module_path!()`), used to map a model — whose own
/// registration carries `module_path!()` — back to its owning manifest (name + version).
pub struct ModuleRegistration {
    pub manifest: fn() -> ModuleManifest,
    pub crate_path: &'static str,
}
inventory::collect!(ModuleRegistration);

/// Registration of a module's ACLs (emitted by `register_acls!`). Compile-time security data; M6
/// adds a DB-backed loader on top, but the in-code registry stays the bootstrap/default source.
pub struct AclRegistration {
    pub acls: fn() -> &'static [Acl],
}
inventory::collect!(AclRegistration);

/// Registration of a module's record rules (emitted by `register_rules!`).
pub struct RecordRuleRegistration {
    pub rules: fn() -> &'static [RecordRule],
}
inventory::collect!(RecordRuleRegistration);

/// A module's document-numbering sequence (emitted by `register_sequence!`): declared next to the
/// action that consumes it, ensured at migrate — an existing sequence keeps its counter. `module`
/// is diagnostic only (a cross-module code collision is reported with both names).
pub struct SequenceRegistration {
    pub module: &'static str,
    pub code: &'static str,
    pub prefix: &'static str,
    pub suffix: &'static str,
    pub padding: i32,
}
inventory::collect!(SequenceRegistration);

/// All sequences registered across linked modules.
pub fn registered_sequences() -> Vec<&'static SequenceRegistration> {
    inventory::iter::<SequenceRegistration>.into_iter().collect()
}

/// A model whose table is owned OUTSIDE the metamodel (e.g. the auth subsystem's `kigumi_user`, or
/// a SQL view): registered so it is resolved/served like any model, but EXCLUDED from migration — the
/// metamodel never creates or alters its table. Odoo's `_auto = False`. Emitted by `register_external!`.
pub struct ExternalTable {
    pub model: &'static str,
}
inventory::collect!(ExternalTable);

/// Names of models whose tables the metamodel must NOT migrate (owned externally).
pub fn external_tables() -> Vec<&'static str> {
    inventory::iter::<ExternalTable>.into_iter().map(|e| e.model).collect()
}

/// A model that opts INTO the mail subsystem (chatter): it gains a thread of `mail.message`s,
/// followers and activities, addressed by the polymorphic `(res_model, res_id)` link. Emitted by
/// `register_mailed!`. Unlike Odoo's 5118-line `mail.thread` mixin, this is a one-line compile-time
/// marker the framework iterates — to gate the chatter API and to clean up the thread on delete.
pub struct MailedRegistration {
    pub model: &'static str,
}
inventory::collect!(MailedRegistration);

/// Names of all models that opted into the mail subsystem.
pub fn mailed_models() -> Vec<&'static str> {
    inventory::iter::<MailedRegistration>.into_iter().map(|e| e.model).collect()
}

/// Whether `model` has a mail thread (chatter enabled).
pub fn is_mailed(model: &str) -> bool {
    inventory::iter::<MailedRegistration>.into_iter().any(|e| e.model == model)
}

/// A model whose rows are ephemeral (Odoo's `TransientModel`): a wizard scratchpad with its own
/// table, served and secured like any model, but periodically garbage-collected by age. Emitted by
/// `register_transient!`. A transient model declares a nullable `create_date Datetime`; migration
/// gives that column a `DEFAULT now()` so every insert is timestamped and the GC cron can reclaim it.
pub struct TransientRegistration {
    pub model: &'static str,
}
inventory::collect!(TransientRegistration);

/// Names of all transient (ephemeral / wizard) models.
pub fn transient_models() -> Vec<&'static str> {
    inventory::iter::<TransientRegistration>.into_iter().map(|e| e.model).collect()
}

/// Whether `model` is transient (its rows are garbage-collected by age).
pub fn is_transient(model: &str) -> bool {
    inventory::iter::<TransientRegistration>.into_iter().any(|e| e.model == model)
}

/// A field whose changes are tracked in the chatter (Odoo's `tracking=True`). When a write changes
/// it on a mailed model, the write path records a `notification` message + a typed `mail.tracking`
/// row (old → new). Emitted by `#[field(tracked)]`. Compile-time, not runtime `track_visibility`.
pub struct TrackedFieldRegistration {
    pub model: &'static str,
    pub field: &'static str,
}
inventory::collect!(TrackedFieldRegistration);

/// The names of `model`'s tracked fields (changes recorded in the chatter on write).
pub fn tracked_fields(model: &str) -> Vec<&'static str> {
    inventory::iter::<TrackedFieldRegistration>
        .into_iter()
        .filter(|e| e.model == model)
        .map(|e| e.field)
        .collect()
}

/// A stored field declared read-only: shown in the UI contract as `readonly` and rejected by the write
/// path (like a computed field), even though it is not computed. Emitted by `#[field(readonly)]`. Used
/// for materialized values a service method maintains (e.g. on-hand) — visible, never hand-edited.
pub struct ReadonlyFieldRegistration {
    pub model: &'static str,
    pub field: &'static str,
}
inventory::collect!(ReadonlyFieldRegistration);

/// Whether `model`.`field` is declared `#[field(readonly)]`.
pub fn field_is_readonly(model: &str, field: &str) -> bool {
    inventory::iter::<ReadonlyFieldRegistration>
        .into_iter()
        .any(|e| e.model == model && e.field == field)
}

/// Delegation inheritance (Odoo's `_inherits`): `model` carries a required Many2one `via` to `parent`
/// and transparently exposes the parent's stored scalar fields (read via the FK, written through to the
/// parent). Emitted by `#[model(inherits = "parent", via = "fk")]`. The `via` FK is an ordinary field
/// the model declares — this only records the delegation, so no `ModelDescriptor`/`FieldDef` churn.
pub struct InheritsRegistration {
    pub model: &'static str,
    pub parent: &'static str,
    pub via: &'static str,
}
inventory::collect!(InheritsRegistration);

/// The `(parent, via)` a model delegates to, or None if it is not an `_inherits` child.
pub fn inherits_of(model: &str) -> Option<(&'static str, &'static str)> {
    inventory::iter::<InheritsRegistration>
        .into_iter()
        .find(|e| e.model == model)
        .map(|e| (e.parent, e.via))
}

/// A field a child transparently exposes from its `_inherits` parent: the parent's [`FieldDef`], the
/// parent's table, and the child's `via` FK column the read subquery / write UPDATE pivots on.
#[derive(Clone, Copy, Debug)]
pub struct DelegatedField {
    pub def: FieldDef,
    pub parent_table: &'static str,
    pub via: &'static str,
}

/// The parent fields that delegation exposes: stored, non-computed, non-related columns (scalars +
/// Many2one). Computed / related / One2many / Many2many parent fields are NOT delegated (v1). Uses only
/// `resolve_registered(parent)`, never `resolve_registered(model)`, so it can't recurse into the caller.
fn parent_delegatable(parent: &str) -> Result<Vec<FieldDef>, String> {
    let p = resolve_registered(parent)?;
    Ok(p.fields
        .iter()
        .copied()
        .filter(|f| f.has_column() && !f.is_computed() && related_path(parent, f.name).is_none())
        .collect())
}

/// Walks the `_inherits` chain from `model` (using only the registry, no resolution) and errors on a
/// cycle or excessive depth — so delegation resolution terminates instead of recursing forever.
fn check_inherits_acyclic(model: &str) -> Result<(), String> {
    let mut seen: Vec<&'static str> = Vec::new();
    let mut cur = model.to_string();
    for _ in 0..32 {
        match inherits_of(&cur) {
            None => return Ok(()),
            Some((parent, _)) => {
                if seen.contains(&parent) || parent == model {
                    return Err(format!("'_inherits' cycle: '{parent}' revisited from '{model}'"));
                }
                seen.push(parent);
                cur = parent.to_string();
            }
        }
    }
    Err(format!("'_inherits' chain from '{model}' is too deep (possible cycle)"))
}

/// Validates a model's `_inherits` declaration against its resolved own fields: the chain is acyclic
/// and the `via` field is a required Many2one to the parent. A child field whose name matches a
/// delegated parent field SHADOWS it (Odoo behaviour): the child owns that column and the name is not
/// delegated (`delegated_fields` excludes it). This is what lets `product.product` carry its own
/// `active` independently of the shared `product.template.active`. Called by `resolve_registered`.
fn validate_inherits(model: &str, own: &ResolvedModel) -> Result<(), String> {
    let Some((parent, via)) = inherits_of(model) else { return Ok(()) };
    check_inherits_acyclic(model)?;
    let vf = own.fields.iter().find(|f| f.name == via).ok_or_else(|| {
        format!("'_inherits' on '{model}': via field '{via}' is not declared on the model")
    })?;
    match vf.kind {
        FieldKind::Many2one { target } if target == parent => {}
        _ => {
            return Err(format!(
                "'_inherits' on '{model}': via field '{via}' must be a Many2one to '{parent}'"
            ))
        }
    }
    if !vf.required {
        return Err(format!("'_inherits' on '{model}': via field '{via}' must be required"));
    }
    // Confirm the parent is itself resolvable (surfaces a parent error here rather than at read time);
    // name overlaps are intentional shadows, resolved by `delegated_fields`, so they are not errors.
    parent_delegatable(parent)?;
    Ok(())
}

/// The fields `model` delegates to its `_inherits` parent (empty if it is not a child). Each is read
/// via the parent through the `via` FK; the child does NOT have a column for it. A name the child also
/// declares (an intentional shadow, e.g. `product.product.active` over `product.template.active`) is
/// excluded — the child's own column wins and the parent's is not delegated.
pub fn delegated_fields(model: &str) -> Result<Vec<DelegatedField>, String> {
    let Some((parent, via)) = inherits_of(model) else { return Ok(Vec::new()) };
    let child = resolve_registered(model)?;
    let parent_table = resolve_registered(parent)?.table;
    Ok(parent_delegatable(parent)?
        .into_iter()
        .filter(|d| !child.fields.iter().any(|f| f.name == d.name))
        .map(|def| DelegatedField { def, parent_table, via })
        .collect())
}

/// A related field (Odoo `related=`): a NON-stored, read-only mirror of a value reached by following
/// a relational `path` (e.g. `order_id.currency_id`). Registered out-of-band (emitted by
/// `#[field(related = "...")]`) so it adds no column to the metamodel — the value is resolved at read
/// time by a correlated subquery over the path. The field's declared kind must match the path target.
pub struct RelatedRegistration {
    pub model: &'static str,
    pub field: &'static str,
    pub path: &'static str,
}
inventory::collect!(RelatedRegistration);

/// The relational path a related field mirrors, or None if `field` is not a related field.
pub fn related_path(model: &str, field: &str) -> Option<&'static str> {
    inventory::iter::<RelatedRegistration>
        .into_iter()
        .find(|r| r.model == model && r.field == field)
        .map(|r| r.path)
}

/// Distinct group names referenced by any registered ACL or record rule — the catalog's known
/// groups (the source for seeding the read-only `res.groups` list). Sorted, deterministic.
pub fn registered_group_names() -> Vec<String> {
    let mut g: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        if !g.iter().any(|x| x == name) {
            g.push(name.to_string());
        }
    };
    for a in registered_acls() {
        push(a.group);
    }
    for r in registered_rules() {
        for gr in r.groups {
            push(gr);
        }
    }
    g.sort();
    g
}

/// All ACLs registered across linked modules (the union a server enforces).
pub fn registered_acls() -> Vec<Acl> {
    inventory::iter::<AclRegistration>
        .into_iter()
        .flat_map(|r| (r.acls)().iter().copied())
        .collect()
}

/// All record rules registered across linked modules.
pub fn registered_rules() -> Vec<RecordRule> {
    inventory::iter::<RecordRuleRegistration>
        .into_iter()
        .flat_map(|r| (r.rules)().iter().cloned())
        .collect()
}

/// One model to migrate, with the owning module's name + version (for the migration ledger).
pub struct MigrationTarget {
    pub module: &'static str,
    pub version: String,
    pub model: ResolvedModel,
}

/// The full migration plan, TOPOLOGICALLY SORTED by Many2one FK dependencies (a model's FK targets
/// — e.g. res.currency/res.partner for res.company — are created before the referencing table).
/// Self-references are ignored (a table may reference itself in its own CREATE); a genuine FK cycle
/// between two tables is an error. Ties break by (module dependency order, model name) for
/// determinism. Each target carries its owning module's name + version for the migration ledger.
pub fn migration_plan() -> Result<Vec<MigrationTarget>, String> {
    let modules = resolve_modules().map_err(|e| format!("{e:?}"))?; // validates the module graph
    let regs: Vec<&'static ModuleRegistration> = inventory::iter::<ModuleRegistration>.into_iter().collect();

    // Owning (module name, version) for a model, via its registration's module_path prefix.
    let owner = |model_name: &str| -> (&'static str, String) {
        let model_path = inventory::iter::<ModelRegistration>
            .into_iter()
            .find(|r| r.name == model_name)
            .map(|r| r.module)
            .unwrap_or("");
        for r in &regs {
            if model_path == r.crate_path || model_path.starts_with(&format!("{}::", r.crate_path)) {
                let m = (r.manifest)();
                return (m.name, m.version.to_string());
            }
        }
        ("", FRAMEWORK_VERSION.to_string())
    };

    // External-table models (Odoo `_auto = False`) are served/resolved but never migrated.
    let external = external_tables();
    let names: Vec<&'static str> =
        registered_model_names().into_iter().filter(|n| !external.contains(n)).collect();
    let n = names.len();
    let index: std::collections::HashMap<&str, usize> =
        names.iter().enumerate().map(|(i, &nm)| (nm, i)).collect();
    let models: Vec<ResolvedModel> =
        names.iter().map(|nm| resolve_registered(nm)).collect::<Result<_, _>>()?;

    // FK edges: model i needs each registered Many2one target created first.
    let mut indeg = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, m) in models.iter().enumerate() {
        for f in &m.fields {
            // An Image is a real FK to ir.attachment, so it constrains ordering like a Many2one.
            let target = match f.kind {
                crate::FieldKind::Many2one { target } => Some(target),
                crate::FieldKind::Image => Some("ir.attachment"),
                _ => None,
            };
            if let Some(target) = target {
                if let Some(&j) = index.get(target) {
                    if j != i {
                        indeg[i] += 1;
                        dependents[j].push(i);
                    }
                }
            }
        }
    }

    let module_rank: std::collections::HashMap<&str, usize> =
        modules.iter().enumerate().map(|(i, m)| (m.name, i)).collect();
    let rank = |i: usize| -> (usize, &'static str) {
        (*module_rank.get(owner(names[i]).0).unwrap_or(&usize::MAX), names[i])
    };

    // Kahn's algorithm; process ready nodes smallest-rank first.
    let mut ready: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut order: Vec<usize> = Vec::with_capacity(n);
    while !ready.is_empty() {
        ready.sort_by(|&a, &b| rank(b).cmp(&rank(a))); // descending → pop() yields the smallest
        let i = ready.pop().unwrap();
        order.push(i);
        for &k in &dependents[i] {
            indeg[k] -= 1;
            if indeg[k] == 0 {
                ready.push(k);
            }
        }
    }
    if order.len() != n {
        return Err("cyclic Many2one FK dependency among models (cannot order migration)".to_string());
    }

    Ok(order
        .into_iter()
        .map(|i| {
            let (module, version) = owner(names[i]);
            MigrationTarget { module, version, model: models[i].clone() }
        })
        .collect())
}

/// Resolves every module registered in the catalog: framework compatibility, dependency
/// version ranges, no duplicates, no cycles — returning them in dependency order.
/// The validation `resolve_module_set` does is exactly what Odoo's `depends` cannot express.
pub fn resolve_modules() -> Result<Vec<ModuleManifest>, ResolutionError> {
    let modules: Vec<ModuleManifest> = inventory::iter::<ModuleRegistration>
        .into_iter()
        .map(|r| (r.manifest)())
        .collect();
    resolve_module_set(&modules, FRAMEWORK_VERSION)
}

/// The name of the module that owns `model`, via the model's registration crate_path → manifest
/// mapping (the same hook `migration_plan` uses). Used to gate migration/serving by installed module.
pub fn module_of(model: &str) -> Option<&'static str> {
    let model_path = inventory::iter::<ModelRegistration>
        .into_iter()
        .find(|r| r.name == model)
        .map(|r| r.module)?;
    for r in inventory::iter::<ModuleRegistration> {
        if model_path == r.crate_path || model_path.starts_with(&format!("{}::", r.crate_path)) {
            return Some((r.manifest)().name);
        }
    }
    None
}

/// A module plus its full transitive dependency closure, in dependency order (dependencies first) —
/// the set to install when installing `name`. Errors if `name` (or a dependency) is not a linked
/// module, or the module graph is invalid.
pub fn module_closure(name: &str) -> Result<Vec<&'static str>, String> {
    let mods = resolve_modules().map_err(|e| format!("{e:?}"))?; // validated + topo-sorted
    let mut want: Vec<&'static str> = Vec::new();
    let mut stack = vec![name.to_string()];
    while let Some(n) = stack.pop() {
        let m = mods
            .iter()
            .find(|m| m.name == n)
            .ok_or_else(|| format!("unknown module '{n}'"))?;
        if !want.contains(&m.name) {
            want.push(m.name);
            for d in m.depends {
                stack.push(d.name.to_string());
            }
        }
    }
    // Return in the validated dependency order (dependencies before dependents).
    Ok(mods.iter().filter(|m| want.contains(&m.name)).map(|m| m.name).collect())
}

/// Names of all models registered in the catalog (sorted, deterministic).
pub fn registered_model_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> =
        inventory::iter::<ModelRegistration>.into_iter().map(|r| r.name).collect();
    names.sort_unstable();
    names
}

/// Resolves every registered model (base + extensions). The bridge from the catalog to a server
/// or any consumer that needs the full set of models.
pub fn resolve_all_registered() -> Result<Vec<ResolvedModel>, String> {
    registered_model_names().iter().map(|n| resolve_registered(n)).collect()
}

/// Resolves a model from the CATALOG: registered base + all auto-registered extensions,
/// merged (with conflict checks) and validated. No wiring: modules auto-register.
pub fn resolve_registered(model: &str) -> Result<ResolvedModel, String> {
    let base = inventory::iter::<ModelRegistration>
        .into_iter()
        .find(|r| r.name == model)
        .ok_or_else(|| format!("model '{model}' not registered in the catalog"))?;

    let mut exts: Vec<&'static FieldExtension> = inventory::iter::<FieldExtension>
        .into_iter()
        .filter(|e| e.target == model)
        .collect();
    exts.sort_by_key(|e| e.module); // deterministic ordering across modules

    let ext_fields: Vec<&'static [FieldDef]> = exts.iter().map(|e| (e.fields)()).collect();
    let m = resolve((base.descriptor)(), &ext_fields)?;
    // Delegation first: confirms the chain is acyclic + the parent resolves (so `parent_delegatable`
    // below is safe and can't recurse into this model).
    validate_inherits(model, &m)?;
    // An on-read compute may depend on an inherited (delegated) field, which has no column on this
    // model — compute the delegated names from the PARENT (never re-resolving this model) and allow them.
    let delegated_names: Vec<&str> = match inherits_of(model) {
        Some((parent, _via)) => parent_delegatable(parent)?
            .into_iter()
            .filter(|d| !m.fields.iter().any(|f| f.name == d.name))
            .map(|d| d.name)
            .collect(),
        None => Vec::new(),
    };
    validate_depends_with_extra(&m, &delegated_names)?;
    Ok(m)
}

/// Fails the boot on a DUPLICATE registration in any registry whose lookup is first-match.
///
/// `action_for`, `view_for` and `report_for` are all `.find()` over the inventory: two modules
/// registering the same key does not error, one of them silently wins, and WHICH one depends on
/// crate link order — so the same source tree can behave differently after an unrelated dependency
/// change. Compile-time-authored registrations are code, and a collision in them is a bug that must
/// stop the process, exactly like `validate_routes` already does for module routes.
///
/// Deliberately NOT an override mechanism: nothing needs to override a registration yet. When F3's
/// localization modules do, the semantics get designed then — a silent first-match is not a design.
pub fn validate_core_registrations() -> Result<(), String> {
    let mut actions = std::collections::BTreeSet::new();
    for a in inventory::iter::<crate::ActionRegistration> {
        if !actions.insert((a.model, a.name)) {
            return Err(format!("duplicate action registration: {}.{}", a.model, a.name));
        }
    }
    let mut reports = std::collections::BTreeSet::new();
    for r in inventory::iter::<crate::ReportRegistration> {
        if !reports.insert((r.model, r.name)) {
            return Err(format!("duplicate report registration: {} on {}", r.name, r.model));
        }
    }
    // A form view is keyed by model alone — a second one for the same model is not an extension,
    // it is a shadow that `view_for` will never return.
    let mut views = std::collections::BTreeSet::new();
    for v in inventory::iter::<crate::FormView> {
        if !views.insert(v.model) {
            return Err(format!("duplicate form view registration for model {}", v.model));
        }
    }
    Ok(())
}
