//! Hub configuration.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

pub const ADMIN_TOKEN_ENV: &str = "SPACETRACE_HUB_ADMIN_TOKEN";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Where snapshots and hub tables live. One SQLite file.
    pub db: PathBuf,

    /// Bind address.
    ///
    /// Unlike the agent, this defaults to all interfaces: a hub nobody can
    /// reach is not a hub. Put TLS in front of it.
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,

    /// Token for the dashboard. Prefer `admin_token_file` or the environment.
    #[serde(default)]
    pub admin_token: Option<String>,

    #[serde(default)]
    pub admin_token_file: Option<PathBuf>,

    /// Largest snapshot body an agent may push, in bytes.
    #[serde(default = "default_max_upload")]
    pub max_upload_bytes: usize,

    /// Snapshots to keep per target. Unset keeps everything, which on a hub
    /// collecting from a fleet every night will grow without bound.
    #[serde(default)]
    pub keep_per_target: Option<usize>,
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 8080))
}

fn default_max_upload() -> usize {
    512 * 1024 * 1024
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let config: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if let Some(keep) = self.keep_per_target {
            anyhow::ensure!(
                keep > 0,
                "keep_per_target must be at least 1 (omit it to keep everything)"
            );
        }
        anyhow::ensure!(
            self.max_upload_bytes >= 1024,
            "max_upload_bytes is implausibly small: {}",
            self.max_upload_bytes
        );
        Ok(())
    }

    /// Resolve the dashboard token from the config, a file, or the environment.
    ///
    /// `None` means the server refuses to start. The dashboard shows every
    /// path on every machine in the fleet; leaving that open by default would
    /// be indefensible.
    pub fn resolve_admin_token(&self) -> Result<Option<String>> {
        if let Some(token) = &self.admin_token {
            let token = token.trim();
            if !token.is_empty() {
                return Ok(Some(token.to_string()));
            }
        }
        if let Some(path) = &self.admin_token_file {
            let raw = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            let token = raw.trim().to_string();
            anyhow::ensure!(!token.is_empty(), "token file is empty: {}", path.display());
            return Ok(Some(token));
        }
        match std::env::var(ADMIN_TOKEN_ENV) {
            Ok(token) if !token.trim().is_empty() => Ok(Some(token.trim().to_string())),
            _ => Ok(None),
        }
    }
}

pub const EXAMPLE_CONFIG: &str = r#"# spacetrace hub configuration.

# Snapshots pushed by agents, plus the hub's own tables, in one SQLite file.
db = "/var/lib/spacetrace-hub/hub.sqlite"

# All interfaces by default: a hub nobody can reach is not a hub. Put a reverse
# proxy with TLS in front of it before exposing it beyond your own network.
listen = "0.0.0.0:8080"

# Token for the dashboard. Generate one with:
#   head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'
# Prefer the file, or set SPACETRACE_HUB_ADMIN_TOKEN.
admin_token_file = "/etc/spacetrace-hub/admin-token"

# Agents get their own tokens, created on the Agents page. They can only push
# snapshots; they cannot read the dashboard.

# Snapshots to keep per (host, root). A fleet scanning nightly will fill a disk
# eventually, which would be an unusually poor look for this particular tool.
keep_per_target = 90
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_config_parses_and_validates() {
        let config: Config = toml::from_str(EXAMPLE_CONFIG).expect("must parse");
        config.validate().expect("must validate");
        assert_eq!(config.keep_per_target, Some(90));
        assert_eq!(config.listen.port(), 8080);
        assert!(!config.listen.ip().is_loopback(), "the hub listens outward");
    }

    #[test]
    fn a_minimal_config_is_enough() {
        let config: Config = toml::from_str(r#"db = "/tmp/hub.sqlite""#).unwrap();
        assert_eq!(config.listen, default_listen());
        assert!(config.keep_per_target.is_none());
        assert_eq!(config.max_upload_bytes, default_max_upload());
    }

    #[test]
    fn unknown_keys_are_rejected_rather_than_ignored() {
        let err = toml::from_str::<Config>("db = \"/tmp/x\"\nlisten_addr = \"x\"\n").unwrap_err();
        assert!(err.to_string().contains("listen_addr"), "{err}");
    }

    #[test]
    fn nonsense_retention_and_limits_are_refused() {
        let zero: Config = toml::from_str("db = \"/tmp/x\"\nkeep_per_target = 0\n").unwrap();
        assert!(zero.validate().is_err());

        let tiny: Config = toml::from_str("db = \"/tmp/x\"\nmax_upload_bytes = 10\n").unwrap();
        assert!(tiny.validate().is_err());
    }

    #[test]
    fn a_token_file_is_read_and_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "  s3cret\n\n").unwrap();

        let config = Config {
            db: PathBuf::from("/tmp/x"),
            listen: default_listen(),
            admin_token: None,
            admin_token_file: Some(path),
            max_upload_bytes: default_max_upload(),
            keep_per_target: None,
        };
        assert_eq!(
            config.resolve_admin_token().unwrap().as_deref(),
            Some("s3cret")
        );
    }

    #[test]
    fn an_inline_token_wins_over_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "from-file").unwrap();

        let config = Config {
            db: PathBuf::from("/tmp/x"),
            listen: default_listen(),
            admin_token: Some("inline".into()),
            admin_token_file: Some(path),
            max_upload_bytes: default_max_upload(),
            keep_per_target: None,
        };
        assert_eq!(
            config.resolve_admin_token().unwrap().as_deref(),
            Some("inline")
        );
    }

    #[test]
    fn an_empty_token_file_is_an_error_not_an_empty_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "  \n").unwrap();

        let config = Config {
            db: PathBuf::from("/tmp/x"),
            listen: default_listen(),
            admin_token: None,
            admin_token_file: Some(path),
            max_upload_bytes: default_max_upload(),
            keep_per_target: None,
        };
        assert!(config.resolve_admin_token().is_err());
    }

    #[test]
    fn a_blank_inline_token_falls_through_rather_than_being_accepted() {
        let config = Config {
            db: PathBuf::from("/tmp/x"),
            listen: default_listen(),
            admin_token: Some("   ".into()),
            admin_token_file: None,
            max_upload_bytes: default_max_upload(),
            keep_per_target: None,
        };
        // With nothing else configured this must be "no token", so the server
        // refuses to start rather than accepting an empty string as valid.
        let resolved = config.resolve_admin_token().unwrap();
        assert!(
            resolved.is_none() || resolved.as_deref() != Some("   "),
            "a blank token must never be usable"
        );
    }
}
