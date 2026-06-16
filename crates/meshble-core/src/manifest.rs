//! Module versioning — the equivalent of Odoo's `__manifest__.py`, but with
//! VERIFIED version ranges (Odoo declares `depends` without any version constraint).
//!
//! Key choices (see `docs/VERSIONING.md`):
//! - The framework uses SemVer (Cargo-native).
//! - Every module has its own version + a compatibility range with the framework
//!   (NO Odoo-style lockstep coupling like "19.0.1.0.0").
//! - Dependencies between modules have SemVer ranges → verifiable resolution.

use semver::{Version, VersionReq};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Dependency on another module, with a SemVer version constraint (e.g. "^1.2").
#[derive(Clone, Copy, Debug)]
pub struct ModuleDep {
    pub name: &'static str,
    pub req: &'static str,
}

/// Manifest of a module. Declarative data, validated at build/install time.
#[derive(Clone, Copy, Debug)]
pub struct ModuleManifest {
    pub name: &'static str,
    /// SemVer of the module, e.g. "1.0.0".
    pub version: &'static str,
    /// Compatibility range with the framework, e.g. ">=0.1, <0.2".
    pub framework: &'static str,
    /// Dependencies on other modules, with version ranges.
    pub depends: &'static [ModuleDep],
    pub summary: &'static str,
}

#[derive(Debug, PartialEq)]
pub enum ResolutionError {
    BadVersion(String),
    BadRequirement(String),
    /// A module is not compatible with the framework version.
    Incompatible { module: String, needs: String, found: String },
    /// A module depends on another module that is not present in the catalog.
    MissingDependency { module: String, dep: String },
    /// A dependency is present but its version does not satisfy the requested range.
    DependencyConflict { module: String, dep: String, needs: String, found: String },
    /// Two modules declare the same name.
    DuplicateModule(String),
    /// A module lists itself as a dependency.
    SelfDependency { module: String },
    /// The dependency graph contains a cycle; the vector lists the modules ON the cycle.
    DependencyCycle(Vec<String>),
}

/// Compatibility policy: a pre-release build (e.g. `0.1.5-rc.1`) is treated as its release
/// line (`0.1.5`) when matching version ranges. Without this, Cargo/SemVer rules reject every
/// in-range pre-release (a range only matches a pre-release when a comparator shares the exact
/// `major.minor.patch` and itself carries a pre-release), which would make every install fail
/// during the framework's own RC/dev builds. See `docs/VERSIONING.md`.
fn release_of(v: &Version) -> Version {
    Version {
        pre: semver::Prerelease::EMPTY,
        build: semver::BuildMetadata::EMPTY,
        ..v.clone()
    }
}

/// Verifies that `manifest` is compatible with the provided framework version.
/// This is the check Odoo does NOT do: there a "19.0" module runs (or breaks) on any 19.x.
pub fn check_compat(
    manifest: &ModuleManifest,
    framework_version: &str,
) -> Result<(), ResolutionError> {
    let fw = Version::parse(framework_version)
        .map_err(|e| ResolutionError::BadVersion(format!("framework: {e}")))?;
    let _ = Version::parse(manifest.version)
        .map_err(|e| ResolutionError::BadVersion(format!("module {}: {e}", manifest.name)))?;
    let req = VersionReq::parse(manifest.framework)
        .map_err(|e| ResolutionError::BadRequirement(format!("module {}: {e}", manifest.name)))?;
    if !req.matches(&release_of(&fw)) {
        return Err(ResolutionError::Incompatible {
            module: manifest.name.to_string(),
            needs: manifest.framework.to_string(),
            found: framework_version.to_string(),
        });
    }
    Ok(())
}

/// Resolves a set of modules: checks framework compatibility, that every dependency exists
/// with a satisfying SemVer version, that there are no duplicate names and no cycles.
/// Returns the modules in dependency (topological) order — what Odoo computes implicitly.
///
/// This is a pure function over an explicit slice so it is fully testable without the global
/// catalog; `resolve_modules` (in `registry`) is the thin wrapper that feeds it the catalog.
pub fn resolve_module_set(
    modules: &[ModuleManifest],
    framework_version: &str,
) -> Result<Vec<ModuleManifest>, ResolutionError> {
    // Index by name, rejecting duplicates.
    let mut by_name: BTreeMap<&str, ModuleManifest> = BTreeMap::new();
    for m in modules {
        if by_name.insert(m.name, *m).is_some() {
            return Err(ResolutionError::DuplicateModule(m.name.to_string()));
        }
    }

    // Validate framework compatibility and dependency version ranges.
    for m in modules {
        check_compat(m, framework_version)?;
        for dep in m.depends {
            if dep.name == m.name {
                return Err(ResolutionError::SelfDependency { module: m.name.to_string() });
            }
            let found = by_name.get(dep.name).ok_or_else(|| ResolutionError::MissingDependency {
                module: m.name.to_string(),
                dep: dep.name.to_string(),
            })?;
            let req = VersionReq::parse(dep.req).map_err(|e| {
                ResolutionError::BadRequirement(format!("{} -> {}: {e}", m.name, dep.name))
            })?;
            let ver = Version::parse(found.version).map_err(|e| {
                ResolutionError::BadVersion(format!("module {}: {e}", found.name))
            })?;
            if !req.matches(&release_of(&ver)) {
                return Err(ResolutionError::DependencyConflict {
                    module: m.name.to_string(),
                    dep: dep.name.to_string(),
                    needs: dep.req.to_string(),
                    found: found.version.to_string(),
                });
            }
        }
    }

    // Topological sort (Kahn). Dependencies are deduplicated per module so that a module
    // listing the same dependency twice does not inflate its indegree into a false cycle.
    let deps_of: BTreeMap<&str, BTreeSet<&str>> = modules
        .iter()
        .map(|m| (m.name, m.depends.iter().map(|d| d.name).collect()))
        .collect();
    let mut indegree: BTreeMap<&str, usize> =
        deps_of.iter().map(|(name, deps)| (*name, deps.len())).collect();
    let mut queue: VecDeque<&str> = indegree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(k, _)| *k)
        .collect();

    let mut order: Vec<ModuleManifest> = Vec::with_capacity(modules.len());
    while let Some(name) = queue.pop_front() {
        order.push(by_name[name]);
        for (dependent, deps) in &deps_of {
            if deps.contains(name) {
                let e = indegree.get_mut(dependent).unwrap();
                *e -= 1;
                if *e == 0 {
                    queue.push_back(dependent);
                }
            }
        }
    }

    if order.len() != modules.len() {
        // The leftover nodes are everything on OR downstream of a cycle. Strip the downstream
        // tail: repeatedly drop any residual node that nothing else in the residual set depends
        // on (a node nobody depends on cannot be on a cycle), leaving only true cycle members.
        let mut residual: BTreeSet<&str> =
            indegree.iter().filter(|(_, &d)| d > 0).map(|(k, _)| *k).collect();
        loop {
            let depended: BTreeSet<&str> = residual
                .iter()
                .flat_map(|n| deps_of[n].iter().copied())
                .filter(|d| residual.contains(d))
                .collect();
            let leaves: Vec<&str> =
                residual.iter().copied().filter(|n| !depended.contains(n)).collect();
            if leaves.is_empty() {
                break;
            }
            for n in leaves {
                residual.remove(n);
            }
        }
        return Err(ResolutionError::DependencyCycle(
            residual.iter().map(|s| s.to_string()).collect(),
        ));
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    static M: ModuleManifest = ModuleManifest {
        name: "sales",
        version: "1.0.0",
        framework: ">=0.1, <0.2",
        depends: &[ModuleDep { name: "base", req: "^0.1" }],
        summary: "Sales",
    };

    #[test]
    fn compatible_framework_passes() {
        assert!(check_compat(&M, "0.1.5").is_ok());
    }

    #[test]
    fn incompatible_framework_rejected() {
        // 0.2.0 falls outside the range ">=0.1, <0.2" → error, whereas Odoo would let it pass.
        match check_compat(&M, "0.2.0") {
            Err(ResolutionError::Incompatible { .. }) => {}
            other => panic!("expected Incompatible, got {other:?}"),
        }
    }

    static BASE: ModuleManifest = ModuleManifest {
        name: "base", version: "1.0.0", framework: ">=0.1, <0.2", depends: &[], summary: "",
    };
    static SALES: ModuleManifest = ModuleManifest {
        name: "sales", version: "1.0.0", framework: ">=0.1, <0.2",
        depends: &[ModuleDep { name: "base", req: "^1.0" }], summary: "",
    };

    #[test]
    fn resolve_orders_topologically() {
        // Input order is reversed; the resolver must return [base, sales].
        let order = resolve_module_set(&[SALES, BASE], "0.1.0").unwrap();
        assert_eq!(order.iter().map(|m| m.name).collect::<Vec<_>>(), ["base", "sales"]);
    }

    #[test]
    fn missing_dependency_errors() {
        match resolve_module_set(&[SALES], "0.1.0") {
            Err(ResolutionError::MissingDependency { dep, .. }) => assert_eq!(dep, "base"),
            other => panic!("expected MissingDependency, got {other:?}"),
        }
    }

    #[test]
    fn version_conflict_errors() {
        static SALES_V2: ModuleManifest = ModuleManifest {
            name: "sales", version: "1.0.0", framework: ">=0.1, <0.2",
            depends: &[ModuleDep { name: "base", req: "^2.0" }], summary: "",
        };
        match resolve_module_set(&[SALES_V2, BASE], "0.1.0") {
            Err(ResolutionError::DependencyConflict { needs, found, .. }) => {
                assert_eq!(needs, "^2.0");
                assert_eq!(found, "1.0.0");
            }
            other => panic!("expected DependencyConflict, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_module_errors() {
        assert!(matches!(
            resolve_module_set(&[BASE, BASE], "0.1.0"),
            Err(ResolutionError::DuplicateModule(_))
        ));
    }

    #[test]
    fn cycle_errors() {
        static A: ModuleManifest = ModuleManifest {
            name: "a", version: "1.0.0", framework: ">=0.1, <0.2",
            depends: &[ModuleDep { name: "b", req: "^1.0" }], summary: "",
        };
        static B: ModuleManifest = ModuleManifest {
            name: "b", version: "1.0.0", framework: ">=0.1, <0.2",
            depends: &[ModuleDep { name: "a", req: "^1.0" }], summary: "",
        };
        assert!(matches!(
            resolve_module_set(&[A, B], "0.1.0"),
            Err(ResolutionError::DependencyCycle(_))
        ));
    }

    #[test]
    fn duplicate_dependency_entries_are_not_a_cycle() {
        // A module listing the same dependency twice must still resolve cleanly.
        static DUP_DEP: ModuleManifest = ModuleManifest {
            name: "s", version: "1.0.0", framework: ">=0.1, <0.2",
            depends: &[
                ModuleDep { name: "base", req: "^1.0" },
                ModuleDep { name: "base", req: "^1.0" },
            ],
            summary: "",
        };
        let order = resolve_module_set(&[DUP_DEP, BASE], "0.1.0").unwrap();
        assert_eq!(order.iter().map(|m| m.name).collect::<Vec<_>>(), ["base", "s"]);
    }

    #[test]
    fn diamond_dependencies_resolve() {
        // a -> b, a -> c, b -> d, c -> d. Must order d before b/c before a, no cycle.
        static D: ModuleManifest = ModuleManifest {
            name: "d", version: "1.0.0", framework: ">=0.1, <0.2", depends: &[], summary: "",
        };
        static B2: ModuleManifest = ModuleManifest {
            name: "b", version: "1.0.0", framework: ">=0.1, <0.2",
            depends: &[ModuleDep { name: "d", req: "^1.0" }], summary: "",
        };
        static C2: ModuleManifest = ModuleManifest {
            name: "c", version: "1.0.0", framework: ">=0.1, <0.2",
            depends: &[ModuleDep { name: "d", req: "^1.0" }], summary: "",
        };
        static A2: ModuleManifest = ModuleManifest {
            name: "a", version: "1.0.0", framework: ">=0.1, <0.2",
            depends: &[ModuleDep { name: "b", req: "^1.0" }, ModuleDep { name: "c", req: "^1.0" }],
            summary: "",
        };
        let order = resolve_module_set(&[A2, B2, C2, D], "0.1.0").unwrap();
        let pos = |n: &str| order.iter().position(|m| m.name == n).unwrap();
        assert!(pos("d") < pos("b") && pos("d") < pos("c"));
        assert!(pos("b") < pos("a") && pos("c") < pos("a"));
    }

    #[test]
    fn prerelease_framework_is_accepted_within_line() {
        // A dev/RC build of the 0.1.x line must be accepted...
        assert!(check_compat(&BASE, "0.1.5-rc.1").is_ok());
        // ...while a pre-release of the NEXT line stays out of range.
        assert!(matches!(
            check_compat(&BASE, "0.2.0-rc.1"),
            Err(ResolutionError::Incompatible { .. })
        ));
    }

    #[test]
    fn prerelease_dependency_satisfies_caret() {
        static BASE_RC: ModuleManifest = ModuleManifest {
            name: "base", version: "1.0.0-rc.1", framework: ">=0.1, <0.2", depends: &[], summary: "",
        };
        static DEP: ModuleManifest = ModuleManifest {
            name: "s", version: "1.0.0", framework: ">=0.1, <0.2",
            depends: &[ModuleDep { name: "base", req: "^1.0" }], summary: "",
        };
        // base 1.0.0-rc.1 is the release candidate of exactly the 1.0.0 that `^1.0` wants.
        assert!(resolve_module_set(&[DEP, BASE_RC], "0.1.0").is_ok());
    }

    #[test]
    fn self_dependency_is_a_dedicated_error() {
        static SELF: ModuleManifest = ModuleManifest {
            name: "x", version: "1.0.0", framework: ">=0.1, <0.2",
            depends: &[ModuleDep { name: "x", req: "^1.0" }], summary: "",
        };
        assert!(matches!(
            resolve_module_set(&[SELF], "0.1.0"),
            Err(ResolutionError::SelfDependency { .. })
        ));
    }

    #[test]
    fn cycle_reports_only_true_members() {
        // a<->b is the real cycle; c->a and d->c are downstream; e is unrelated.
        static A: ModuleManifest = ModuleManifest {
            name: "a", version: "1.0.0", framework: ">=0.1, <0.2",
            depends: &[ModuleDep { name: "b", req: "^1.0" }], summary: "",
        };
        static B: ModuleManifest = ModuleManifest {
            name: "b", version: "1.0.0", framework: ">=0.1, <0.2",
            depends: &[ModuleDep { name: "a", req: "^1.0" }], summary: "",
        };
        static C: ModuleManifest = ModuleManifest {
            name: "c", version: "1.0.0", framework: ">=0.1, <0.2",
            depends: &[ModuleDep { name: "a", req: "^1.0" }], summary: "",
        };
        static D: ModuleManifest = ModuleManifest {
            name: "d", version: "1.0.0", framework: ">=0.1, <0.2",
            depends: &[ModuleDep { name: "c", req: "^1.0" }], summary: "",
        };
        static E: ModuleManifest = ModuleManifest {
            name: "e", version: "1.0.0", framework: ">=0.1, <0.2", depends: &[], summary: "",
        };
        match resolve_module_set(&[A, B, C, D, E], "0.1.0") {
            // Only a and b are on the cycle; c and d (downstream) must NOT be reported.
            Err(ResolutionError::DependencyCycle(members)) => {
                assert_eq!(members, ["a".to_string(), "b".to_string()]);
            }
            other => panic!("expected DependencyCycle, got {other:?}"),
        }
    }
}
