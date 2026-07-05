//! The OpenID Connect (SSO) protocol layer: discovery, PKCE + nonce, id_token verification. It turns a
//! browser round-trip into a VERIFIED email; minting a Kigumi session from that (provisioning + tokens)
//! is the caller's job (the `/auth/oidc` handlers). The security-critical crypto — JWKS signature
//! checks, nonce/iss/aud/exp validation, PKCE — is delegated to the `openidconnect` crate, never
//! hand-rolled.

use kigumi_db::{Db, DbError};
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    reqwest, AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};
use tokio::sync::OnceCell;

/// What can go wrong in the OIDC handshake; the caller maps each to an HTTP status.
pub enum OidcError {
    /// IdP discovery / JWKS unreachable or malformed — an upstream failure (→ 502).
    Discovery(String),
    /// Token-endpoint exchange failed — upstream/network (→ 502).
    Exchange(String),
    /// id_token signature / nonce / iss / aud / exp verification failed (→ 401).
    Verify(String),
    /// Unknown, already-used, or expired `state` (→ 400).
    InvalidState,
    /// The id_token carried no email claim (→ 400).
    NoEmail,
    /// The email is present but not marked verified by the IdP (→ 403).
    UnverifiedEmail,
    /// A database error storing/taking the flow or provisioning (→ 500).
    Db(DbError),
}

impl From<DbError> for OidcError {
    fn from(e: DbError) -> Self {
        OidcError::Db(e)
    }
}

/// Everything needed to run the OIDC flow for ONE configured IdP. Discovery metadata is fetched once
/// (lazily) and cached; the typed client is rebuilt per request from it (cheap, no network), which
/// avoids threading openidconnect's verbose client type through the program.
pub struct OidcState {
    issuer: IssuerUrl,
    client_id: ClientId,
    client_secret: ClientSecret,
    redirect_uri: RedirectUrl,
    /// Where the browser is sent after a successful login, with the minted tokens in the URL fragment.
    pub post_login_url: String,
    http: reqwest::Client,
    metadata: OnceCell<CoreProviderMetadata>,
}

impl OidcState {
    /// Builds the OIDC state from validated config values (the host supplies the secret from the env).
    /// Returns `None` if the issuer or redirect URL is malformed, or the HTTP client cannot be built.
    pub fn new(
        issuer: &str,
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
        post_login_url: &str,
    ) -> Option<Self> {
        // A client that NEVER follows redirects — SSRF hardening for the server-to-IdP calls.
        let http = reqwest::ClientBuilder::new()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok()?;
        Some(OidcState {
            issuer: IssuerUrl::new(issuer.to_string()).ok()?,
            client_id: ClientId::new(client_id.to_string()),
            client_secret: ClientSecret::new(client_secret.to_string()),
            redirect_uri: RedirectUrl::new(redirect_uri.to_string()).ok()?,
            post_login_url: post_login_url.to_string(),
            http,
            metadata: OnceCell::new(),
        })
    }

    /// The provider metadata, discovered once and cached. A failed discovery is not cached (retried).
    async fn metadata(&self) -> Result<CoreProviderMetadata, OidcError> {
        self.metadata
            .get_or_try_init(|| async {
                CoreProviderMetadata::discover_async(self.issuer.clone(), &self.http).await
            })
            .await
            .map(|m| m.clone())
            .map_err(|e| OidcError::Discovery(e.to_string()))
    }

    /// Builds the IdP authorization URL and records the in-flight flow (state → nonce + PKCE verifier).
    /// Returns `(url, state)`: the URL to redirect the browser to, and the `state` the caller must also
    /// pin to this browser (a cookie) so the callback can reject a state issued to a different browser
    /// — the login-CSRF defense.
    pub async fn authorize(&self, db: &Db) -> Result<(String, String), OidcError> {
        let meta = self.metadata().await?;
        let client =
            CoreClient::from_provider_metadata(meta, self.client_id.clone(), Some(self.client_secret.clone()))
                .set_redirect_uri(self.redirect_uri.clone());

        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, csrf, nonce) = client
            .authorize_url(CoreAuthenticationFlow::AuthorizationCode, CsrfToken::new_random, Nonce::new_random)
            .add_scope(Scope::new("email".to_string()))
            .set_pkce_challenge(challenge)
            .url();
        let state = csrf.secret().to_string();
        db.store_oidc_flow(&state, nonce.secret(), verifier.secret()).await?;
        Ok((url.to_string(), state))
    }

    /// Consumes the flow for `state`, exchanges `code`, verifies the id_token, and returns the caller's
    /// VERIFIED email. Rejects an unknown/expired state, a failed exchange, a bad signature/nonce, and
    /// an unverified email.
    pub async fn exchange_and_verify(&self, db: &Db, code: &str, state: &str) -> Result<String, OidcError> {
        // One-shot: the state is deleted whether or not it was still valid, so it cannot be replayed.
        let (nonce_s, verifier_s) = db.take_oidc_flow(state).await?.ok_or(OidcError::InvalidState)?;
        let meta = self.metadata().await?;
        let client =
            CoreClient::from_provider_metadata(meta, self.client_id.clone(), Some(self.client_secret.clone()))
                .set_redirect_uri(self.redirect_uri.clone());

        let token_response = client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .map_err(|e| OidcError::Exchange(e.to_string()))?
            .set_pkce_verifier(PkceCodeVerifier::new(verifier_s))
            .request_async(&self.http)
            .await
            .map_err(|e| OidcError::Exchange(e.to_string()))?;

        let id_token = token_response.id_token().ok_or_else(|| OidcError::Verify("no id_token".into()))?;
        // `claims` verifies the signature (against the IdP's JWKS), the nonce binding, and iss/aud/exp.
        let claims = id_token
            .claims(&client.id_token_verifier(), &Nonce::new(nonce_s))
            .map_err(|e| OidcError::Verify(e.to_string()))?;

        let email = claims.email().ok_or(OidcError::NoEmail)?;
        // email_verified MUST be explicitly true: a present-but-unverified email is untrusted, since an
        // attacker could add that address to their own IdP account without controlling the mailbox.
        if claims.email_verified() != Some(true) {
            return Err(OidcError::UnverifiedEmail);
        }
        // Canonicalize the identity (trim + lowercase) so a JIT user is stored consistently and links
        // deterministically to an existing account regardless of the casing the IdP echoes.
        Ok(email.as_str().trim().to_ascii_lowercase())
    }
}
