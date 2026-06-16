//! Module versioning — the equivalent of Odoo's `__manifest__.py`, but with
//! VERIFIED version ranges (Odoo declares `depends` without any version constraint).
//!
//! Key choices (see `docs/VERSIONING.md`):
//! - The framework uses SemVer (Cargo-native).
//! - Every module has its own version + a compatibility range with the framework
//!   (NO Odoo-style lockstep coupling like "19.0.1.0.0").
//! - Dependencies between modules have SemVer ranges → verifiable resolution.

use semver::{Version, VersionReq};

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
    Incompatible { module: String, needs: String, found: String },
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
    if !req.matches(&fw) {
        return Err(ResolutionError::Incompatible {
            module: manifest.name.to_string(),
            needs: manifest.framework.to_string(),
            found: framework_version.to_string(),
        });
    }
    Ok(())
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
}
