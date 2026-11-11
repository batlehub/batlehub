use std::sync::Arc;

use actix_web::{get, web, Responder};
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

use batlehub_core::services::{ProxyMetrics, ProxyService};

use crate::{error::AppError, extractors::AuthIdentity, RegistryMap};

/// The registries this caller holds `stats:read` on (RFC 0015 §4.2), or a `403`
/// when that is none of them.
///
/// # Refuse at nothing, filter at some
///
/// `stats:read` is one of the three disclosure verbs §4.2 splits out of
/// `require_admin`, and §7 is explicit that doing so *"is a reduction in
/// privilege for existing admins and must not be applied silently"*. §10 rule 5
/// is what makes that safe: the verb goes to `role:admin` on every registry, so
/// an administrator's dashboard is unchanged on upgrade and the verb becomes
/// **grantable** rather than newly required.
///
/// The boundary is §4.4 rule 2 applied one level up, and it has two sides that
/// are easy to collapse into one:
///
/// - **Held nowhere → `403`.** The caller asserted nothing and is told so. This
///   is what `require_admin` answered before, so an anonymous or ordinary-user
///   request to this endpoint is refused exactly as it is today — which is
///   §10's whole promise, and what the pre-existing tests pin.
/// - **Held somewhere → filter.** *"`stats:read` without any package grants
///   therefore resolves to a dashboard of zeroes rather than a `403` … the
///   caller asked for their own view, and their own view is empty."* An operator
///   who grants it on one registry gets that registry's numbers and nothing for
///   the rest, not a refusal for the whole page.
///
/// Reading the second rule as covering the first would turn an admin-only
/// endpoint into one that answers `200` to anonymous. It would disclose nothing
/// — the filtered result is empty — but a surface that answers everybody is a
/// different surface, and this document does not widen one by accident.
pub(super) async fn registries_with_stats_read(
    identity: &batlehub_core::entities::Identity,
    registries: &[String],
    hot: &batlehub_core::services::hot_config::HotConfigLock,
) -> Result<Vec<String>, AppError> {
    use batlehub_core::entities::{Action, PackageId};

    // A registry with no configured hierarchy is **not** permitted here, which
    // is the opposite of what `authorize_grants` answers for one. That is not an
    // inconsistency: its permissive reading exists because an unknown registry
    // is a routing question the handler answers `404`, and there is no `404` to
    // fall through to inside an aggregate. A number is either included or it is
    // not, so the absent case has to pick a side, and §4.3 says which one.
    let configured: std::collections::HashSet<String> =
        hot.read().await.grants.keys().cloned().collect();

    let mut out = Vec::new();
    for registry in registries {
        if !configured.contains(registry) {
            continue;
        }
        // The registry tier names no package, which is the coordinate a
        // registry-wide verb is asked about.
        let id = PackageId::new(registry, "", "");
        if batlehub_core::services::authz::authorize_grants_public(
            hot,
            &id,
            identity,
            Action::StatsRead,
        )
        .await
        .is_ok()
        {
            out.push(registry.clone());
        }
    }
    if out.is_empty() {
        return Err(AppError::forbidden(
            "reading statistics requires 'stats:read'",
        ));
    }
    Ok(out)
}

fn hit_rate(hits: u64, misses: u64) -> Option<f64> {
    let total = hits + misses;
    (total > 0).then(|| hits as f64 / total as f64)
}

#[derive(Serialize, ToSchema)]
pub struct RegistryStatsDto {
    pub registry: String,
    pub artifact_hits: u64,
    pub artifact_misses: u64,
    /// Artifact hit rate in [0, 1], or null if no requests yet.
    pub hit_rate: Option<f64>,
    /// Total bytes cached in storage for this registry (from storage backend).
    pub cached_bytes: Option<u64>,
    /// `true` when upstream is showing a high error rate or slow responses;
    /// cached data may be stale until it recovers.
    pub upstream_degraded: bool,
    /// Rolling upstream error rate in [0, 1] (exponential moving average).
    pub upstream_error_rate: f64,
    /// Rolling average upstream call latency in milliseconds.
    pub upstream_latency_ms: u64,
}

#[derive(Serialize, ToSchema)]
pub struct AggregateStats {
    pub artifact_hits: u64,
    pub artifact_misses: u64,
    /// Aggregate artifact hit rate in [0, 1], or null if no requests yet.
    pub hit_rate: Option<f64>,
    /// Total bytes cached across all registries.
    pub cached_bytes: u64,
}

#[derive(Serialize, ToSchema)]
pub struct StatsResponse {
    /// When this process started (counters reset on restart).
    pub since_startup: DateTime<Utc>,
    pub aggregate: AggregateStats,
    pub per_registry: Vec<RegistryStatsDto>,
    /// `[stats] dashboard_url`; only with `system:read` (RFC 0044 §4.2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitoring_url: Option<String>,
}

/// Aggregate cache hit/miss statistics since last restart (admin).
#[utoipa::path(
    get,
    path = "/api/v1/admin/stats",
    tag = "back-office",
    responses(
        (status = 200, description = "Cache statistics", body = StatsResponse),
        (status = 403, description = "`stats:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/stats")]
pub async fn admin_stats(
    identity: AuthIdentity,
    registry_map: web::Data<RegistryMap>,
    proxy_svc: web::Data<Arc<ProxyService>>,
    proxy_metrics: web::Data<Arc<ProxyMetrics>>,
    views: Option<web::Data<super::registries::RegistryViews>>,
) -> Result<impl Responder, AppError> {
    let mut per_registry: Vec<RegistryStatsDto> = Vec::new();
    let mut total_hits: u64 = 0;
    let mut total_misses: u64 = 0;
    let mut total_cached_bytes: u64 = 0;

    let mut registries: Vec<String> = registry_map.keys();
    registries.sort();
    // RFC 0015 §4.2 — `stats:read`, in place of `require_admin`. Filtered rather
    // than refused, and the aggregate below is summed over what survives, so a
    // total is never taken over a registry this caller may not read: §4.4 rule 1
    // applied to the one number on this page that is a sum.
    let registries = registries_with_stats_read(&identity, &registries, &proxy_svc.hot).await?;

    for registry in registries {
        let (hits, misses, degraded, error_rate, latency_ms) =
            if let Some(c) = proxy_metrics.all().get(&registry) {
                (
                    c.hits(),
                    c.misses(),
                    c.is_degraded(),
                    c.upstream_error_rate_permille() as f64 / 1000.0,
                    c.upstream_latency_ms(),
                )
            } else {
                (0, 0, false, 0.0, 0)
            };

        let prefix = format!("artifact:{}/", registry);
        let cached_bytes: Option<u64> = match proxy_svc.storage.stat_by_prefix(&prefix).await {
            Ok((_, bytes)) => Some(bytes),
            Err(_) => None,
        };

        total_hits += hits;
        total_misses += misses;
        total_cached_bytes += cached_bytes.unwrap_or(0);

        per_registry.push(RegistryStatsDto {
            registry,
            artifact_hits: hits,
            artifact_misses: misses,
            hit_rate: hit_rate(hits, misses),
            cached_bytes,
            upstream_degraded: degraded,
            upstream_error_rate: error_rate,
            upstream_latency_ms: latency_ms,
        });
    }

    let aggregate_hit_rate = hit_rate(total_hits, total_misses);

    Ok(web::Json(StatsResponse {
        since_startup: proxy_metrics.started_at,
        aggregate: AggregateStats {
            artifact_hits: total_hits,
            artifact_misses: total_misses,
            hit_rate: aggregate_hit_rate,
            cached_bytes: total_cached_bytes,
        },
        per_registry,
        monitoring_url: match views {
            Some(v)
                if batlehub_core::services::authz::authorize_control(
                    &proxy_svc.hot,
                    None,
                    &identity.0,
                    batlehub_core::entities::Action::SystemRead,
                )
                .await
                .is_ok() =>
            {
                v.dashboard_url()
            }
            _ => None,
        },
    }))
}
