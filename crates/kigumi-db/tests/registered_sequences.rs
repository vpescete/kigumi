//! The register_sequence! seam: a module-declared sequence is ensured by the migrate path (via
//! `ensure_registered_sequences`), formats as declared, and an existing counter survives a
//! re-ensure (upgrades never reset numbering). Requires DATABASE_URL.

use kigumi_core::SequenceRegistration;

kigumi_core::inventory::submit! {
    SequenceRegistration { module: "seqtest", code: "SQT", prefix: "SQT/", suffix: "", padding: 4 }
}

#[tokio::test]
async fn registered_sequence_is_ensured_and_counter_survives() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;

    // The kit's reset already ran ensure_registered_sequences; the declared code exists.
    assert_eq!(db.next_value("SQT").await.unwrap(), "SQT/0001");
    assert_eq!(db.next_value("SQT").await.unwrap(), "SQT/0002");

    // Re-ensuring (what every later migrate does) keeps the counter — never resets numbering.
    db.ensure_registered_sequences().await.unwrap();
    assert_eq!(db.next_value("SQT").await.unwrap(), "SQT/0003");
}
