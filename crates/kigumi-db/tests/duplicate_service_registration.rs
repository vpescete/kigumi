//! The db-side half of the boot guard: a duplicate service must fail validation, and
//! `validate_registrations` must also run the core registries. Own test binary — `inventory` is
//! per-binary, so the deliberately broken registration cannot leak into another test.

use kigumi_core::inventory;
use kigumi_db::{
    validate_registrations, BoxServiceFut, DbError, ServiceCtx, ServiceInput, ServiceOutput,
    ServiceRegistration,
};

/// Never runs — the point is the registration, not the body.
fn unused<'c, 'a, 't>(
    _cx: &'c mut ServiceCtx<'a, 't>,
    _inp: ServiceInput,
) -> BoxServiceFut<'c, Result<ServiceOutput, DbError>> {
    Box::pin(async { Err(DbError::BadInput("unused".to_string())) })
}

// Two services on the same (model, name). `service_for` is a `.find()`, so today one silently wins
// and which one depends on crate link order.
inventory::submit! {
    ServiceRegistration { model: "dup.model", name: "collide", func: unused, write_gate: true, groups: &[] }
}
inventory::submit! {
    ServiceRegistration { model: "dup.model", name: "collide", func: unused, write_gate: true, groups: &[] }
}

#[test]
fn a_duplicate_service_fails_validation_naming_the_key() {
    let err = validate_registrations().expect_err("a shadowed service must not pass boot");
    assert!(err.contains("dup.model"), "names the colliding model: {err}");
    assert!(err.contains("collide"), "names the colliding service: {err}");
}
