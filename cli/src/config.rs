use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    pub server_url: Option<String>,
    pub token: Option<String>,
    pub registry: Option<String>,

    /// Cached OIDC refresh token (stored after `auth login`).
    #[serde(default)]
    pub oidc_refresh_token: Option<String>,
    /// Unix timestamp (seconds) when the OIDC access token expires.
    #[serde(default)]
    pub oidc_expires_at: Option<i64>,
    /// OIDC provider name used at login — forwarded on refresh so the server
    /// selects the correct provider in multi-provider deployments.
    #[serde(default)]
    pub oidc_provider: Option<String>,

    /// Path to a Kubernetes service account token file; read fresh on every request.
    #[serde(default)]
    pub kubernetes_token_path: Option<String>,
}

impl Profile {
    /// True if the OIDC access token expires within the next 120 seconds.
    pub fn is_token_expiring_soon(&self) -> bool {
        match self.oidc_expires_at {
            Some(exp) => Utc::now().timestamp() >= exp - 120,
            None => false,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(default)]
    pub default: Profile,
    #[serde(default)]
    pub profiles: HashMap<String, Profile>,
}

/// Resolved connection settings after merging config file + CLI overrides.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub server_url: String,
    pub token: Option<String>,
    pub registry: Option<String>,
}

impl ConfigFile {
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("batlehub")
            .join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&content).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating config dir {}", parent.display()))?;
            // The file below is 0600, but a 0755 directory around it still
            // tells every other local user which servers and profiles exist.
            // `contract.rs` already restricts its own parent this way.
            restrict_dir_to_owner(parent);
        }
        let content = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;

        // This file holds live bearer/OIDC/k8s tokens; don't leave it readable
        // by other local users under a permissive umask.
        restrict_to_owner(&path);

        Ok(())
    }

    pub fn resolve(
        &self,
        profile: Option<&str>,
        server_override: Option<String>,
        token_override: Option<String>,
        registry_override: Option<String>,
    ) -> ResolvedConfig {
        let base = match profile {
            Some(name) => self.profiles.get(name).cloned().unwrap_or_default(),
            None => self.default.clone(),
        };

        let server_url = server_override
            .or(base.server_url)
            .unwrap_or_else(|| "http://localhost:8080".to_string());

        let token = token_override.or(base.token);
        let registry = registry_override.or(base.registry);

        ResolvedConfig {
            server_url,
            token,
            registry,
        }
    }
}

/// Restrict `path` to owner-only read/write (`0600`) on Unix. Best-effort: a
/// failure here shouldn't fail the save, since the content was already
/// written successfully.
#[cfg(unix)]
fn restrict_to_owner(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// Restrict `path` to owner-only (`0700`) on Unix. Best-effort, as above.
#[cfg(unix)]
fn restrict_dir_to_owner(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &std::path::Path) {}

#[cfg(not(unix))]
fn restrict_dir_to_owner(_path: &std::path::Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_default(url: &str, token: &str) -> ConfigFile {
        ConfigFile {
            default: Profile {
                server_url: Some(url.to_string()),
                token: Some(token.to_string()),
                registry: None,
                ..Default::default()
            },
            profiles: HashMap::new(),
        }
    }

    #[test]
    fn resolve_uses_default_when_no_profile() {
        let cfg = cfg_with_default("http://example.com", "tok");
        let r = cfg.resolve(None, None, None, None);
        assert_eq!(r.server_url, "http://example.com");
        assert_eq!(r.token.as_deref(), Some("tok"));
    }

    #[test]
    fn cli_overrides_win_over_default() {
        let cfg = cfg_with_default("http://example.com", "tok");
        let r = cfg.resolve(
            None,
            Some("http://other.com".into()),
            Some("override".into()),
            None,
        );
        assert_eq!(r.server_url, "http://other.com");
        assert_eq!(r.token.as_deref(), Some("override"));
    }

    #[test]
    fn resolve_uses_named_profile() {
        let mut cfg = ConfigFile::default();
        cfg.profiles.insert(
            "prod".into(),
            Profile {
                server_url: Some("http://prod.example.com".into()),
                token: Some("prod-tok".into()),
                registry: None,
                ..Default::default()
            },
        );
        let r = cfg.resolve(Some("prod"), None, None, None);
        assert_eq!(r.server_url, "http://prod.example.com");
    }

    #[test]
    fn fallback_to_localhost_when_empty() {
        let cfg = ConfigFile::default();
        let r = cfg.resolve(None, None, None, None);
        assert_eq!(r.server_url, "http://localhost:8080");
        assert!(r.token.is_none());
    }

    #[test]
    fn token_expiring_soon_when_within_threshold() {
        let profile = Profile {
            oidc_expires_at: Some(Utc::now().timestamp() + 60),
            ..Default::default()
        };
        assert!(profile.is_token_expiring_soon());
    }

    #[test]
    fn token_already_expired_is_expiring_soon() {
        let profile = Profile {
            oidc_expires_at: Some(Utc::now().timestamp() - 1),
            ..Default::default()
        };
        assert!(profile.is_token_expiring_soon());
    }

    #[test]
    fn token_not_expiring_when_far_future() {
        let profile = Profile {
            oidc_expires_at: Some(Utc::now().timestamp() + 3600),
            ..Default::default()
        };
        assert!(!profile.is_token_expiring_soon());
    }

    #[test]
    fn token_not_expiring_when_no_expiry_set() {
        let profile = Profile::default();
        assert!(!profile.is_token_expiring_soon());
    }

    #[cfg(unix)]
    #[test]
    fn restrict_dir_to_owner_sets_0700() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("batlehub");
        std::fs::create_dir(&nested).unwrap();
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o755)).unwrap();

        restrict_dir_to_owner(&nested);

        let mode = std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn restrict_to_owner_sets_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "content").unwrap();
        // Start from permissive perms so the test would fail if
        // `restrict_to_owner` were a no-op.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        restrict_to_owner(&path);

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
