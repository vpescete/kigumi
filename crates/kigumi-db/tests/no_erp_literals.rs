//! Framework-purity guard: kigumi-db is the framework crate and must name NO ERP business model. If this
//! fails, an ERP concern leaked back into the core — relocate it to the owning module
//! (modules/{sales,account,stock}) behind the register_service! / register_write_trigger! seam, exactly as
//! the invoicing, tax, stock and variant engines were. The mandate is "framework first, ERP optional": the
//! core must compile and serve as a bare metamodel engine with no ERP module linked.
//!
//! It scans the crate's own src/ for a QUOTED, dotted model literal in the ERP chain — the
//! `resolve_registered("…")` key form. Prose that merely mentions a model name (unquoted, in a comment) is
//! fine; only a quoted literal (which is what couples code to a specific business model) trips the guard.

use std::fs;
use std::path::Path;

/// The ERP model namespaces the framework must never name. res.* / mail.* are deliberately NOT here: the
/// framework handles multi-company and the mail transport through generic mechanisms, not by naming those
/// models — and the base/mail table seams are a documented framework concern.
const ERP_NAMESPACES: &[&str] = &["sale.", "purchase.", "account.", "stock.", "product.", "uom."];

/// True if `line` contains a QUOTED ERP model literal (a resolve_registered key), not mere prose.
fn line_names_erp(line: &str) -> bool {
    ERP_NAMESPACES.iter().any(|ns| line.contains(&format!("\"{ns}")))
}

fn scan(dir: &Path, hits: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            scan(&path, hits);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        for (i, line) in text.lines().enumerate() {
            // A quoted, dotted literal like "sale.order": the opening quote distinguishes a
            // resolve_registered key from prose that merely names the model.
            if line_names_erp(line) {
                hits.push(format!("  {}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    }
}

#[test]
fn core_names_no_erp_model() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    scan(&src, &mut hits);
    assert!(
        hits.is_empty(),
        "ERP business-model literals leaked into kigumi-db/src — relocate them to the owning module \
         (modules/{{sales,account,stock}}) behind register_service! / register_write_trigger!:\n{}",
        hits.join("\n")
    );
}

#[test]
fn detector_catches_a_leak_and_ignores_prose() {
    // A quoted ERP literal (the thing that couples code to a business model) is caught…
    assert!(line_names_erp(r#"let m = resolve_registered("sale.order")?;"#));
    assert!(line_names_erp(r#"register_service!("stock.picking", "reserve", ...)"#));
    // …while prose that merely names a model, or a non-ERP literal, is not.
    assert!(!line_names_erp("// relocate generate_variants to the sales module"));
    assert!(!line_names_erp(r#"resolve_registered("res.company")"#));
    assert!(!line_names_erp("let table = \"mail_mail\";"));
}
