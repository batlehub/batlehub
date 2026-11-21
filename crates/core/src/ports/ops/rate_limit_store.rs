//! Port definition for the pluggable rate-limit counter store.

use async_trait::async_trait;

use crate::error::CoreError;

/// Pluggable storage backend for fixed-window rate-limit counters.
///
/// Implementations must be `Send + Sync` so the service can be shared across
/// Actix worker threads via `Arc<dyn RateLimitStore>`.
///
/// Three backends are provided in `batlehub-adapters`:
/// - [`InMemoryRateLimitStore`] — single-process, lost on restart
/// - [`PgRateLimitStore`] — persisted in PostgreSQL, shared across instances
/// - [`RedisRateLimitStore`] — persisted in Redis (feature `cache-redis`), shared across instances
#[async_trait]
pub trait RateLimitStore: Send + Sync {
    /// Atomically increment the request counter for `key` within the current fixed time window.
    ///
    /// The window is aligned to the Unix epoch: `window_start = floor(now / window_secs) * window_secs`.
    /// Each unique `(key, window_start)` pair gets its own independent counter.
    ///
    /// Returns `(new_count, window_reset_unix_secs)` where `window_reset_unix_secs` is the Unix
    /// timestamp at which the current window closes and the counter resets to zero.
    ///
    /// # Errors
    /// Returns `CoreError` if the backend is unavailable or `window_secs` is zero.
    ///
    /// # Panics
    /// Implementations must not panic; `window_secs == 0` must return an error.
    async fn increment(&self, key: &str, window_secs: u32) -> Result<(u64, u64), CoreError>;

    /// Read `key`'s counter in the current window **without** incrementing it.
    ///
    /// Returns `(count, window_reset_unix_secs)`: `count` is `0` when nothing
    /// has been counted in this window. Same window alignment as
    /// [`increment`](Self::increment), so the two agree on which window is
    /// current. Used to show a caller their own usage (`GET /api/v1/me/limits`),
    /// which must not itself spend a request.
    ///
    /// # Errors
    /// Returns `CoreError` if the backend is unavailable or `window_secs` is zero.
    async fn peek(&self, key: &str, window_secs: u32) -> Result<(u64, u64), CoreError>;
}
