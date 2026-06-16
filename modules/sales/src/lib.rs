//! Modulo applicativo `sales`: `sale.order` + estensione `sale_margin`.
//! Definito a mano nel walking skeleton; alla fase 2 sarà `#[model]` / `#[extend]`.

use meshble::prelude::*;

/// Manifest del modulo: versione propria + range di compatibilità col framework.
/// Equivalente del `__manifest__.py` di Odoo, ma con versioni verificabili.
pub static MANIFEST: ModuleManifest = ModuleManifest {
    name: "sales",
    version: "1.0.0",
    framework: ">=0.1, <0.2",
    depends: &[ModuleDep { name: "base", req: "^0.1" }],
    summary: "Gestione ordini di vendita",
};

/// La "base" di sale.order.
pub static SALE_ORDER: ModelDescriptor = ModelDescriptor {
    name: "sale.order",
    table: "sale_order",
    fields: &[
        FieldDef {
            name: "name", label: "Order Reference", kind: FieldKind::Text,
            required: true, stored: true, compute: None, depends: &[],
        },
        FieldDef {
            name: "partner_id", label: "Customer",
            kind: FieldKind::Many2one { target: "res.partner" },
            required: true, stored: true, compute: None, depends: &[],
        },
        FieldDef {
            name: "line_ids", label: "Order Lines",
            kind: FieldKind::One2many { target: "sale.order.line", inverse: "order_id" },
            required: false, stored: false, compute: None, depends: &[],
        },
        FieldDef {
            name: "state", label: "Status",
            kind: FieldKind::Selection(&[("draft", "Draft"), ("sale", "Confirmed"), ("done", "Done")]),
            required: true, stored: true, compute: None, depends: &[],
        },
        FieldDef {
            name: "currency_id", label: "Currency",
            kind: FieldKind::Many2one { target: "res.currency" },
            required: true, stored: true, compute: None, depends: &[],
        },
        FieldDef {
            name: "amount_total", label: "Total",
            kind: FieldKind::Decimal { currency_field: Some("currency_id") },
            required: false, stored: true, compute: Some("compute_amount"),
            depends: &["line_ids.price_subtotal"],
        },
    ],
};

/// Estensione `sale_margin`: aggiunge `margin` SENZA toccare la base.
pub static SALE_MARGIN_FIELDS: &[FieldDef] = &[FieldDef {
    name: "margin", label: "Margin",
    kind: FieldKind::Decimal { currency_field: Some("currency_id") },
    required: false, stored: true, compute: Some("compute_margin"),
    depends: &["amount_total"],
}];

/// Risolve il modello completo del modulo (base + estensioni), validato.
pub fn resolved_sale_order() -> ResolvedModel {
    let m = resolve(&SALE_ORDER, &[SALE_MARGIN_FIELDS]).expect("risoluzione sale.order");
    validate_depends(&m).expect("depends sale.order");
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_compatibile_col_framework() {
        // Il framework dichiara FRAMEWORK_VERSION (0.1.0); il modulo accetta ">=0.1, <0.2".
        assert!(check_compat(&MANIFEST, FRAMEWORK_VERSION).is_ok());
    }

    #[test]
    fn estensione_margin_fusa() {
        let m = resolved_sale_order();
        assert_eq!(m.fields.len(), SALE_ORDER.fields.len() + 1);
        assert!(m.fields.iter().any(|f| f.name == "margin"));
    }

    #[test]
    fn ddl_e_ui_generati() {
        let m = resolved_sale_order();
        let ddl = to_ddl(&m);
        assert!(!ddl.contains("line_ids"), "one2many non ha colonna");
        assert!(ddl.contains("partner_id bigint REFERENCES res_partner(id) NOT NULL"));
        assert!(to_ui_contract(&m).contains("\"widget\": \"monetary\""));
    }
}
