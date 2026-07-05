//! OIDC server-side DB state: the one-shot login-flow rows and JIT / link-by-email provisioning.
//! The OIDC handshake itself (discovery, PKCE, id_token verification) is the openidconnect crate's
//! job and is not exercised here. Requires DATABASE_URL.

#[tokio::test]
async fn oidc_flow_is_one_shot_and_provisioning_links_or_creates() {
    let Some(t) = kigumi_test::TestDb::new().await else { return };
    let db = &t.db;

    // Flow state: store, then a SINGLE take returns it; a replay (or an unknown state) returns None.
    db.store_oidc_flow("state-1", "nonce-1", "verifier-1").await.unwrap();
    assert_eq!(
        db.take_oidc_flow("state-1").await.unwrap(),
        Some(("nonce-1".to_string(), "verifier-1".to_string()))
    );
    assert_eq!(db.take_oidc_flow("state-1").await.unwrap(), None, "a state cannot be replayed");
    assert_eq!(db.take_oidc_flow("never-stored").await.unwrap(), None);

    // JIT: an unknown email is provisioned with NO groups and an unusable password.
    let jit = db.find_or_create_oidc_user("new@example.com").await.unwrap();
    assert!(jit.groups.is_empty(), "a JIT user starts with no groups");
    assert_eq!(jit.password_hash, "!", "the sentinel hash disables password login");
    // Idempotent: a second login for the same email returns the SAME user, never a duplicate.
    let again = db.find_or_create_oidc_user("new@example.com").await.unwrap();
    assert_eq!(again.id, jit.id);

    // Link by email is case-INSENSITIVE: a password user stored mixed-case links to the lowercased
    // OIDC identity (never a duplicate), keeping its groups and password.
    let uid = db.upsert_user("Alice@Example.com", "argon2-real-hash", &["admin"]).await.unwrap();
    let linked = db.find_or_create_oidc_user("alice@example.com").await.unwrap();
    assert_eq!(linked.id, uid, "case-variant email links to the SAME account");
    assert_eq!(linked.groups, vec!["admin".to_string()]);
    assert_eq!(linked.password_hash, "argon2-real-hash", "linking never clobbers the password");
}
