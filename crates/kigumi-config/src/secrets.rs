//! Secrets — read from the environment ONLY (never from `kigumi.toml`), presence verified at
//! startup. `DATABASE_URL` is the single source of the database connection identity.

use crate::ConfigError;
use url::Url;

/// All runtime secrets. Construct via [`Secrets::from_env`]; never serialize this.
pub struct Secrets {
    /// Full Postgres DSN — the sole connection identity (host, port, db, user, password, sslmode).
    pub database_url: String,
    /// HS256 signing secret for access/refresh tokens.
    pub jwt_secret: String,
    /// Previous JWT secret, still ACCEPTED on verify during a rotation window (kid-keyed rotation).
    pub jwt_secret_old: Option<String>,
    pub smtp_password: Option<String>,
    /// Bearer token gating destructive db operations (dump/restore/gc). Optional at boot.
    pub admin_token: Option<String>,
}

impl Secrets {
    pub fn from_env() -> Result<Secrets, ConfigError> {
        let database_url = req("DATABASE_URL")?;
        // Validate it is a parseable Postgres DSN now, not at first query.
        match Url::parse(&database_url) {
            Ok(u) if matches!(u.scheme(), "postgres" | "postgresql") => {}
            _ => return Err(ConfigError::Secret("DATABASE_URL is not a valid postgres:// URL".into())),
        }
        Ok(Secrets {
            database_url,
            jwt_secret: req("KIGUMI_JWT_SECRET")?,
            jwt_secret_old: opt("KIGUMI_JWT_SECRET_OLD"),
            smtp_password: opt("KIGUMI_SMTP_PASSWORD"),
            admin_token: opt("KIGUMI_ADMIN_TOKEN"),
        })
    }
}

fn req(key: &'static str) -> Result<String, ConfigError> {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(ConfigError::MissingSecret(key)),
    }
}

fn opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Masks the password component of a DSN while keeping host/port/db/user visible — safe to print.
pub fn redact_db_url(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut u) => {
            if u.password().is_some() {
                let _ = u.set_password(Some("****"));
            }
            u.to_string()
        }
        Err(_) => "<unparseable DATABASE_URL>".into(),
    }
}
