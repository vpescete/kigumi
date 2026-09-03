//! A duplicate registration in a first-match registry must fail the boot instead of resolving by
//! crate link order. Own test binary on purpose: `inventory` is per-binary, so the deliberately
//! broken registration below cannot leak into any other test.

use kigumi_core::{inventory, validate_core_registrations, FormView};

// Two form views for the SAME model. `view_for` is a `.find()`, so today one silently shadows the
// other and which one wins depends on link order.
inventory::submit! { FormView { model: "dup.model", groups: &[], pages: &[] } }
inventory::submit! { FormView { model: "dup.model", groups: &[], pages: &[] } }

#[test]
fn a_duplicate_form_view_fails_validation_naming_the_model() {
    let err = validate_core_registrations().expect_err("a shadowed form view must not pass boot");
    assert!(err.contains("dup.model"), "names the colliding model: {err}");
    assert!(err.contains("form view"), "says what collided: {err}");
}
