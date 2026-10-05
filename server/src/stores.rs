use std::sync::Arc;

use anyhow::{Context, Result};

use batlehub_adapters::cache::InMemoryBannerStore;
#[cfg(feature = "cache-redis")]
use batlehub_adapters::cache::RedisBannerStore;
#[cfg(feature = "cache-redis")]
use batlehub_adapters::cache::RedisCacheStore;
use batlehub_adapters::cache::{InMemoryCacheStore, PgCacheStore};
use batlehub_adapters::db::PgBannerStore;
use batlehub_adapters::db::PgUserBlockRepository;
use batlehub_adapters::local_registry::PostgresLocalRegistry;
use batlehub_adapters::notification::PgNotificationStore;
use batlehub_adapters::rate_limit::{
    InMemoryIpBlockStore, InMemoryRateLimitStore, PgIpBlockStore, PgRateLimitStore,
};
#[cfg(feature = "cache-redis")]
use batlehub_adapters::rate_limit::{RedisIpBlockStore, RedisRateLimitStore};
use batlehub_adapters::sbom::HttpSbomFetcher;
use batlehub_config::schema::AppConfig;
use batlehub_core::ports::LocalRegistryBackend;
use batlehub_core::ports::{
    BannerPort, CacheStore, IpBlockStore, NotificationPort, RateLimitStore, SbomRepository,
    UserBlockRepository,
};
use batlehub_core::services::{QuotaService, SbomService};

pub(super) const DEFAULT_REDIS_URL: &str = "redis://127.0.0.1:6379";

/// Backend selected via `[cache] type = "..."` in config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CacheBackend {
    Memory,
    Postgres,
    Redis,
}

impl std::str::FromStr for CacheBackend {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "memory" => Ok(Self::Memory),
            "postgres" => Ok(Self::Postgres),
            "redis" => Ok(Self::Redis),
            _ => Err(()),
        }
    }
}

/// Parse `config.cache.cache_type`, falling back to `Memory` with a startup
/// warning on an unrecognized value. Used by every `create_*_store` function
/// below so an unparseable `cache_type` produces the same warning and the
/// same fallback everywhere, instead of the 3-way split (2 warn, 3 silent)
/// the raw string match previously produced.
fn cache_backend(config: &AppConfig) -> CacheBackend {
    let raw = config.cache.cache_type.as_str();
    raw.parse().unwrap_or_else(|()| {
        tracing::warn!(cache_type = %raw, "unknown cache type, falling back to memory");
        CacheBackend::Memory
    })
}

/// How far back `prune_expired` reaches on each sweep. Generously larger than
/// any realistic rate-limit window so a stale row is never pruned mid-window.
const RATE_LIMIT_PRUNE_RETENTION_SECS: u64 = 24 * 60 * 60;
const RATE_LIMIT_PRUNE_INTERVAL_SECS: u64 = 60;

/// How long a pending publish row must be older than before it is deleted.
/// Two cleanup intervals — so a row created just before one sweep survives long
/// enough to be promoted before the next sweep can remove it.
const PENDING_CLEANUP_INTERVAL_SECS: u64 = 3600;

/// Periodically delete orphaned `status = 'pending'` rows left by hard crashes.
/// A normal publish either promotes the row to `'published'` or removes it on
/// error; rows that survive are only from processes that died mid-publish. The
/// cleanup threshold is twice the interval so a row created just before a sweep
/// cannot be deleted before it has time to be promoted.
pub(super) fn spawn_pending_publish_cleanup(backend: Arc<PostgresLocalRegistry>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(
            PENDING_CLEANUP_INTERVAL_SECS,
        ));
        loop {
            ticker.tick().await;
            let older_than = std::time::Duration::from_secs(PENDING_CLEANUP_INTERVAL_SECS * 2);
            match backend.cleanup_pending(older_than).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(deleted = n, "cleaned up orphaned pending publish rows"),
                Err(e) => tracing::warn!(error = %e, "pending publish cleanup failed"),
            }
        }
    });
}

/// How often quota usage/limit gauges are re-sampled. Usage changes only on
/// publish/revoke, much less often than pool stats, so a longer interval is fine.
const QUOTA_GAUGE_INTERVAL_SECS: u64 = 60;

/// Periodically sample every (user, registry) quota row into
/// `batlehub_quota_bytes_used`/`batlehub_quota_bytes_limit` gauges.
///
/// Per-user granularity is deliberate — the "Quota Usage (top users)" and "Quota
/// Utilisation %" Grafana panels drill into individual users via `topk(...)`, so
/// the label set must stay per-`(registry, user)`, not collapsed to per-registry.
/// The tradeoff is that the `metrics` facade never removes a label set once
/// created, so a `(registry, user)` pair that drops out of `list_usage` (usage
/// reset, user deleted) is explicitly zeroed on the tick it disappears, rather
/// than left frozen at its last non-zero value forever.
pub(super) fn spawn_quota_gauge_sampler(quota_svc: Arc<QuotaService>) {
    tokio::spawn(async move {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(QUOTA_GAUGE_INTERVAL_SECS));
        let mut previous_keys: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        loop {
            ticker.tick().await;
            let usages = match quota_svc.list_usage(None).await {
                Ok(usages) => usages,
                Err(e) => {
                    tracing::warn!(error = %e, "quota gauge sampler: list_usage failed");
                    continue;
                }
            };

            let current_keys: std::collections::HashSet<(String, String)> = usages
                .iter()
                .map(|u| (u.registry.clone(), u.user_id.clone()))
                .collect();
            for (registry, user_id) in previous_keys.difference(&current_keys) {
                metrics::gauge!(
                    "batlehub_quota_bytes_used",
                    "registry" => registry.clone(),
                    "user" => user_id.clone()
                )
                .set(0.0);
                metrics::gauge!(
                    "batlehub_quota_bytes_limit",
                    "registry" => registry.clone(),
                    "user" => user_id.clone()
                )
                .set(0.0);
            }

            for usage in usages {
                metrics::gauge!(
                    "batlehub_quota_bytes_used",
                    "registry" => usage.registry.clone(),
                    "user" => usage.user_id.clone()
                )
                .set(usage.bytes_published as f64);
                if let Some(limit) = quota_svc.max_storage_bytes(&usage.registry) {
                    metrics::gauge!(
                        "batlehub_quota_bytes_limit",
                        "registry" => usage.registry,
                        "user" => usage.user_id
                    )
                    .set(limit as f64);
                }
            }
            previous_keys = current_keys;
        }
    });
}

/// How often the DB connection pool gauges are re-sampled.
const DB_POOL_GAUGE_INTERVAL_SECS: u64 = 15;

/// Periodically sample the DB connection pool's size and idle-connection count
/// into Prometheus gauges. `sqlx::Pool::size`/`num_idle` are cheap, in-memory
/// reads, so a short interval is fine.
pub(super) fn spawn_db_pool_gauge_sampler(pool: sqlx::PgPool) {
    tokio::spawn(async move {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(DB_POOL_GAUGE_INTERVAL_SECS));
        loop {
            ticker.tick().await;
            metrics::gauge!("batlehub_db_pool_size").set(pool.size() as f64);
            metrics::gauge!("batlehub_db_pool_available_connections").set(pool.num_idle() as f64);
        }
    });
}

/// How often `db::housekeeping::sweep` runs.
const HOUSEKEEPING_INTERVAL_SECS: u64 = 3600;

/// Periodically delete the operational rows nothing reads any more (expired
/// metadata cache entries, finished scan jobs, …) — see
/// `batlehub_adapters::db::housekeeping` for the list and each cutoff.
pub(super) fn spawn_db_housekeeping(pool: sqlx::PgPool) {
    tokio::spawn(async move {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(HOUSEKEEPING_INTERVAL_SECS));
        loop {
            ticker.tick().await;
            match batlehub_adapters::db::housekeeping::sweep(&pool).await {
                Ok(deleted) => {
                    for (table, n) in deleted.into_iter().filter(|(_, n)| *n > 0) {
                        tracing::info!(table, deleted = n, "db housekeeping");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "db housekeeping failed"),
            }
        }
    });
}

/// Periodically delete expired `rate_limit_counters` rows in the background,
/// instead of pruning inline on every `increment()` call. Mirrors the detached
/// `tokio::spawn` + `tokio::time::interval` pattern used by
/// `watcher::spawn_periodic_vuln_scan`.
fn spawn_rate_limit_prune(store: Arc<PgRateLimitStore>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(
            RATE_LIMIT_PRUNE_INTERVAL_SECS,
        ));
        loop {
            ticker.tick().await;
            if let Err(e) = store.prune_expired(RATE_LIMIT_PRUNE_RETENTION_SECS).await {
                tracing::warn!(error = %e, "rate-limit: periodic prune failed");
            }
        }
    });
}

pub(super) async fn create_cache_store(
    config: &AppConfig,
    pool: sqlx::PgPool,
) -> Result<Arc<dyn CacheStore>> {
    let store: Arc<dyn CacheStore> = match cache_backend(config) {
        CacheBackend::Postgres => {
            tracing::info!("metadata cache: postgres");
            Arc::new(PgCacheStore::new(pool))
        }
        CacheBackend::Redis => {
            #[cfg(feature = "cache-redis")]
            {
                let url = config.cache.url.as_deref().unwrap_or(DEFAULT_REDIS_URL);
                tracing::info!(url, "metadata cache: redis");
                Arc::new(
                    RedisCacheStore::new(url)
                        .await
                        .context("connecting to Redis cache")?,
                )
            }
            #[cfg(not(feature = "cache-redis"))]
            {
                tracing::warn!(
                    "compiled without cache-redis feature; falling back to in-memory cache"
                );
                Arc::new(InMemoryCacheStore::new())
            }
        }
        CacheBackend::Memory => {
            tracing::warn!(
                "metadata cache: in-memory — rate-limit, quota, and session state \
                 are NOT shared between replicas; use [cache] type = \"postgres\" or \
                 type = \"redis\" in multi-instance deployments"
            );
            Arc::new(InMemoryCacheStore::new())
        }
    };
    Ok(store)
}

pub(super) async fn create_rate_limit_store(
    config: &AppConfig,
    pool: sqlx::PgPool,
) -> Result<Arc<dyn RateLimitStore>> {
    let store: Arc<dyn RateLimitStore> = match cache_backend(config) {
        CacheBackend::Postgres => {
            tracing::info!("rate limit store: postgres");
            let store = Arc::new(PgRateLimitStore::new(pool));
            spawn_rate_limit_prune(Arc::clone(&store));
            store
        }
        CacheBackend::Redis => {
            #[cfg(feature = "cache-redis")]
            {
                let url = config.cache.url.as_deref().unwrap_or(DEFAULT_REDIS_URL);
                tracing::info!(url, "rate limit store: redis");
                Arc::new(
                    RedisRateLimitStore::new(url)
                        .await
                        .context("connecting to Redis rate limit store")?,
                )
            }
            #[cfg(not(feature = "cache-redis"))]
            {
                tracing::warn!(
                    "compiled without cache-redis feature; falling back to in-memory rate limit store"
                );
                Arc::new(InMemoryRateLimitStore::new())
            }
        }
        CacheBackend::Memory => Arc::new(InMemoryRateLimitStore::new()),
    };
    Ok(store)
}

pub(super) async fn create_ip_block_store(
    config: &AppConfig,
    pool: sqlx::PgPool,
) -> Result<Arc<dyn IpBlockStore>> {
    let store: Arc<dyn IpBlockStore> = match cache_backend(config) {
        CacheBackend::Postgres => {
            tracing::info!("ip block store: postgres");
            Arc::new(PgIpBlockStore::new(pool))
        }
        CacheBackend::Redis => {
            #[cfg(feature = "cache-redis")]
            {
                let url = config.cache.url.as_deref().unwrap_or(DEFAULT_REDIS_URL);
                tracing::info!(url, "ip block store: redis");
                Arc::new(
                    RedisIpBlockStore::new(url)
                        .await
                        .context("connecting to Redis ip block store")?,
                )
            }
            #[cfg(not(feature = "cache-redis"))]
            {
                tracing::warn!(
                    "compiled without cache-redis feature; falling back to in-memory ip block store"
                );
                Arc::new(InMemoryIpBlockStore::new())
            }
        }
        CacheBackend::Memory => Arc::new(InMemoryIpBlockStore::new()),
    };
    Ok(store)
}

/// User blocks always use Postgres when available (in-memory in dev/test without a DB).
pub(super) fn create_user_block_repository(pool: sqlx::PgPool) -> Arc<dyn UserBlockRepository> {
    tracing::info!("user block repository: postgres");
    Arc::new(PgUserBlockRepository::new(pool))
}

pub(super) async fn create_banner_store(
    config: &AppConfig,
    pool: sqlx::PgPool,
) -> Result<Arc<dyn BannerPort>> {
    let store: Arc<dyn BannerPort> = match cache_backend(config) {
        CacheBackend::Postgres => Arc::new(PgBannerStore::new(pool)),
        CacheBackend::Redis => {
            #[cfg(feature = "cache-redis")]
            {
                let url = config.cache.url.as_deref().unwrap_or(DEFAULT_REDIS_URL);
                Arc::new(
                    RedisBannerStore::new(url)
                        .await
                        .context("connecting to Redis banner store")?,
                )
            }
            #[cfg(not(feature = "cache-redis"))]
            Arc::new(InMemoryBannerStore::new())
        }
        CacheBackend::Memory => Arc::new(InMemoryBannerStore::new()),
    };
    Ok(store)
}

pub(super) async fn create_warm_coordinator(
    config: &AppConfig,
) -> Result<Arc<dyn batlehub_core::ports::WarmCoordinator>> {
    use batlehub_core::ports::NoopWarmCoordinator;

    let coordinator: Arc<dyn batlehub_core::ports::WarmCoordinator> = match cache_backend(config) {
        CacheBackend::Redis => {
            #[cfg(feature = "cache-redis")]
            {
                let url = config.cache.url.as_deref().unwrap_or(DEFAULT_REDIS_URL);
                tracing::info!(url, "warm-up coordinator: redis");
                Arc::new(
                    batlehub_adapters::cache::RedisWarmCoordinator::new(url)
                        .await
                        .context("connecting to Redis warm coordinator")?,
                )
            }
            #[cfg(not(feature = "cache-redis"))]
            {
                tracing::warn!(
                    "compiled without cache-redis feature; warm-up coordination disabled"
                );
                Arc::new(NoopWarmCoordinator)
            }
        }
        CacheBackend::Postgres | CacheBackend::Memory => Arc::new(NoopWarmCoordinator),
    };
    Ok(coordinator)
}

pub(super) fn create_notification_store(pool: sqlx::PgPool) -> Arc<dyn NotificationPort> {
    Arc::new(PgNotificationStore::new(pool))
}

pub(super) fn build_notification_service(
    notification_store: Arc<dyn NotificationPort>,
    notifications_config: &Option<batlehub_config::schema::NotificationsConfig>,
) -> Option<Arc<batlehub_web::services::NotificationService>> {
    let explicitly_disabled = notifications_config
        .as_ref()
        .map(|nc| !nc.enabled)
        .unwrap_or(false);
    if explicitly_disabled {
        return None;
    }
    let effective = notifications_config.as_ref().cloned().unwrap_or_default();
    Some(Arc::new(batlehub_web::services::NotificationService::new(
        notification_store,
        &effective,
    )))
}

pub(super) fn build_sbom_service(pool: sqlx::PgPool) -> Result<Arc<SbomService>> {
    use batlehub_adapters::db::PgSbomRepository;

    let sbom_repo: Arc<dyn SbomRepository> = Arc::new(PgSbomRepository::new(pool));
    #[cfg(feature = "sbom")]
    let sbom_extractor: Option<Arc<dyn batlehub_core::ports::SbomExtractor>> =
        Some(Arc::new(batlehub_adapters::sbom::ArchiveSbomExtractor));
    #[cfg(not(feature = "sbom"))]
    let sbom_extractor: Option<Arc<dyn batlehub_core::ports::SbomExtractor>> = None;

    let sbom_http = reqwest::Client::builder()
        .user_agent("batlehub/sbom")
        .build()
        .context("building SBOM HTTP client")?;
    Ok(Arc::new(SbomService::new(
        sbom_repo,
        sbom_extractor,
        Some(Arc::new(HttpSbomFetcher::new(sbom_http))),
    )))
}

#[cfg(test)]
mod tests {
    use super::CacheBackend;

    #[test]
    fn cache_backend_parses_known_values() {
        assert_eq!("memory".parse(), Ok(CacheBackend::Memory));
        assert_eq!("postgres".parse(), Ok(CacheBackend::Postgres));
        assert_eq!("redis".parse(), Ok(CacheBackend::Redis));
    }

    #[test]
    fn cache_backend_rejects_unknown_values() {
        assert_eq!("".parse::<CacheBackend>(), Err(()));
        assert_eq!("Memory".parse::<CacheBackend>(), Err(()));
        assert_eq!("postgresql".parse::<CacheBackend>(), Err(()));
    }
}
