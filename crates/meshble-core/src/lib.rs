//! Meshble core: il metamodello ispezionabile + il modello di versioning dei moduli.
//!
//! Differenza con Odoo (vedi `docs/ANALISI_ODOO19.md`): la definizione di un modello NON
//! è una classe sintetizzata a runtime con `type()`, ma un dato statico ispezionabile;
//! l'estensione è una FUSIONE VERIFICATA, non un monkey-patch globale; le dipendenze tra
//! moduli hanno RANGE DI VERSIONE verificati (Odoo non li ha — vedi `docs/VERSIONING.md`).

mod metamodel;
mod manifest;

pub use metamodel::{resolve, validate_depends, FieldDef, FieldKind, Model, ModelDescriptor, ResolvedModel};
pub use manifest::{check_compat, ModuleDep, ModuleManifest, ResolutionError};

/// Versione SemVer del framework (= versione del workspace). I moduli la confrontano
/// col proprio range di compatibilità via [`check_compat`].
pub const FRAMEWORK_VERSION: &str = env!("CARGO_PKG_VERSION");
