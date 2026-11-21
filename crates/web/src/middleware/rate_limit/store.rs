use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use batlehub_config::schema::{RateLimitConfig, RateLimitEnforcement};
use batlehub_core::ports::RateLimitStore;

// ── Public service ────────────────────────────────────────────────────────────

/// Distributed rate limiter backed by a pluggable `RateLimitStore`.
///
/// Holds per-registry configuration (limits, windows, group overrides).
/// State is persisted in the configured store (InMemory / Postgres / Redis),
/// so limits survive restarts and are shared across multiple instances when
/// using a shared backend.
pub struct RateLimitService {
    configs: HashMap<String, RateLimitConfig>,
    store: Arc<dyn RateLimitStore>,
}

impl RateLimitService {
    pub fn new(configs: &HashMap<String, RateLimitConfig>, store: Arc<dyn RateLimitStore>) -> Self {
        Self {
            configs: configs.clone(),
            store,
        }
    }

    /// Check and consume one request token for the given user and groups in `registry`.
    ///
    /// Returns:
    /// - `None` — no rate limit configured for this registry
    /// - `Some(Ok(limit))` — request allowed; `limit` is the binding ceiling
    /// - `Some(Err((wait, limit, enforcement, reset_unix)))` — rate limited by the most-restrictive check;
    ///   `reset_unix` is the exact Unix timestamp when the violating window resets
    ///
    /// **Multi-limiter semantics:** the user bucket AND all applicable group buckets are checked.
    /// All relevant buckets are incremented; the request is blocked if any bucket exceeds its limit.
    /// On failure the `Err` contains the worst violation (Block beats Warn; longer wait wins).
    ///
    /// **Fail-open design:** if the backing store returns an error, the affected bucket is skipped
    /// and the request is allowed rather than refused. This prevents the rate-limit store becoming
    /// a hard dependency that takes down the proxy when unavailable.
    pub async fn check(
        &self,
        registry: &str,
        user_key: &str,
        user_groups: &[String],
    ) -> Option<Result<u32, (Duration, u32, RateLimitEnforcement, u64)>> {
        let cfg = self.configs.get(registry)?;

        let user_store_key = format!("rl:{registry}:user:{user_key}");
        // Fail-open: if the store is unavailable, skip rate limiting rather than blocking the request.
        let (user_count, user_reset) =
            match self.store.increment(&user_store_key, cfg.window_secs).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        registry = %registry,
                        "rate-limit store unavailable for user bucket; failing open"
                    );
                    return None;
                }
            };

        let mut binding_limit = cfg.requests_per_window;
        let mut worst: Option<(Duration, u32, RateLimitEnforcement, u64)> = None;

        if user_count > cfg.requests_per_window as u64 {
            let wait = wait_from_reset(user_reset);
            worst = Some((
                wait,
                cfg.requests_per_window,
                cfg.enforcement.clone(),
                user_reset,
            ));
        }

        for group in user_groups {
            let Some(grp) = cfg.groups.iter().find(|g| &g.name == group) else {
                continue;
            };
            binding_limit = binding_limit.min(grp.requests_per_window);

            let group_store_key = format!("rl:{registry}:group:{group}");
            // Fail-open: skip this group bucket on store error.
            let (grp_count, grp_reset) = match self
                .store
                .increment(&group_store_key, grp.window_secs)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        registry = %registry,
                        group = %group,
                        "rate-limit store unavailable for group bucket; failing open"
                    );
                    continue;
                }
            };

            if grp_count > grp.requests_per_window as u64 {
                let effective = grp
                    .enforcement
                    .clone()
                    .unwrap_or_else(|| cfg.enforcement.clone());
                let wait = wait_from_reset(grp_reset);
                worst = Some(merge_failure(
                    worst,
                    (wait, grp.requests_per_window, effective, grp_reset),
                ));
            }
        }

        match worst {
            None => Some(Ok(binding_limit)),
            Some(failure) => Some(Err(failure)),
        }
    }
}

/// Who a counter belongs to: the caller alone, or a group they are in (a group
/// bucket is shared by every member, so its count is not the caller's alone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitScope {
    User,
    Group(String),
}

/// One counter as it stands in the current window, read without spending a request.
#[derive(Debug, Clone)]
pub struct RateLimitUsage {
    pub scope: RateLimitScope,
    pub used: u64,
    pub limit: u32,
    pub window_secs: u32,
    pub reset_unix: u64,
    pub enforcement: RateLimitEnforcement,
}

impl RateLimitService {
    /// The caller's counters in `registry`, read-only: the user bucket, then one
    /// per group of theirs that the registry gives its own ceiling — the same
    /// buckets, keyed the same way, that [`check`](Self::check) increments.
    ///
    /// `None` when the registry has no rate limit. Unlike `check`, a store error
    /// is returned rather than failed open: a usage figure the store could not
    /// read is unknown, and reporting it as zero would say the opposite.
    pub async fn usage(
        &self,
        registry: &str,
        user_key: &str,
        user_groups: &[String],
    ) -> Option<Result<Vec<RateLimitUsage>, batlehub_core::error::CoreError>> {
        let cfg = self.configs.get(registry)?;
        let mut buckets = vec![(
            RateLimitScope::User,
            format!("rl:{registry}:user:{user_key}"),
            cfg.requests_per_window,
            cfg.window_secs,
            cfg.enforcement.clone(),
        )];
        for group in user_groups {
            if let Some(grp) = cfg.groups.iter().find(|g| &g.name == group) {
                buckets.push((
                    RateLimitScope::Group(group.clone()),
                    format!("rl:{registry}:group:{group}"),
                    grp.requests_per_window,
                    grp.window_secs,
                    grp.enforcement
                        .clone()
                        .unwrap_or_else(|| cfg.enforcement.clone()),
                ));
            }
        }
        let mut out = Vec::with_capacity(buckets.len());
        for (scope, key, limit, window_secs, enforcement) in buckets {
            match self.store.peek(&key, window_secs).await {
                Ok((used, reset_unix)) => out.push(RateLimitUsage {
                    scope,
                    used,
                    limit,
                    window_secs,
                    reset_unix,
                    enforcement,
                }),
                Err(e) => return Some(Err(e)),
            }
        }
        Some(Ok(out))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Compute the wait duration from a window-reset Unix timestamp.
pub(super) fn wait_from_reset(reset_unix: u64) -> Duration {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Duration::from_secs(reset_unix.saturating_sub(now).max(1))
}

/// Return the "worse" of two rate-limit failures.
///
/// `Block` enforcement beats `Warn`; among equal enforcement modes, the longer
/// wait time wins (the client must wait longer before retrying).
pub(super) fn merge_failure(
    a: Option<(Duration, u32, RateLimitEnforcement, u64)>,
    b: (Duration, u32, RateLimitEnforcement, u64),
) -> (Duration, u32, RateLimitEnforcement, u64) {
    let Some(a) = a else { return b };
    let a_blocks = matches!(a.2, RateLimitEnforcement::Block);
    let b_blocks = matches!(b.2, RateLimitEnforcement::Block);
    match (a_blocks, b_blocks) {
        (true, false) => a,
        (false, true) => b,
        _ => {
            if b.0 > a.0 {
                b
            } else {
                a
            }
        }
    }
}
