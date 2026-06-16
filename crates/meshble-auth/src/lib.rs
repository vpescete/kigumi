//! Authentication: password hashing (argon2) and signed (HS256 JWT) access/refresh tokens.
//!
//! Tokens are TYPED: an `access` token verifies into a [`Ctx`] for data requests; a `refresh`
//! token only proves identity to mint a new access token. A refresh token can NEVER be used as a
//! bearer for data access, and vice versa (the `kind` claim is checked) — this prevents a
//! long-lived refresh token from acting as a god-mode bearer. JWT crypto is delegated to the
//! battle-tested `jsonwebtoken` crate; passwords use argon2.

use std::time::{SystemTime, UNIX_EPOCH};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand_core::{OsRng, RngCore};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use meshble_core::Ctx;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Claims {
    /// Subject: the user id.
    sub: i64,
    /// "access" or "refresh". Verified to prevent token-kind confusion.
    kind: String,
    /// Groups (access tokens only; refresh tokens re-fetch groups at refresh time).
    #[serde(default)]
    groups: Vec<String>,
    /// Token id (refresh tokens only) — the handle the server uses to revoke/rotate.
    #[serde(default)]
    jti: String,
    /// Expiry (unix seconds). Validated by `jsonwebtoken`.
    exp: usize,
}

const ACCESS: &str = "access";
const REFRESH: &str = "refresh";

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    /// No bearer token was presented.
    Missing,
    /// The token is malformed, has a bad signature/algorithm/kind, or is expired.
    Invalid,
}

/// The verified identity carried by a refresh token.
#[derive(Debug, Clone)]
pub struct RefreshClaims {
    pub uid: i64,
    pub jti: String,
}

/// Issues and verifies HS256 tokens with a shared secret.
pub struct Authenticator {
    secret: String,
}

impl Authenticator {
    pub fn new(secret: impl Into<String>) -> Self {
        Self { secret: secret.into() }
    }

    fn sign(&self, claims: &Claims) -> Result<String, AuthError> {
        encode(
            &Header::new(Algorithm::HS256),
            claims,
            &EncodingKey::from_secret(self.secret.as_bytes()),
        )
        .map_err(|_| AuthError::Invalid)
    }

    fn decode_kind(&self, token: &str, kind: &str) -> Result<Claims, AuthError> {
        // Pin HS256 (rejects alg=none/confusion) and validate exp with no grace window.
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = 0;
        let data = decode::<Claims>(
            token,
            &DecodingKey::from_secret(self.secret.as_bytes()),
            &validation,
        )
        .map_err(|_| AuthError::Invalid)?;
        if data.claims.kind != kind {
            return Err(AuthError::Invalid);
        }
        Ok(data.claims)
    }

    /// Signs a short-lived ACCESS token carrying the user's groups.
    pub fn issue_access(&self, uid: i64, groups: Vec<String>, ttl_secs: u64) -> Result<String, AuthError> {
        self.sign(&Claims {
            sub: uid,
            kind: ACCESS.to_string(),
            groups,
            jti: String::new(),
            exp: (now_unix() + ttl_secs) as usize,
        })
    }

    /// Signs a long-lived REFRESH token bound to a server-stored `jti`.
    pub fn issue_refresh(&self, uid: i64, jti: &str, ttl_secs: u64) -> Result<String, AuthError> {
        self.sign(&Claims {
            sub: uid,
            kind: REFRESH.to_string(),
            groups: Vec::new(),
            jti: jti.to_string(),
            exp: (now_unix() + ttl_secs) as usize,
        })
    }

    /// Verifies an ACCESS token into a trusted [`Ctx`]. Rejects refresh tokens.
    pub fn verify_access(&self, token: &str) -> Result<Ctx, AuthError> {
        let c = self.decode_kind(token, ACCESS)?;
        Ok(Ctx::new(c.sub, c.groups))
    }

    /// Verifies a REFRESH token into its identity + token id. Rejects access tokens.
    pub fn verify_refresh(&self, token: &str) -> Result<RefreshClaims, AuthError> {
        let c = self.decode_kind(token, REFRESH)?;
        Ok(RefreshClaims { uid: c.sub, jti: c.jti })
    }

    /// Extracts a `Bearer <token>` from an Authorization header and verifies it as an ACCESS token.
    pub fn verify_bearer(&self, authorization: Option<&str>) -> Result<Ctx, AuthError> {
        let header = authorization.ok_or(AuthError::Missing)?;
        let token = header.strip_prefix("Bearer ").ok_or(AuthError::Invalid)?;
        self.verify_access(token)
    }
}

/// Hashes a password with argon2 (random salt). Store the returned PHC string.
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| AuthError::Invalid)
}

/// Verifies a password against a stored argon2 hash (constant-time inside argon2).
pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

/// A random 128-bit token id (hex) for refresh-token tracking.
pub fn new_jti() -> String {
    let mut b = [0u8; 16];
    OsRng.fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_token_verifies_to_its_claims() {
        let auth = Authenticator::new("secret");
        let token = auth.issue_access(7, vec!["sales.user".to_string()], 3600).unwrap();
        let ctx = auth.verify_access(&token).unwrap();
        assert_eq!(ctx.uid, 7);
        assert!(ctx.is_member("sales.user"));
    }

    #[test]
    fn refresh_token_cannot_be_used_as_a_bearer() {
        // The key separation: a refresh token must NOT grant data access.
        let auth = Authenticator::new("secret");
        let refresh = auth.issue_refresh(1, "jti-1", 3600).unwrap();
        assert!(matches!(auth.verify_access(&refresh), Err(AuthError::Invalid)));
        assert!(matches!(auth.verify_bearer(Some(&format!("Bearer {refresh}"))), Err(AuthError::Invalid)));
        // ...and an access token is not a valid refresh token.
        let access = auth.issue_access(1, vec![], 3600).unwrap();
        assert!(matches!(auth.verify_refresh(&access), Err(AuthError::Invalid)));
    }

    #[test]
    fn refresh_token_verifies_to_uid_and_jti() {
        let auth = Authenticator::new("secret");
        let token = auth.issue_refresh(42, "abc", 3600).unwrap();
        let c = auth.verify_refresh(&token).unwrap();
        assert_eq!(c.uid, 42);
        assert_eq!(c.jti, "abc");
    }

    #[test]
    fn token_from_a_different_secret_is_rejected() {
        let token = Authenticator::new("attacker").issue_access(1, vec!["admin".to_string()], 3600).unwrap();
        assert!(matches!(Authenticator::new("server").verify_access(&token), Err(AuthError::Invalid)));
    }

    #[test]
    fn tampered_and_garbage_tokens_are_rejected() {
        let auth = Authenticator::new("secret");
        let mut token = auth.issue_access(1, vec![], 3600).unwrap();
        token.push('x');
        assert!(matches!(auth.verify_access(&token), Err(AuthError::Invalid)));
        assert!(matches!(auth.verify_access("not-a-jwt"), Err(AuthError::Invalid)));
    }

    #[test]
    fn bearer_parsing() {
        let auth = Authenticator::new("secret");
        let token = auth.issue_access(3, vec![], 3600).unwrap();
        assert!(auth.verify_bearer(Some(&format!("Bearer {token}"))).is_ok());
        assert!(matches!(auth.verify_bearer(None), Err(AuthError::Missing)));
        assert!(matches!(auth.verify_bearer(Some(&token)), Err(AuthError::Invalid)));
    }

    #[test]
    fn password_hash_roundtrip_and_distinct_salts() {
        let h1 = hash_password("correct horse").unwrap();
        let h2 = hash_password("correct horse").unwrap();
        assert_ne!(h1, h2, "random salt → different hashes for the same password");
        assert!(verify_password("correct horse", &h1));
        assert!(!verify_password("battery staple", &h1));
        assert!(!verify_password("x", "not-a-valid-hash"));
    }

    #[test]
    fn jti_is_random() {
        assert_ne!(new_jti(), new_jti());
        assert_eq!(new_jti().len(), 32);
    }
}
