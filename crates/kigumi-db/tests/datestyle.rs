//! Every pooled connection pins `DateStyle = ISO, YMD`, so `::text` dates are always big-endian
//! ISO (lexical order == chronological) regardless of the server/role default — the invariant the
//! activity-state derivation, tracking diffs and the frontend's date parsing all rely on. Live PG.

use kigumi_db::Db;

#[tokio::test]
async fn connection_pins_iso_datestyle() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        }
    };
    let db = Db::connect(&url).await.unwrap();
    let ds: String = sqlx::query_scalar("SHOW DateStyle").fetch_one(db.pool()).await.unwrap();
    assert!(ds.starts_with("ISO, YMD"), "expected ISO, YMD; got {ds}");

    // And a date renders ISO, so lexical compare on `::text` is chronological.
    let d: String = sqlx::query_scalar("SELECT '2027-01-01'::date::text").fetch_one(db.pool()).await.unwrap();
    assert_eq!(d, "2027-01-01");
}
