//! Descrittori di modello ispezionabili e risoluzione delle estensioni.

/// Tipo logico di un campo. Da qui si derivano tipo SQL, widget UI e tipo API.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FieldKind {
    Text,
    Integer,
    /// `currency_field`: campo monetario → widget "monetary" + valuta collegata.
    Decimal { currency_field: Option<&'static str> },
    Bool,
    Selection(&'static [(&'static str, &'static str)]),
    /// Relazione N→1: genera una colonna FK.
    Many2one { target: &'static str },
    /// Relazione 1→N: NON genera colonna (vive sull'inverso).
    One2many { target: &'static str, inverse: &'static str },
}

/// Definizione di un singolo campo.
#[derive(Clone, Copy, Debug)]
pub struct FieldDef {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    /// `false` per computed non memorizzati → nessuna colonna in tabella.
    pub stored: bool,
    /// Nome del metodo di compute, se calcolato.
    pub compute: Option<&'static str>,
    /// Dipendenze dichiarate; VERIFICATE da `validate_depends` (no N+1 ciechi).
    pub depends: &'static [&'static str],
}

impl FieldDef {
    /// Un campo ha una colonna in tabella sse è memorizzato e non è un one2many.
    pub fn has_column(&self) -> bool {
        self.stored && !matches!(self.kind, FieldKind::One2many { .. })
    }
    pub fn is_computed(&self) -> bool {
        self.compute.is_some()
    }
}

/// Descrittore di un modello come definito da UN modulo (la "base").
pub struct ModelDescriptor {
    pub name: &'static str,
    pub table: &'static str,
    pub fields: &'static [FieldDef],
}

/// Implementato da ogni modello. Nel walking skeleton è scritto a mano;
/// alla fase 2 lo genera la proc-macro `#[model]`.
pub trait Model {
    fn descriptor() -> &'static ModelDescriptor;
}

/// Descrittore RISOLTO: base + tutte le estensioni fuse e validate.
/// È il punto — assente in Odoo — dove la definizione finale è materializzabile.
#[derive(Debug)]
pub struct ResolvedModel {
    pub name: &'static str,
    pub table: &'static str,
    pub fields: Vec<FieldDef>,
}

/// Fonde la base con le estensioni dei moduli. I conflitti sono errori.
pub fn resolve(
    base: &ModelDescriptor,
    extensions: &[&'static [FieldDef]],
) -> Result<ResolvedModel, String> {
    let mut fields: Vec<FieldDef> = base.fields.to_vec();
    for ext in extensions {
        for f in *ext {
            if fields.iter().any(|x| x.name == f.name) {
                return Err(format!(
                    "conflitto: il campo '{}' è già definito sul modello '{}'",
                    f.name, base.name
                ));
            }
            fields.push(*f);
        }
    }
    Ok(ResolvedModel { name: base.name, table: base.table, fields })
}

/// Verifica che ogni `depends` punti a un campo esistente (primo segmento del path).
/// Antidoto agli N+1 silenziosi di Odoo: dipendenza rotta = errore, non bug a runtime.
pub fn validate_depends(m: &ResolvedModel) -> Result<(), String> {
    for f in &m.fields {
        for dep in f.depends {
            let first = dep.split('.').next().unwrap_or(dep);
            if !m.fields.iter().any(|x| x.name == first) {
                return Err(format!(
                    "il campo '{}' dipende da un campo inesistente '{}' (in \"{}\")",
                    f.name, first, dep
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static BASE: ModelDescriptor = ModelDescriptor {
        name: "demo.model",
        table: "demo_model",
        fields: &[FieldDef {
            name: "name", label: "Name", kind: FieldKind::Text,
            required: true, stored: true, compute: None, depends: &[],
        }],
    };

    #[test]
    fn resolve_detects_conflict() {
        static DUP: &[FieldDef] = &[FieldDef {
            name: "name", label: "X", kind: FieldKind::Text,
            required: false, stored: true, compute: None, depends: &[],
        }];
        assert!(resolve(&BASE, &[DUP]).is_err());
    }

    #[test]
    fn depends_on_unknown_field_errors() {
        static BAD: &[FieldDef] = &[FieldDef {
            name: "x", label: "X", kind: FieldKind::Integer,
            required: false, stored: true, compute: Some("c"), depends: &["nope"],
        }];
        let m = resolve(&BASE, &[BAD]).unwrap();
        assert!(validate_depends(&m).is_err());
    }
}
