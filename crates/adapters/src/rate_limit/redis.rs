//! Redis-backed rate-limit counter store (requires the `cache-redis` feature).

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use redis::aio::ConnectionManager;
use redis::AsyncCommands;

use batlehub_core::error::CoreError;
use batlehub_core::ports::RateLimitStore;

/// Rate-limit counter store backed by Redis.
///
/// Each window counter is stored under the key `batlehub:rl:{key}:{window_start}`.
/// The key is given a TTL of `window_secs + 1` on first write so Redis evicts it
/// automatically once the window closes — no periodic cleanup needed.
///
/// Counters survive server restarts and are shared across all instances pointing at
/// the same Redis instance, making this suitable for multi-instance deployments.
///
/// Uses `INCR` for atomic counter increments and a guarded `EXPIRE` (only on first
/// write) to avoid resetting the TTL on subsequent writes.
pub struct RedisRateLimitStore {
    conn: ConnectionManager,
}

impl RedisRateLimitStore {
    /// Connect to Redis at `url` and return a new store.
    ///
    /// The connection manager maintains a single multiplexed connection and
    /// transparently reconnects on failure.
    pub async fn new(url: &str) -> Result<Self, CoreError> {
        let client = redis::Client::open(url)
            .map_err(|e| CoreError::Cache(format!("invalid Redis URL for rate limiter: {e}")))?;
        let conn = ConnectionManager::new(client)
            .await
            .map_err(|e| CoreError::Cache(format!("Redis rate limiter connection failed: {e}")))?;
        Ok(Self { conn })
    }

    fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

#[async_trait]
impl RateLimitStore for RedisRateLimitStore {
    async fn increment(&self, key: &str, window_secs: u32) -> Result<(u64, u64), CoreError> {
        if window_secs == 0 {
            return Err(CoreError::Cache("window_secs must be > 0".into()));
        }
        let now = Self::now_unix();
        let ws = window_secs as u64;
        let window_start = (now / ws) * ws;
        let window_reset = window_start + ws;

        let redis_key = format!("batlehub:rl:{key}:{window_start}");
        let mut conn = self.conn.clone();

        let count: u64 = conn
            .incr(&redis_key, 1u64)
            .await
            .map_err(|e| CoreError::Cache(format!("Redis INCR rate_limit: {e}")))?;

        // Set TTL only on the first write so Redis evicts the key automatically.
        if count == 1 {
            conn.expire::<_, ()>(&redis_key, (window_secs + 1) as i64)
                .await
                .map_err(|e| CoreError::Cache(format!("Redis EXPIRE rate_limit: {e}")))?;
        }

        Ok((count, window_reset))
    }

    async fn peek(&self, key: &str, window_secs: u32) -> Result<(u64, u64), CoreError> {
        if window_secs == 0 {
            return Err(CoreError::Cache("window_secs must be > 0".into()));
        }
        let ws = window_secs as u64;
        let window_start = (Self::now_unix() / ws) * ws;
        let redis_key = format!("batlehub:rl:{key}:{window_start}");
        let mut conn = self.conn.clone();
        let count: Option<u64> = conn
            .get(&redis_key)
            .await
            .map_err(|e| CoreError::Cache(format!("Redis GET rate_limit: {e}")))?;
        Ok((count.unwrap_or(0), window_start + ws))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_unix_is_reasonable() {
        assert!(RedisRateLimitStore::now_unix() > 1_704_067_200);
    }

    #[test]
    fn window_aligns_to_boundary() {
        let now: u64 = 1000;
        let ws: u64 = 60;
        let window_start = (now / ws) * ws;
        assert_eq!(window_start, 960);
        assert_eq!(window_start + ws, 1020);
    }

    #[test]
    fn window_start_is_always_lte_now() {
        let now: u64 = 9999;
        for ws in [1u64, 30, 60, 300, 3600] {
            let window_start = (now / ws) * ws;
            assert!(
                window_start <= now,
                "window_start {window_start} > now {now} for ws={ws}"
            );
            assert!(window_start + ws > now);
        }
    }
}
