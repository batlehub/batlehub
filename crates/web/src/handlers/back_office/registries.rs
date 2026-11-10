//! One registry's configuration and health, on one page (RFC 0044 §5.3).
//!
//! The configuration half is **projected**, field by field, from
//! `RegistryConfig` when a config is loaded or a reload is staged: a field added
//! to `RegistryConfig` tomorrow, secret or not, does not appear until someone
//! writes it into [`project`]. Credentials reach the DTO only as
//! [`AuthPresence`]. The live half (upstream health, the queue, the SBOM pass)
//! is joined on each request and never frozen at reload time.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use actix_web::{get, web, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

use batlehub_config::schema::{RegistryConfig, UpstreamAuthConfig};
use batlehub_config::AppConfig;
use batlehub_core::entities::{Action, ScanJobFilter};
use batlehub_core::services::authz;
use batlehub_core::services::metrics::ProxyMetrics;
use batlehub_core::services::scan_status::{suggestions, Suggestion};
use batlehub_core::services::ProxyService;

use crate::{error::AppError, extractors::AuthIdentity};

/// Whether an upstream credential is configured, and of which kind — never
/// its value, its length or a hash of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AuthPresence {
    None,
    Configured {
        /// `bearer`, `basic` or `header`.
        kind: String,
    },
}

/// `[registries.cache]`, the keys the page shows.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CacheView {
    pub metadata_ttl_secs: u64,
    pub serve_stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_ttl_secs: Option<u64>,
}

/// `[registries.security]`, the keys the page shows.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SecurityView {
    /// `block` or `warn`.
    pub mode: String,
    pub scanners: Vec<String>,
    pub max_severity: String,
    /// Absent: versions are never rescanned on a clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rescan_interval_secs: Option<u64>,
}

/// The registry's configuration, allow-listed. URL fields are redacted, and
/// served only with `upstreams:read`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RegistryConfigView {
    pub upstream_count: usize,
    /// Redacted; absent without `upstreams:read`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstreams: Option<Vec<String>>,
    /// `index_url`, `broker_url` and `search_url` where set, redacted; absent
    /// without `upstreams:read`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other_urls: Option<Vec<String>>,
    pub upstream_auth: AuthPresence,
    pub cache: CacheView,
    /// The rule kinds, in chain order.
    pub rules: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<SecurityView>,
    /// Subjects named in `[registries.grants]`.
    pub grant_subjects: usize,
    pub namespaces: usize,
}

/// What a load or a staged reload projects for one registry.
#[derive(Debug, Clone)]
struct Projected {
    registry_type: String,
    mode: String,
    config: RegistryConfigView,
    monitoring_url: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RegistryViewsSnapshot {
    registries: HashMap<String, Projected>,
    dashboard_url: Option<String>,
}

/// The projected snapshot, replaced whole on reload so a request never reads
/// half of one — the shape of `ConfigWarnings`.
#[derive(Clone, Default)]
pub struct RegistryViews(Arc<RwLock<Arc<RegistryViewsSnapshot>>>);

impl RegistryViews {
    pub fn from_config(config: &AppConfig) -> Self {
        let views = Self::default();
        views.replace(project(config));
        views
    }

    pub fn replace(&self, snapshot: RegistryViewsSnapshot) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(snapshot);
    }

    fn get(&self) -> Arc<RegistryViewsSnapshot> {
        Arc::clone(&self.0.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// `[stats] dashboard_url`, for `/admin/stats`.
    pub fn dashboard_url(&self) -> Option<String> {
        self.get().dashboard_url.clone()
    }
}

fn redact(url: &str) -> String {
    batlehub_adapters::registry::http_client::redact_text_urls(url)
}

fn project_one(r: &RegistryConfig, template: Option<&str>) -> Projected {
    let security = r.security.as_ref().map(|s| SecurityView {
        mode: match s.mode {
            batlehub_core::entities::SecurityMode::Block => "block",
            batlehub_core::entities::SecurityMode::Warn => "warn",
        }
        .to_owned(),
        scanners: s.scanners.clone(),
        max_severity: s.max_severity.clone(),
        rescan_interval_secs: s
            .rescan
            .as_ref()
            .map(|x| x.interval_secs)
            .filter(|i| *i > 0),
    });
    Projected {
        registry_type: r.registry_type.clone(),
        mode: r.mode.as_str().to_owned(),
        config: RegistryConfigView {
            upstream_count: r.upstreams.len(),
            upstreams: Some(r.upstreams.iter().map(|u| redact(u)).collect()),
            other_urls: Some(
                [&r.index_url, &r.broker_url, &r.search_url]
                    .into_iter()
                    .flatten()
                    .map(|u| redact(u))
                    .collect(),
            ),
            upstream_auth: match &r.upstream_auth {
                None => AuthPresence::None,
                Some(a) => AuthPresence::Configured {
                    kind: match a {
                        UpstreamAuthConfig::Bearer(_) => "bearer",
                        UpstreamAuthConfig::Basic(_) => "basic",
                        UpstreamAuthConfig::Header(_) => "header",
                    }
                    .to_owned(),
                },
            },
            cache: CacheView {
                metadata_ttl_secs: r.cache.metadata_ttl_secs,
                serve_stale: r.cache.serve_stale,
                artifact_ttl_secs: r.cache.artifact_ttl_secs,
            },
            rules: r
                .rules
                .iter()
                .filter_map(|rule| {
                    serde_json::to_value(rule)
                        .ok()
                        .and_then(|v| v.get("kind")?.as_str().map(str::to_owned))
                })
                .collect(),
            security,
            grant_subjects: r.grants.as_ref().map_or(0, HashMap::len),
            namespaces: r.namespaces.len(),
        },
        monitoring_url: template.map(|t| batlehub_config::schema::monitoring_url_for(t, &r.name)),
    }
}

/// The configuration half of every registry's view, from a validated config.
pub fn project(config: &AppConfig) -> RegistryViewsSnapshot {
    let template = config.stats.registry_dashboard_url.as_deref();
    RegistryViewsSnapshot {
        registries: config
            .registries
            .iter()
            .map(|r| (r.name.clone(), project_one(r, template)))
            .collect(),
        dashboard_url: config.stats.dashboard_url.clone(),
    }
}

/// Upstream health, from this replica's counters since it started.
#[derive(Debug, Serialize, ToSchema)]
pub struct LiveView {
    /// Error rate or latency over threshold.
    pub upstream_degraded: bool,
    /// Rolling, in [0, 1].
    pub upstream_error_rate: f64,
    pub upstream_latency_ms: u64,
    /// When this replica's counters started.
    pub since: DateTime<Utc>,
}

/// The registry-wide SBOM re-check.
#[derive(Debug, Serialize, ToSchema)]
pub struct SbomRecheckView {
    pub last_scan_at: DateTime<Utc>,
    pub artifacts_scanned: u64,
    pub findings: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_at: Option<DateTime<Utc>>,
}

/// Scanning on this registry. Counts are absent without `quarantine:read`
/// here: a count of held versions is a hold disclosure (T16's reasoning).
#[derive(Debug, Serialize, ToSchema)]
pub struct ScanView {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanning: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<u64>,
    /// Absent when `[vulnerability_scan]` is off or never covered this registry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sbom_recheck: Option<SbomRecheckView>,
    /// Whether `[vulnerability_scan]` is enabled at all.
    pub sbom_recheck_enabled: bool,
}

/// `GET /api/v1/admin/registries/{name}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct RegistryView {
    pub name: String,
    pub registry_type: String,
    pub mode: String,
    /// `[stats] registry_dashboard_url` for this registry, encoded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitoring_url: Option<String>,
    pub live: LiveView,
    pub scan: ScanView,
    pub suggestions: Vec<Suggestion>,
    /// Only with `config:read`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<RegistryConfigView>,
}

/// One registry's configuration and health (RFC 0044 §4.4). `system:read`
/// first, so an unknown name never answers an unauthorised caller; then each
/// block behind its own verb.
#[utoipa::path(
    get,
    path = "/api/v1/admin/registries/{name}",
    tag = "back-office",
    params(("name" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "The registry view", body = RegistryView),
        (status = 403, description = "`system:read` required"),
        (status = 404, description = "No registry with that name"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/registries/{name}")]
#[allow(clippy::too_many_arguments)]
pub async fn get_registry_view(
    name: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    views: web::Data<RegistryViews>,
    metrics: web::Data<Arc<ProxyMetrics>>,
    exposure: Option<web::Data<super::exposure::ExposureConfig>>,
    advisories: Option<web::Data<Arc<dyn batlehub_core::ports::AdvisoryRepository>>>,
) -> Result<HttpResponse, AppError> {
    super::require_verb(&identity, Action::SystemRead, None, &svc.hot).await?;
    let name = name.into_inner();
    let snapshot = views.get();
    let Some(p) = snapshot.registries.get(&name) else {
        return Err(AppError::not_found(format!("no registry named '{name}'")));
    };
    let may = |a| authz::authorize_control(&svc.hot, Some(&name), &identity.0, a);
    let config_read = may(Action::ConfigRead).await.is_ok();
    let upstreams_read = may(Action::UpstreamsRead).await.is_ok();
    let quarantine_read = may(Action::QuarantineRead).await.is_ok();

    let live = match metrics.all().get(&name) {
        Some(c) => LiveView {
            upstream_degraded: c.is_degraded(),
            upstream_error_rate: c.upstream_error_rate_permille() as f64 / 1000.0,
            upstream_latency_ms: c.upstream_latency_ms(),
            since: metrics.started_at,
        },
        None => LiveView {
            upstream_degraded: false,
            upstream_error_rate: 0.0,
            upstream_latency_ms: 0,
            since: metrics.started_at,
        },
    };

    let (security_on, queue) = {
        let hot = svc.hot.read().await;
        (hot.security.contains_key(&name), hot.scan_queue.clone())
    };
    // One row is enough to read the total `list_jobs` counts over the filter.
    let count = |filter| {
        let queue = queue.clone();
        let regs = vec![name.clone()];
        async move {
            match queue {
                Some(q) if security_on => q
                    .list_jobs(&regs, filter, 1, 0)
                    .await
                    .map(|(_, total)| Some(total))
                    .inspect_err(|e| tracing::warn!(error = %e, "registry view: queue unreadable"))
                    .unwrap_or(None),
                _ => None,
            }
        }
    };
    let (queued, scanning, failed) = if quarantine_read {
        (
            count(ScanJobFilter::Queued).await,
            count(ScanJobFilter::Scanning).await,
            count(ScanJobFilter::Failed).await,
        )
    } else {
        (None, None, None)
    };

    let interval = exposure.as_ref().and_then(|e| e.sbom_recheck_interval);
    let sbom_recheck = match (&advisories, interval) {
        (Some(repo), Some(i)) => repo
            .list_scan_state()
            .await
            .inspect_err(|e| tracing::warn!(error = %e, "registry view: scan state unreadable"))
            .ok()
            .and_then(|rows| rows.into_iter().find(|r| r.registry == name))
            .map(|r| SbomRecheckView {
                next_at: chrono::Duration::from_std(i)
                    .ok()
                    .and_then(|d| r.last_scan_at.checked_add_signed(d)),
                last_scan_at: r.last_scan_at,
                artifacts_scanned: r.artifacts_scanned,
                findings: r.findings,
            }),
        _ => None,
    };

    let rescan = p
        .config
        .security
        .as_ref()
        .and_then(|s| s.rescan_interval_secs)
        .map(std::time::Duration::from_secs);
    let suggestions = suggestions(
        live.upstream_degraded,
        p.config.cache.serve_stale,
        security_on,
        rescan,
        failed.unwrap_or(0),
    );

    let config = config_read.then(|| {
        let mut c = p.config.clone();
        if !upstreams_read {
            c.upstreams = None;
            c.other_urls = None;
        }
        c
    });
    Ok(HttpResponse::Ok().json(RegistryView {
        name,
        registry_type: p.registry_type.clone(),
        mode: p.mode.clone(),
        monitoring_url: p.monitoring_url.clone(),
        live,
        scan: ScanView {
            enabled: security_on,
            queued,
            scanning,
            failed,
            sbom_recheck,
            sbom_recheck_enabled: interval.is_some(),
        },
        suggestions,
        config,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENTINEL: &str = "SENTINEL-0042";

    /// T4 and T4b: every secret-bearing type is set to a sentinel and the
    /// projection never carries it; upstream URLs are redacted.
    #[test]
    fn registry_view_never_contains_sentinel() {
        let raw = format!(
            r#"
            [database]
            type = "postgresql"
            url = "postgresql://localhost/test"

            [storage]
            type = "filesystem"
            path = "/tmp/batlehub-test"

            [server]
            host = "127.0.0.1"
            port = 8080

            [stats]
            registry_dashboard_url = "https://grafana.test/d/x?var-registry={{registry}}"

            [[registries]]
            type = "npm"
            name = "npm-q"
            mode = "proxy"
            upstreams = ["https://u:{SENTINEL}@up.test/?token={SENTINEL}"]
            index_url = "https://up.test/index?access_token={SENTINEL}"

            [registries.upstream_auth]
            type = "bearer"
            token = "{SENTINEL}"

            [registries.cache]
            serve_stale = false

            [registries.security]
            mode = "block"
            scanners = ["osv"]

            [[registries]]
            type = "npm"
            name = "npm-b"
            mode = "proxy"
            upstreams = ["https://registry.npmjs.org"]

            [registries.upstream_auth]
            type = "basic"
            username = "{SENTINEL}"
            password = "{SENTINEL}"

            [[registries]]
            type = "npm"
            name = "npm-h"
            mode = "proxy"
            upstreams = ["https://registry.npmjs.org"]

            [registries.upstream_auth]
            type = "header"
            name = "X-Api-Key"
            value = "{SENTINEL}"
            "#
        );
        let config: AppConfig = toml::from_str(&raw).expect("parses");
        let snap = project(&config);
        let all = format!("{snap:?}");
        assert!(!all.contains(SENTINEL), "{all}");

        let q = &snap.registries["npm-q"];
        assert_eq!(
            q.config.upstream_auth,
            AuthPresence::Configured {
                kind: "bearer".into()
            }
        );
        assert_eq!(
            snap.registries["npm-b"].config.upstream_auth,
            AuthPresence::Configured {
                kind: "basic".into()
            }
        );
        let up = &q.config.upstreams.as_ref().unwrap()[0];
        assert!(up.starts_with("https://up.test/"), "{up}");
        assert_eq!(
            q.monitoring_url.as_deref(),
            Some("https://grafana.test/d/x?var-registry=npm-q")
        );
        assert_eq!(
            q.config.security.as_ref().unwrap().rescan_interval_secs,
            None
        );
        assert!(!q.config.cache.serve_stale);
    }
}
