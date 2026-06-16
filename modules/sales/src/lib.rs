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

/// La "base" di sale.order — ora dichiarata con `#[model]` (fase 2).
/// La macro genera `ModelDescriptor` + `impl Model`; i "tipi" dei campi sono il DSL.
#[model(name = "sale.order", table = "sale_order")]
pub struct SaleOrder {
    #[field(label = "Order Reference", required)]
    name: Text,

    #[field(label = "Customer", required, target = "res.partner")]
    partner_id: Many2one,

    #[field(label = "Order Lines", target = "sale.order.line", inverse = "order_id")]
    line_ids: One2many,

    #[field(label = "Status", required, selection = "draft:Draft,sale:Confirmed,done:Done")]
    state: Selection,

    #[field(label = "Currency", required, target = "res.currency")]
    currency_id: Many2one,

    #[field(label = "Total", compute = "compute_amount", depends = "line_ids.price_subtotal", currency = "currency_id", store)]
    amount_total: Decimal,
}

/// Estensione `sale_margin`: aggiunge `margin` SENZA toccare la base.
pub static SALE_MARGIN_FIELDS: &[FieldDef] = &[FieldDef {
    name: "margin", label: "Margin",
    kind: FieldKind::Decimal { currency_field: Some("currency_id") },
    required: false, stored: true, compute: Some("compute_margin"),
    depends: &["amount_total"],
}];

/// Risolve il modello completo del modulo (base + estensioni), validato.
pub fn resolved_sale_order() -> ResolvedModel {
    let m = resolve(SaleOrder::descriptor(), &[SALE_MARGIN_FIELDS]).expect("risoluzione sale.order");
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
        assert_eq!(m.fields.len(), SaleOrder::descriptor().fields.len() + 1);
        assert!(m.fields.iter().any(|f| f.name == "margin"));
    }

    #[test]
    fn macro_genera_descrittore_corretto() {
        // La macro deve produrre lo STESSO descrittore della versione scritta a mano.
        let d = SaleOrder::descriptor();
        assert_eq!(d.name, "sale.order");
        assert_eq!(d.fields.len(), 6);
        let total = d.fields.iter().find(|f| f.name == "amount_total").unwrap();
        assert!(total.stored, "computed con `store` deve essere stored");
        assert_eq!(total.compute, Some("compute_amount"));
        assert_eq!(total.depends, &["line_ids.price_subtotal"]);
        let lines = d.fields.iter().find(|f| f.name == "line_ids").unwrap();
        assert!(!lines.has_column(), "one2many non ha colonna");
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
