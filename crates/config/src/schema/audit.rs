//! `[audit]` (RFC 0036 §4.1): the audit trail's lifecycle — how long each class
//! of row is kept, when access rows lose their precision, how the trail is
//! sealed, and the key erasure pseudonymises with.
//!
//! **Absent means unchanged.** No `[audit]` block expires nothing,
//! pseudonymises nothing and seals nothing; `AppConfig::warnings` says so once.
//! An upgrade that deleted a year of rows unasked is the incident this exists
//! to prevent (RFC 0036 §11 q4).

use anyhow::{bail, Result};
use serde::Deserialize;

use super::air_gap::valid_ed25519_hex_key;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditConfig {
    /// Days a `download` / `view_metadata` row is kept. `0` never expires.
    #[serde(default)]
    pub access_retention_days: u32,
    /// Days every other row — admin actions, sign-ins, purges — is kept. `0`
    /// never expires.
    #[serde(default)]
    pub security_retention_days: u32,
    /// Days after which an access row's IP is truncated (to /24 or /48) and its
    /// user agent dropped. `0` disables.
    #[serde(default)]
    pub pseudonymise_after_days: u32,
    /// How often a sealing window closes. `0` disables sealing.
    #[serde(default)]
    pub seal_interval_secs: u64,
    /// The Ed25519 key that signs the seal chain: a 32-byte seed, hex — the
    /// form every other signing key in this configuration takes.
    #[serde(default)]
    pub seal_signing_key: Option<String>,
    /// The secret erasure keys its HMAC with, so an erased subject's rows stay
    /// linkable to each other and to nobody. Keep it outside the database.
    #[serde(default)]
    pub erasure_key: Option<String>,
}

/// An erasure key shorter than this is refused: the HMAC is the only thing
/// between a pseudonym and the user id it replaced.
pub const MIN_ERASURE_KEY_LEN: usize = 32;

impl AuditConfig {
    /// RFC 0036 §4.3's refusals.
    pub fn validate(&self) -> Result<()> {
        if self.seal_interval_secs > 0 && self.seal_signing_key.is_none() {
            bail!(
                "[audit] seal_interval_secs is set without seal_signing_key: an unsigned \
                 chain proves order, not origin"
            );
        }
        if let Some(key) = &self.seal_signing_key {
            if !valid_ed25519_hex_key(key.trim()) {
                bail!(
                    "[audit] seal_signing_key must be a 32-byte Ed25519 seed, as 64 hex characters"
                );
            }
        }
        if let Some(key) = &self.erasure_key {
            if key.len() < MIN_ERASURE_KEY_LEN {
                bail!("[audit] erasure_key must be at least {MIN_ERASURE_KEY_LEN} characters");
            }
        }
        let (access, security, pseudo) = (
            self.access_retention_days,
            self.security_retention_days,
            self.pseudonymise_after_days,
        );
        if pseudo > 0 && access > 0 && pseudo > access {
            bail!(
                "[audit] pseudonymise_after_days ({pseudo}) is past access_retention_days \
                 ({access}): the rows would be deleted before they are pseudonymised"
            );
        }
        if access > 0 && security > 0 && security < access {
            bail!(
                "[audit] security_retention_days ({security}) is shorter than \
                 access_retention_days ({access}): the evidence would expire before the \
                 traffic it explains"
            );
        }
        Ok(())
    }

    pub fn sealing(&self) -> bool {
        self.seal_interval_secs > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(toml_text: &str) -> AuditConfig {
        toml::from_str(toml_text).unwrap()
    }

    const KEY: &str = "9d61b19deffeba00aa3f3b6e3b0fe6a3f3a76b08e2c0a3f3b6e3b0fe6a3f3a76";

    #[test]
    fn the_documented_example_validates() {
        let c = cfg(&format!(
            "access_retention_days = 365\nsecurity_retention_days = 1095\n\
             pseudonymise_after_days = 30\nseal_interval_secs = 300\nseal_signing_key = \"{KEY}\"\n"
        ));
        c.validate().unwrap();
        assert!(c.sealing());
    }

    #[test]
    fn the_four_refusals() {
        for (bad, why) in [
            ("seal_interval_secs = 300".to_owned(), "seal without key"),
            ("seal_signing_key = \"nope\"".to_owned(), "key not a seed"),
            (
                "access_retention_days = 30\npseudonymise_after_days = 60".to_owned(),
                "pseudo past access",
            ),
            (
                "access_retention_days = 365\nsecurity_retention_days = 30".to_owned(),
                "security short",
            ),
            ("erasure_key = \"short\"".to_owned(), "erasure key short"),
        ] {
            assert!(cfg(&bad).validate().is_err(), "{why} was accepted");
        }
    }

    #[test]
    fn zero_means_never_and_is_not_a_conflict() {
        cfg(
            "access_retention_days = 0\nsecurity_retention_days = 30\npseudonymise_after_days = 90",
        )
        .validate()
        .unwrap();
    }
}
