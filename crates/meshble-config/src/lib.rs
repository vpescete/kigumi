//! Typed instance configuration — the principled replacement for `odoo.conf`.
//!
//! Two planes (see `docs/OPERATIONS.md`):
//! - **boot-time** (this crate): non-secret settings from `defaults < meshble.toml < env`, parsed
//!   into a typed [`Config`] and validated **fail-fast** (a typo in a core section refuses to boot,
//!   unlike `odoo.conf` which silently ignores it).
//! - **secrets**: never in the file — [`Secrets`] are read from the environment only and their
//!   presence is verified at startup.
//!
//! Connection identity is the single `DATABASE_URL` (a complete DSN); `[database]` carries only
//! non-URL tuning, so there is no ambiguous overlap. Runtime settings (base_url, mode, neutralized,
//! banner) are NOT here — they live in the database and are authoritative there.

use std::collections::BTreeMap;
use std::path::Path;

use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};

mod secrets;
pub use secrets::{redact_db_url, Secrets};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config: {0}")]
    Figment(#[from] figment::Error),
    #[error("config invalid: {0}")]
    Invalid(String),
    #[error("missing required secret: {0}")]
    MissingSecret(&'static str),
    #[error("{0}")]
    Secret(String),
}

/// Boot-time configuration (everything serializable to `meshble.toml`; never a secret).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub instance: Instance,
    #[serde(default)]
    pub server: Server,
    #[serde(default)]
    pub database: Database,
    #[serde(default)]
    pub storage: Storage,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub mail: Mail,
    #[serde(default)]
    pub modules: Modules,
    #[serde(default)]
    pub log: Log,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub name: String,
}
impl Default for Instance {
    fn default() -> Self {
        Self { name: "meshble".into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub bind: String,
    pub workers: usize,
    pub proxy_mode: bool,
}
impl Default for Server {
    fn default() -> Self {
        Self { bind: "127.0.0.1:8099".into(), workers: 4, proxy_mode: false }
    }
}

/// Connection TUNING only — the connection identity (host/port/db/user/password/sslmode) is the
/// single `DATABASE_URL` env var, so there is no host/name field here to conflict with it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Database {
    pub pool_max: u32,
    pub connect_timeout: String,
}
impl Default for Database {
    fn default() -> Self {
        Self { pool_max: 10, connect_timeout: "5s".into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StorageBackend {
    #[default]
    Fs,
    S3,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    #[serde(default)]
    pub backend: StorageBackend,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub bucket: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    pub access_ttl: u64,
    pub refresh_ttl: u64,
}
impl Default for Auth {
    fn default() -> Self {
        Self { access_ttl: 900, refresh_ttl: 2_592_000 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Mail {
    #[serde(default)]
    pub smtp_host: Option<String>,
    #[serde(default)]
    pub smtp_port: Option<u16>,
    #[serde(default)]
    pub from: Option<String>,
}

/// Core `[modules]` keys are strict, but each `[modules.<name>]` subtree is OPEN — captured verbatim
/// and validated by the owning module at load, so a module can carry its own settings without the
/// instance refusing to boot.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Modules {
    #[serde(default)]
    pub load: Vec<String>,
    #[serde(flatten, default)]
    pub per_module: BTreeMap<String, figment::value::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Log {
    pub level: String,
    pub format: String,
}
impl Default for Log {
    fn default() -> Self {
        Self { level: "info".into(), format: "text".into() }
    }
}

impl Config {
    fn base() -> Figment {
        Figment::from(Serialized::defaults(Config::default()))
    }

    /// Loads `defaults < meshble.toml (if present) < MESHBLE_CONF_* env` and validates. The env
    /// prefix is deliberately distinct from the secret env vars (DATABASE_URL, MESHBLE_JWT_SECRET, …)
    /// so secrets are never captured by the config layer.
    pub fn load(path: Option<&Path>) -> Result<Config, ConfigError> {
        let mut fig = Config::base();
        if let Some(p) = path {
            if p.exists() {
                fig = fig.merge(Toml::file(p));
            }
        }
        fig = fig.merge(Env::prefixed("MESHBLE_CONF_").split("__"));
        let cfg: Config = fig.extract()?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Parses + validates config from a TOML string (used by tests and embedding hosts).
    pub fn from_toml_str(s: &str) -> Result<Config, ConfigError> {
        let cfg: Config = Config::base().merge(Toml::string(s)).extract()?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Cross-field checks serde cannot express.
    pub fn validate(&self) -> Result<(), ConfigError> {
        match self.storage.backend {
            StorageBackend::Fs if self.storage.path.is_none() => {
                return Err(ConfigError::Invalid("storage.backend = fs requires storage.path".into()))
            }
            StorageBackend::S3 if self.storage.bucket.is_none() => {
                return Err(ConfigError::Invalid("storage.backend = s3 requires storage.bucket".into()))
            }
            _ => {}
        }
        if self.server.bind.parse::<std::net::SocketAddr>().is_err() {
            return Err(ConfigError::Invalid(format!("server.bind is not a host:port ({})", self.server.bind)));
        }
        Ok(())
    }
}

/// The full effective settings: boot-time [`Config`] + env-only [`Secrets`].
pub struct Settings {
    pub config: Config,
    pub secrets: Secrets,
}

impl Settings {
    /// Loads config from `path` and secrets from the environment, validating both (and their
    /// interplay, e.g. SMTP host configured but no password).
    pub fn load(path: Option<&Path>) -> Result<Settings, ConfigError> {
        let config = Config::load(path)?;
        let secrets = Secrets::from_env()?;
        if config.mail.smtp_host.is_some() && secrets.smtp_password.is_none() {
            return Err(ConfigError::Secret(
                "mail.smtp_host is set but MESHBLE_SMTP_PASSWORD is not".into(),
            ));
        }
        Ok(Settings { config, secrets })
    }

    /// A human-readable dump with EVERY secret redacted at the field level (the DATABASE_URL password
    /// is masked while host/db/user stay visible) — safe to paste into a support ticket.
    pub fn redacted(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("{:#?}\n", self.config));
        out.push_str("[secrets]\n");
        out.push_str(&format!("  database_url = {}\n", redact_db_url(&self.secrets.database_url)));
        out.push_str(&format!("  jwt_secret = {}\n", mask(Some(&self.secrets.jwt_secret))));
        out.push_str(&format!("  jwt_secret_old = {}\n", mask(self.secrets.jwt_secret_old.as_deref())));
        out.push_str(&format!("  smtp_password = {}\n", mask(self.secrets.smtp_password.as_deref())));
        out.push_str(&format!("  admin_token = {}\n", mask(self.secrets.admin_token.as_deref())));
        out
    }
}

fn mask(v: Option<&str>) -> &'static str {
    match v {
        Some(_) => "set (****)",
        None => "unset",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        // The Default impl (validation is separate — a bare config is intentionally invalid until a
        // storage.path is given, see `fs_storage_without_path_fails_validation`).
        let c = Config::default();
        assert_eq!(c.instance.name, "meshble");
        assert_eq!(c.server.bind, "127.0.0.1:8099");
        assert_eq!(c.auth.access_ttl, 900);
        assert_eq!(c.storage.backend, StorageBackend::Fs);
    }

    #[test]
    fn toml_overrides_defaults() {
        let c = Config::from_toml_str(
            r#"
            [server]
            bind = "0.0.0.0:9000"
            workers = 8
            proxy_mode = true
            [storage]
            backend = "fs"
            path = "/data/blobs"
            [modules]
            load = ["base", "sales"]
            "#,
        )
        .unwrap();
        assert_eq!(c.server.bind, "0.0.0.0:9000");
        assert_eq!(c.server.workers, 8);
        assert!(c.server.proxy_mode);
        assert_eq!(c.modules.load, vec!["base", "sales"]);
    }

    #[test]
    fn unknown_top_level_section_is_rejected() {
        // A typo'd section must fail-fast, not be silently ignored (the odoo.conf flaw).
        let err = Config::from_toml_str("[serever]\nbind = \"x\"\n");
        assert!(err.is_err());
    }

    #[test]
    fn unknown_key_in_core_section_is_rejected() {
        let err = Config::from_toml_str("[server]\nbnid = \"x\"\n");
        assert!(err.is_err());
    }

    #[test]
    fn host_in_database_section_is_rejected() {
        // Connection identity is DATABASE_URL only; a host here would be an ambiguous overlap, so it
        // is an unknown key and refused.
        let err = Config::from_toml_str("[database]\nhost = \"db.internal\"\n");
        assert!(err.is_err());
    }

    #[test]
    fn module_subtree_is_open() {
        let c = Config::from_toml_str(
            r#"
            [storage]
            path = "/data/blobs"
            [modules]
            load = ["sales"]
            [modules.sales]
            default_tax = "0.22"
            "#,
        )
        .unwrap();
        assert!(c.modules.per_module.contains_key("sales"), "[modules.sales] captured");
        assert!(!c.modules.per_module.contains_key("load"), "named field not in the open map");
    }

    #[test]
    fn fs_storage_without_path_fails_validation() {
        let err = Config::from_toml_str("[storage]\nbackend = \"fs\"\n");
        assert!(matches!(err, Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn bad_bind_fails_validation() {
        let err = Config::from_toml_str("[storage]\npath=\"/d\"\n[server]\nbind = \"not-a-socket\"\n");
        assert!(matches!(err, Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn database_url_password_is_redacted() {
        let r = redact_db_url("postgres://user:supersecret@db.internal:5432/meshble_prod");
        assert!(!r.contains("supersecret"), "password must be masked");
        assert!(r.contains("db.internal") && r.contains("meshble_prod"), "host/db stay visible");
    }
}
