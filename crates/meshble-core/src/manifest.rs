//! Versioning dei moduli — l'equivalente del `__manifest__.py` di Odoo, ma con
//! range di versione VERIFICATI (Odoo dichiara `depends` senza alcun vincolo di versione).
//!
//! Scelte chiave (vedi `docs/VERSIONING.md`):
//! - Il framework usa SemVer (Cargo-native).
//! - Ogni modulo ha versione propria + un range di compatibilità col framework
//!   (NO accoppiamento lockstep stile Odoo "19.0.1.0.0").
//! - Le dipendenze tra moduli hanno range SemVer → risoluzione verificabile.

use semver::{Version, VersionReq};

/// Dipendenza verso un altro modulo, con vincolo di versione SemVer (es. "^1.2").
#[derive(Clone, Copy, Debug)]
pub struct ModuleDep {
    pub name: &'static str,
    pub req: &'static str,
}

/// Manifest di un modulo. Dato dichiarativo, validato a build/install time.
#[derive(Clone, Copy, Debug)]
pub struct ModuleManifest {
    pub name: &'static str,
    /// SemVer del modulo, es. "1.0.0".
    pub version: &'static str,
    /// Range di compatibilità col framework, es. ">=0.1, <0.2".
    pub framework: &'static str,
    /// Dipendenze verso altri moduli, con range di versione.
    pub depends: &'static [ModuleDep],
    pub summary: &'static str,
}

#[derive(Debug, PartialEq)]
pub enum ResolutionError {
    BadVersion(String),
    BadRequirement(String),
    Incompatible { module: String, needs: String, found: String },
}

/// Verifica che `manifest` sia compatibile con la versione del framework fornita.
/// È il controllo che Odoo NON fa: lì un modulo "19.0" gira (o si rompe) su qualsiasi 19.x.
pub fn check_compat(
    manifest: &ModuleManifest,
    framework_version: &str,
) -> Result<(), ResolutionError> {
    let fw = Version::parse(framework_version)
        .map_err(|e| ResolutionError::BadVersion(format!("framework: {e}")))?;
    let _ = Version::parse(manifest.version)
        .map_err(|e| ResolutionError::BadVersion(format!("modulo {}: {e}", manifest.name)))?;
    let req = VersionReq::parse(manifest.framework)
        .map_err(|e| ResolutionError::BadRequirement(format!("modulo {}: {e}", manifest.name)))?;
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
        // 0.2.0 esce dal range ">=0.1, <0.2" → errore, mentre Odoo lo lascerebbe passare.
        match check_compat(&M, "0.2.0") {
            Err(ResolutionError::Incompatible { .. }) => {}
            other => panic!("atteso Incompatible, ottenuto {other:?}"),
        }
    }
}
