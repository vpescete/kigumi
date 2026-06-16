//! Authentication: turn a signed bearer token into a TRUSTED [`Ctx`].
//!
//! This replaces "trust the client's identity headers" (forgeable by anyone) with an HS256 JWT
//! signed by a server secret: a client cannot mint a valid token for a group it was not granted,
//! and tampered/expired tokens are rejected. Token verification (signature, algorithm, expiry) is
//! delegated to the battle-tested `jsonwebtoken` crate. Issuing tokens (after a real credential
//! check, which is out of scope here) is provided for login endpoints and tests.

use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use meshble_core::Ctx;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Claims {
    /// Subject: the user id.
    sub: i64,
    groups: Vec<String>,
    /// Expiry (unix seconds). Validated by `jsonwebtoken`.
    exp: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    /// No bearer token was presented.
    Missing,
    /// The token is malformed, has a bad signature/algorithm, or is expired.
    Invalid,
}

/// Issues and verifies HS256 tokens with a shared secret.
pub struct Authenticator {
    secret: String,
}

impl Authenticator {
    pub fn new(secret: impl Into<String>) -> Self {
        Self { secret: secret.into() }
    }

    /// Signs a token for `uid`/`groups` valid for `ttl_secs`. Use after authenticating a user.
    pub fn issue(&self, uid: i64, groups: Vec<String>, ttl_secs: u64) -> Result<String, AuthError> {
        let exp = (now_unix() + ttl_secs) as usize;
        let claims = Claims { sub: uid, groups, exp };
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(self.secret.as_bytes()),
        )
        .map_err(|_| AuthError::Invalid)
    }

    /// Verifies a token (signature, HS256 algorithm, and expiry) into a trusted [`Ctx`].
    pub fn verify(&self, token: &str) -> Result<Ctx, AuthError> {
        // Validation::new pins the algorithm to HS256 and validates `exp`, rejecting the classic
        // alg=none / algorithm-confusion attacks. Leeway 0 → no grace window on expiry.
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = 0;
        let data = decode::<Claims>(
            token,
            &DecodingKey::from_secret(self.secret.as_bytes()),
            &validation,
        )
        .map_err(|_| AuthError::Invalid)?;
        Ok(Ctx::new(data.claims.sub, data.claims.groups))
    }

    /// Extracts a `Bearer <token>` from an Authorization header value and verifies it.
    pub fn verify_bearer(&self, authorization: Option<&str>) -> Result<Ctx, AuthError> {
        let header = authorization.ok_or(AuthError::Missing)?;
        let token = header.strip_prefix("Bearer ").ok_or(AuthError::Invalid)?;
        self.verify(token)
    }
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_token_verifies_to_its_claims() {
        let auth = Authenticator::new("secret");
        let token = auth.issue(7, vec!["sales.user".to_string()], 3600).unwrap();
        let ctx = auth.verify(&token).unwrap();
        assert_eq!(ctx.uid, 7);
        assert!(ctx.is_member("sales.user"));
    }

    #[test]
    fn token_from_a_different_secret_is_rejected() {
        // The forge-resistance guarantee: a token minted with another secret must not verify.
        let token = Authenticator::new("attacker-secret")
            .issue(1, vec!["admin".to_string()], 3600)
            .unwrap();
        assert!(matches!(
            Authenticator::new("server-secret").verify(&token),
            Err(AuthError::Invalid)
        ));
    }

    #[test]
    fn tampered_and_garbage_tokens_are_rejected() {
        let auth = Authenticator::new("secret");
        let mut token = auth.issue(1, vec![], 3600).unwrap();
        token.push('x'); // corrupt the signature
        assert!(matches!(auth.verify(&token), Err(AuthError::Invalid)));
        assert!(matches!(auth.verify("not-a-jwt"), Err(AuthError::Invalid)));
    }

    #[test]
    fn bearer_parsing() {
        let auth = Authenticator::new("secret");
        let token = auth.issue(3, vec![], 3600).unwrap();
        assert!(auth.verify_bearer(Some(&format!("Bearer {token}"))).is_ok());
        assert!(matches!(auth.verify_bearer(None), Err(AuthError::Missing)));
        // No "Bearer " prefix.
        assert!(matches!(auth.verify_bearer(Some(&token)), Err(AuthError::Invalid)));
    }
}
