//! Form views (a minimal slice of Odoo's form arch): a model may declare how its form lays out — titled
//! groups of scalar fields in a two-column "sheet", plus a notebook of tabbed pages for its One2many
//! relations or secondary details. Emitted in the UI contract; a model with no view gets a smart default
//! layout client-side. Like ACLs and reports, a view is static data registered at compile time, so the
//! layout lives with the model rather than in the frontend.

/// A field placed in a group. `full` makes it span both columns — use it for relations, long text
/// (Html), images, and the primary name.
pub struct FieldSlot {
    pub name: &'static str,
    pub full: bool,
}

/// A titled group of scalar fields, laid out in two columns. `title` is optional (a lead group with no
/// heading is common for the identity fields).
pub struct FieldGroup {
    pub title: Option<&'static str>,
    pub fields: &'static [FieldSlot],
}

/// A notebook page (tab) below the sheet, usually a One2many relation or grouped secondary fields.
pub struct NotebookPage {
    pub title: &'static str,
    pub fields: &'static [&'static str],
}

/// A model's form layout. Emitted by `register_view!`.
pub struct FormView {
    pub model: &'static str,
    pub groups: &'static [FieldGroup],
    pub pages: &'static [NotebookPage],
}
inventory::collect!(FormView);

/// The registered form view for `model`, if any (else the frontend applies a smart default layout).
pub fn view_for(model: &str) -> Option<&'static FormView> {
    inventory::iter::<FormView>.into_iter().find(|v| v.model == model)
}
