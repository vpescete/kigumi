//! Reports (Odoo's QWeb reports, HTML-first): a named server-side render of a record to an HTML
//! document. Like an action, a report is a pure `fn` registered by (model, name); the endpoint fetches
//! the record under the caller's ACL first, so a report is secured exactly by read access to its
//! record. A PDF is an optional rasterization of the same HTML, behind a server-side trait.

use serde_json::Value as Json;

/// Renders a record — with its inlined One2many children, exactly as `find_one_secured` returns it —
/// to a complete HTML document. Pure: no DB access and no recompute; it reads only the fields present.
pub type ReportFn = fn(&Json) -> String;

/// Registration of a report by (model, name), emitted by `register_report!`. `name` is the URL segment
/// (e.g. "quotation"); `title` is the human label used in the contract and the download filename.
pub struct ReportRegistration {
    pub model: &'static str,
    pub name: &'static str,
    pub title: &'static str,
    pub func: ReportFn,
}
inventory::collect!(ReportRegistration);

/// Looks up a registered report by model + name.
pub fn report_for(model: &str, name: &str) -> Option<&'static ReportRegistration> {
    inventory::iter::<ReportRegistration>.into_iter().find(|r| r.model == model && r.name == name)
}

/// All reports registered on `model` (for the UI contract, so a form can offer its "Print" buttons).
pub fn reports_for(model: &str) -> Vec<&'static ReportRegistration> {
    inventory::iter::<ReportRegistration>.into_iter().filter(|r| r.model == model).collect()
}
