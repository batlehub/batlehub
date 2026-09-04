//! Forge-specific registry settings (RFC 0019 §4.1).
//!
//! `[registries.refs]` today; `[registries.raw]` and `[registries.api_reads]`
//! arrive with phase 3 and sit beside it here. All three mean something only
//! on a `github`, `gitlab` or `forgejo` registry, and validation refuses them
//! anywhere else — the same class as `index_url` on a non-cargo registry: a
//! silently ignored option is a misconfiguration that looks like a proxy bug.

use serde::{Deserialize, Serialize};

/// `[registries.refs]` — how long a ref → commit resolution is trusted.
///
/// Resolution itself always runs; it is what builds the cache key. These two
/// numbers decide how often the forge is asked again, and so how quickly a
/// moved tag or an advanced branch is noticed — and how many API calls a
/// busy registry spends noticing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefsConfig {
    /// How long a branch → commit resolution is trusted. Default 60 s.
    #[serde(default = "default_branch_ttl_secs")]
    pub branch_ttl_secs: u64,
    /// How long a tag → commit resolution is trusted. Tags can move; default
    /// one hour, which is also the detection latency of a moved tag.
    #[serde(default = "default_tag_ttl_secs")]
    pub tag_ttl_secs: u64,
}

impl Default for RefsConfig {
    fn default() -> Self {
        Self {
            branch_ttl_secs: default_branch_ttl_secs(),
            tag_ttl_secs: default_tag_ttl_secs(),
        }
    }
}

fn default_branch_ttl_secs() -> u64 {
    60
}

fn default_tag_ttl_secs() -> u64 {
    3600
}

/// The floor under `branch_ttl_secs`: below it a busy branch is re-resolved
/// on nearly every request, which is a rate-limit self-DoS (RFC 0019 §4.3).
pub const MIN_BRANCH_TTL_SECS: u64 = 10;
