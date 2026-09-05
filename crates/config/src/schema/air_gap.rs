//! `[air_gap]` — a server that will not dial out (RFC 0008 §4.1).
//!
//! Absent, or `enabled = false`, is exactly today's behaviour: every proxy
//! registry reaches upstream on a miss. `enabled = true` is a different
//! promise — **no proxy-mode registry attempts an upstream connection at
//! all** — and the point of the section is that the promise is made once, at
//! the top level, rather than per registry where one forgotten entry would
//! quietly break it.
//!
//! ```toml
//! [air_gap]
//! enabled             = true
//! bundle_trusted_keys = ["3b1f…"]   # hex ed25519 public keys accepted on import
//! record_misses       = true
//! miss_retention_days = 90
//! ```
//!
//! What a miss becomes is the other half: a `503` naming the registry and the
//! coordinate, recorded once per `(registry, key)`, so the list of things the
//! next bundle needs is produced by the estate rather than guessed by an
//! operator.

use serde::{Deserialize, Serialize};

use super::registry::default_true;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AirGapConfig {
    /// The whole switch. `false` — the default, and the value in every
    /// existing config — leaves every code path as it is today.
    #[serde(default)]
    pub enabled: bool,
    /// Hex-encoded 32-byte ed25519 public keys whose signature an imported
    /// bundle must carry. Required when `enabled`: an air-gapped instance
    /// whose only content path is unauthenticated is worse than one with no
    /// content path.
    #[serde(default)]
    pub bundle_trusted_keys: Vec<String>,
    /// Record what was asked for and not held. On by default with the mode:
    /// the record is the input to the next bundle.
    #[serde(default = "default_true")]
    pub record_misses: bool,
    /// How long a recorded miss is kept. `0` keeps it until purged by hand.
    #[serde(default = "default_miss_retention_days")]
    pub miss_retention_days: u32,
    /// Answer a listing from what this instance holds when it holds no
    /// document for it (RFC 0008-bis §4.1). Absent means `true` under
    /// `enabled = true`: an instance that has just imported a bundle should
    /// answer `npm install` without a second setting. `false` is RFC 0008's
    /// behaviour — every listing it does not hold is a `503` and a recorded
    /// miss. Read only under `enabled = true`; `true` elsewhere is refused at
    /// load, because it would read as if the instance answered offline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesise_listings: Option<bool>,
}

impl AirGapConfig {
    /// The effective value of `synthesise_listings`: on unless turned off.
    pub fn synthesises_listings(&self) -> bool {
        self.synthesise_listings.unwrap_or(true)
    }
}

impl Default for AirGapConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bundle_trusted_keys: Vec::new(),
            record_misses: true,
            miss_retention_days: default_miss_retention_days(),
            synthesise_listings: None,
        }
    }
}

fn default_miss_retention_days() -> u32 {
    90
}

/// A trusted key is 32 bytes, hex-encoded: 64 characters.
///
/// Checked at load for `[air_gap].bundle_trusted_keys` **and** for every
/// registry's `signing.trusted_keys`, which had no load-time check at all —
/// it was parsed at verify time, so a typo read as "signing is configured"
/// until the first download failed with a `502` nobody could explain.
pub fn valid_ed25519_hex_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listings_are_synthesised_unless_turned_off() {
        let c: AirGapConfig = toml::from_str("enabled = true").unwrap();
        assert_eq!(c.synthesise_listings, None);
        assert!(c.synthesises_listings());
        let c: AirGapConfig =
            toml::from_str("enabled = true\nsynthesise_listings = false").unwrap();
        assert!(!c.synthesises_listings());
    }

    #[test]
    fn absent_is_off_and_recording_is_on_with_the_mode() {
        let c: AirGapConfig = toml::from_str("").expect("every field defaults");
        assert!(!c.enabled);
        assert!(c.record_misses);
        assert_eq!(c.miss_retention_days, 90);
        assert!(c.bundle_trusted_keys.is_empty());
    }

    #[test]
    fn a_key_is_thirty_two_hex_bytes_and_nothing_else() {
        assert!(valid_ed25519_hex_key(&"a".repeat(64)));
        assert!(valid_ed25519_hex_key(&"0123456789ABCDEF".repeat(4)));
        assert!(!valid_ed25519_hex_key(&"a".repeat(63)));
        assert!(!valid_ed25519_hex_key(&"a".repeat(66)));
        assert!(!valid_ed25519_hex_key(""));
        // The shape an operator pastes when they copy the wrong thing.
        assert!(!valid_ed25519_hex_key(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI"
        ));
        assert!(!valid_ed25519_hex_key(&format!("0x{}", "a".repeat(62))));
    }
}
