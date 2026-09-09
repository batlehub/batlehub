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
    /// What following a branch does: `"warn"` (default) or `"deny"`. A
    /// registry that must be reproducible sets `deny` and every mutable
    /// coordinate is refused.
    #[serde(default = "default_mutable_refs")]
    pub mutable_refs: String,
    /// What a tag that has moved — and a release asset whose digest changed —
    /// does: `"deny"` (default) or `"warn"`.
    #[serde(default = "default_tag_moved")]
    pub tag_moved: String,
}

impl Default for RefsConfig {
    fn default() -> Self {
        Self {
            branch_ttl_secs: default_branch_ttl_secs(),
            tag_ttl_secs: default_tag_ttl_secs(),
            mutable_refs: default_mutable_refs(),
            tag_moved: default_tag_moved(),
        }
    }
}

fn default_branch_ttl_secs() -> u64 {
    60
}

fn default_tag_ttl_secs() -> u64 {
    3600
}

fn default_mutable_refs() -> String {
    "warn".to_owned()
}

fn default_tag_moved() -> String {
    "deny".to_owned()
}

impl RefsConfig {
    /// The two actions as core reads them; `None` when a value does not
    /// parse, which validation refuses at load.
    pub fn actions(
        &self,
    ) -> Option<(
        batlehub_core::entities::RefAction,
        batlehub_core::entities::RefAction,
    )> {
        Some((
            batlehub_core::entities::RefAction::parse(&self.mutable_refs)?,
            batlehub_core::entities::RefAction::parse(&self.tag_moved)?,
        ))
    }
}

/// The floor under `branch_ttl_secs`: below it a busy branch is re-resolved
/// on nearly every request, which is a rate-limit self-DoS (RFC 0019 §4.3).
pub const MIN_BRANCH_TTL_SECS: u64 = 10;

// ── `[registries.raw]` (RFC 0019 §4.1, phase 3) ───────────────────────────────

/// Raw file serving, **off unless written**.
///
/// Raw was implicitly on for all three forges before this section existed:
/// `raw.githubusercontent.com`, Forgejo's `raw/`, GitLab's `/-/raw/`. Turning
/// it off by default is the behaviour change RFC 0019 §9 states, and the
/// first refused request answers with a body that says exactly this.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Ceiling on one raw file. Must be set when `enabled`, and must not
    /// exceed `[limits].max_artifact_size_bytes` — a number the global limit
    /// would silently win over is a lie.
    #[serde(default = "default_raw_max_size")]
    pub max_size_bytes: u64,
    /// `owner/repo` globs (`cli/*`). Empty allows any repository.
    #[serde(default)]
    pub repos: Vec<String>,
    /// Refuse a branch ref for raw content.
    #[serde(default)]
    pub require_pinned: bool,
    /// `"warn"` | `"deny"` | `"ignore"` for shell, PowerShell, Python and
    /// batch payloads. **Absent means `deny` on a registry with
    /// `[registries.security]`** and `warn` on any other (RFC 0019 §11 q2,
    /// decided 2026-09-04): opting into a quarantine is opting into "nothing
    /// unscanned is served", and a script passed through is the one artifact
    /// no scanner here reads.
    #[serde(default)]
    pub scripts: Option<String>,
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_size_bytes: default_raw_max_size(),
            repos: Vec::new(),
            require_pinned: false,
            scripts: None,
        }
    }
}

fn default_raw_max_size() -> u64 {
    10 * 1024 * 1024
}

impl RawConfig {
    /// The action for script payloads, given whether this registry has a
    /// `[registries.security]` profile.
    pub fn scripts_action(
        &self,
        has_security: bool,
    ) -> Option<batlehub_core::entities::ScriptAction> {
        match &self.scripts {
            Some(s) => batlehub_core::entities::ScriptAction::parse(s),
            None if has_security => Some(batlehub_core::entities::ScriptAction::Deny),
            None => Some(batlehub_core::entities::ScriptAction::Warn),
        }
    }

    /// The policy as the read path reads it.
    pub fn policy(&self, has_security: bool) -> batlehub_core::entities::RawPolicy {
        batlehub_core::entities::RawPolicy {
            enabled: self.enabled,
            max_size_bytes: self.max_size_bytes,
            repos: self.repos.clone(),
            require_pinned: self.require_pinned,
            scripts: self
                .scripts_action(has_security)
                .unwrap_or(batlehub_core::entities::ScriptAction::Warn),
        }
    }
}

/// `[registries.api_reads]` — the typed read-only JSON routes to add beside
/// the release listing (RFC 0019 §4.1).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiReadsConfig {
    /// `tags`, `commits`, `branches`. Nothing else is accepted: `contents`
    /// and `git/blobs` are raw by another door, and an unknown family would
    /// proxy writes.
    #[serde(default)]
    pub families: Vec<String>,
}

impl ApiReadsConfig {
    pub fn parsed(&self) -> Option<Vec<batlehub_core::entities::ApiReadFamily>> {
        self.families
            .iter()
            .map(|f| batlehub_core::entities::ApiReadFamily::parse(f))
            .collect()
    }
}

/// A `repos` entry is `owner/repo`, each half a literal or `*`.
pub fn valid_repo_glob(pattern: &str) -> bool {
    let Some((owner, repo)) = pattern.split_once('/') else {
        return false;
    };
    let seg_ok = |s: &str| {
        !s.is_empty()
            && (s == "*"
                || s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    };
    seg_ok(owner) && seg_ok(repo) && !pattern.contains("..")
}
