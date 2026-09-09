//! `[[flag_sources]]` — the third parties that may push vulnerability flags
//! (RFC 0002 §4.3, recast by §13).
//!
//! ```toml
//! [[flag_sources]]
//! name = "soc"
//! secret = "hmac-key-from-your-vault"
//! max_effect = "hard_block"      # inform | warn | gate | hard_block; default gate
//! registries = ["npm", "pypi"]   # empty: any registry
//! max_flags_per_minute = 600     # 0: no limit
//! ```
//!
//! A source pushes to `POST /api/v1/flags/{name}` with the batch signed by
//! `secret` (`X-Hub-Signature-256: sha256=<hex>` over the raw body), the same
//! scheme `[[notifications.inbound]]` verifies. `max_effect` is the ceiling:
//! a push asking for more is stored at the ceiling and told so
//! (`effect_capped`), because a feed that can deny every download of an
//! estate is a decision the operator takes in the config, not one the feed
//! takes per item.

use serde::Deserialize;

use batlehub_core::entities::FlagEffect;
use batlehub_core::services::FlagSourceLimits;

#[derive(Debug, Clone, Deserialize)]
pub struct FlagSourceConfig {
    /// The path segment of the push endpoint: `[a-z0-9][a-z0-9_-]*`.
    pub name: String,
    /// HMAC-SHA256 key. Required and non-empty: the signature is the only
    /// credential the endpoint has.
    pub secret: String,
    /// The strongest effect this source may set. Default `gate`.
    #[serde(default = "default_max_effect")]
    pub max_effect: String,
    /// Registries this source may flag. Empty means every registry.
    #[serde(default)]
    pub registries: Vec<String>,
    /// Items per minute across pushes; over it the push is `429`. Default
    /// 600; `0` disables the limit.
    #[serde(default = "default_rate")]
    pub max_flags_per_minute: u32,
}

fn default_max_effect() -> String {
    "gate".to_owned()
}

fn default_rate() -> u32 {
    600
}

impl FlagSourceConfig {
    pub fn max_effect(&self) -> Option<FlagEffect> {
        FlagEffect::parse(&self.max_effect)
    }

    /// The entry as core reads it. Validation has already refused an
    /// unparsable `max_effect`; a stray one here falls to `inform`, the
    /// weakest effect, never the strongest.
    pub fn limits(&self) -> FlagSourceLimits {
        FlagSourceLimits {
            name: self.name.clone(),
            max_effect: self.max_effect().unwrap_or(FlagEffect::Inform),
            registries: self.registries.clone(),
            max_flags_per_minute: self.max_flags_per_minute,
        }
    }

    pub fn valid_name(name: &str) -> bool {
        let mut chars = name.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
            && name.len() <= 64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_gate_and_six_hundred() {
        let c: FlagSourceConfig = toml::from_str("name = \"soc\"\nsecret = \"k\"").expect("parses");
        assert_eq!(c.max_effect(), Some(FlagEffect::Gate));
        assert_eq!(c.max_flags_per_minute, 600);
        assert!(c.registries.is_empty());
        assert_eq!(c.limits().max_effect, FlagEffect::Gate);
    }

    #[test]
    fn names_are_path_segments() {
        assert!(FlagSourceConfig::valid_name("soc"));
        assert!(FlagSourceConfig::valid_name("vendor-feed_2"));
        assert!(!FlagSourceConfig::valid_name("SOC"));
        assert!(!FlagSourceConfig::valid_name("-soc"));
        assert!(!FlagSourceConfig::valid_name("a/b"));
        assert!(!FlagSourceConfig::valid_name(""));
    }
}
