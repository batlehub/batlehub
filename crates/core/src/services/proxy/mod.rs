mod cache;
mod discovery;
pub(crate) mod handle;
mod passthrough;
mod resolve;

pub use discovery::DiscoveryOutcome;
pub use passthrough::{FetchOutcome, Freshness, Passthrough, UpstreamBytes};

use std::sync::Arc;
use std::time::Instant;

use futures::{stream, StreamExt};

use crate::entities::AccessEvent;
use crate::entities::Action;
use crate::error::CoreError;
use crate::ports::{
    ArtifactCacheMeta, ArtifactStream, CacheStore, FetchedArtifact, PackageRepository,
    RegistryClient, StorageBackend,
};
use crate::services::hot_config::HotConfigLock;
use crate::services::metrics::ProxyMetrics;
use crate::services::readme::ReadmeService;
use crate::services::sbom::SbomService;

/// Input to `ProxyService::handle`.
pub struct ProxyRequest {
    pub package_id: crate::entities::PackageId,
    pub identity: crate::entities::Identity,
    /// The operation being checked against RBAC (e.g. `"releases:read"`).
    pub action: Action,
    /// Caller's IP address (for audit log enrichment).
    pub ip_address: Option<String>,
    /// HTTP User-Agent header (for audit log enrichment).
    pub user_agent: Option<String>,
}

/// The storage key a **proxied** artifact is cached under.
///
/// `artifact:` plus the coordinate, including the `PackageId::artifact`
/// sub-coordinate when the kind uses one — a `vsix`, a `plugin`. Distinct from
/// [`crate::services::artifact_storage_key`], which is the `local:` key a
/// *published* artifact goes to: the two describe different halves of the same
/// catalogue, and using one where the other belongs asks a question that is
/// always answered "no".
///
/// A function rather than a `format!` at each site because there are now two
/// sites — the download path writes it, and the console's fetch button reads it
/// to decide whether a version is already held (RFC 0007-bis §4.4).
pub fn proxy_artifact_key(package_id: &crate::entities::PackageId) -> String {
    format!("artifact:{}", package_id.cache_key())
}

/// The cache key a coordinate's resolved metadata is stored under.
///
/// The `meta:` sibling of [`proxy_artifact_key`], and a function for the same
/// reason: `request_prelude` writes it and `ProxyService::cached_metadata_for`
/// reads it without ever going through the prelude, so the two spellings must
/// not drift — a reader that formats the key one character differently answers
/// `None` for every coordinate and looks like an empty cache.
pub(crate) fn proxy_meta_key(package_id: &crate::entities::PackageId) -> String {
    format!("meta:{}", package_id.cache_key())
}

/// Output of `ProxyService::handle`.
pub enum ProxyResponse {
    /// Artifact stream to forward to the HTTP client.
    Stream(ArtifactStream),
    /// [`Self::Stream`] for a forge coordinate whose ref was resolved first
    /// (RFC 0019 §4.2): the same bytes, plus what the ref resolved to, so the
    /// handler can say so in `X-BatleHub-Ref-Kind` and
    /// `X-BatleHub-Resolved-Commit`. A separate variant rather than a field on
    /// `Stream`, so every non-forge path is untouched.
    ForgeStream {
        stream: ArtifactStream,
        // Boxed together: a `ResolvedRef` and a `PackageId` are ~200 bytes of
        // strings, and every `ProxyResponse` in the process — one per request,
        // most of them plain `Stream`s — would otherwise be that wide.
        resolved: Box<crate::entities::ResolvedRef>,
        /// The coordinate these bytes are *stored* under, after the ref was
        /// rewritten onto its commit. It is not the coordinate the client
        /// asked for — `tarball/main` is stored under the commit — and the
        /// difference is the whole point of RFC 0019's identity model. The
        /// handler reports it as `X-BatleHub-Storage-Key`, which is what
        /// lets RFC 0008's bundle name a key the disconnected instance will
        /// actually look under.
        keyed: Box<crate::entities::PackageId>,
    },
    /// Access was denied; the caller should receive a 403.
    ///
    /// `verdict` is present when the refusal is a security verdict (RFC 0018
    /// §4.2): the handler answers with the registry's native body, the
    /// `X-BatleHub-*` headers and `Retry-After`, and hides the hold as a 404
    /// from a caller without `quarantine:read`. `None` is every other
    /// refusal, answered as it always was.
    Denied {
        reason: String,
        verdict: Option<Box<crate::entities::Verdict>>,
    },
    /// A served response under a `warned` verdict (RFC 0018 §4.2): the same
    /// bytes, plus the verdict so the handler can say so in the
    /// `X-BatleHub-*` headers. A wrapper rather than a field on each served
    /// variant, so every non-security path is untouched.
    Warned {
        response: Box<ProxyResponse>,
        verdict: Box<crate::entities::Verdict>,
    },
    /// A document composed from what this instance holds, on a route that
    /// otherwise streams an artifact — a forge's release by tag on an
    /// air-gapped instance (RFC 0008-bis §4.3). Carries its own content
    /// type and the `synthesised` count the handler turns into
    /// `X-BatleHub-Listing`.
    Document(crate::ports::VersionDocument),
}

impl ProxyResponse {
    /// The bytes, whatever they were served under, or the refusal.
    ///
    /// For the callers that want the stream and nothing else — the console's
    /// fetch, an asset read — and would otherwise have to know that a
    /// `Warned` wraps a `Stream` or a `ForgeStream`.
    pub fn into_stream(
        self,
    ) -> Result<ArtifactStream, (String, Option<Box<crate::entities::Verdict>>)> {
        match self {
            Self::Stream(stream) | Self::ForgeStream { stream, .. } => Ok(stream),
            Self::Document(doc) => {
                let bytes = match doc.body {
                    crate::ports::DocumentBody::Json(v) => {
                        bytes::Bytes::from(serde_json::to_vec(&v).unwrap_or_default())
                    }
                    crate::ports::DocumentBody::Text(t) => bytes::Bytes::from(t),
                };
                Ok(Box::pin(futures::stream::once(async move { Ok(bytes) })))
            }
            Self::Warned { response, .. } => response.into_stream(),
            Self::Denied { reason, verdict } => Err((reason, verdict)),
        }
    }
}

/// Caching proxy service: resolves metadata, evaluates rules, streams artifacts.
pub struct ProxyService {
    /// Hot-swappable state (registries, policies, size limit). Replaced atomically on reload.
    pub hot: HotConfigLock,
    pub storage: Arc<dyn StorageBackend>,
    pub cache: Arc<dyn CacheStore>,
    pub repo: Arc<dyn PackageRepository>,
    pub artifact_meta: Arc<dyn ArtifactCacheMeta>,
    /// In-memory counters for the stats dashboard (reset on restart).
    pub metrics: Arc<ProxyMetrics>,
    /// Optional SBOM service; when `None`, SBOM generation is disabled globally.
    pub sbom: Option<Arc<SbomService>>,
    /// Per-process coordination for the console's discovery read: the
    /// single-flight map and the negative cache (RFC 0007 §5.5).
    ///
    /// Not optional and not in `ExploreCache`: that cache is keyed by query and
    /// invalidated per registry, so a per-package absence marker keyed into it
    /// would be cleared by an unrelated catalogue write. Defaulted, so a test
    /// that does not exercise the discovery read need not know it exists.
    pub discovery: Arc<crate::services::upstream_detail::UpstreamDetailCoordinator>,
    /// Optional README service; when `None`, README capture is disabled globally.
    ///
    /// Per-registry configuration lives in `HotConfig::readme` and defaults to
    /// *on* — this field is the process-level wiring, absent only where nothing
    /// has a store to write to (RFC 0007 §4.1).
    pub readme: Option<Arc<ReadmeService>>,
}

pub(super) fn warn_if_audit_failed(r: Result<(), CoreError>, ctx: &str) {
    if let Err(e) = r {
        tracing::warn!(error = %e, ctx, "audit log write failed");
    }
}

impl ProxyService {
    /// Fetch an artifact from upstream, recording upstream latency, the
    /// `batlehub_upstream_errors_total` counter, and a `proxy_error` audit event
    /// on failure. Shared by `handle`'s firewall-only path and
    /// `cache::fetch_and_cache`'s cache-miss path — the only two callers of
    /// `RegistryClient::fetch_artifact`. `start` should be captured immediately
    /// before this call so the caller can reuse it for `time_upstream_stream`
    /// on the success path.
    pub(super) async fn fetch_artifact_or_record_error(
        &self,
        client: &Arc<dyn RegistryClient>,
        req: &ProxyRequest,
        registry_label: &Arc<str>,
        start: Instant,
    ) -> Result<FetchedArtifact, CoreError> {
        match client.fetch_artifact(&req.package_id).await {
            Ok(artifact) => {
                self.metrics.record_upstream_outcome(registry_label, true);
                Ok(artifact)
            }
            Err(e) => {
                record_upstream_duration(registry_label, "fetch_artifact", start, &self.metrics);
                self.metrics.record_upstream_outcome(registry_label, false);
                metrics::counter!("batlehub_upstream_errors_total", "registry" => Arc::clone(registry_label)).increment(1);
                warn_if_audit_failed(
                    self.repo
                        .record_access(
                            AccessEvent::proxy_error(
                                req.package_id.clone(),
                                req.identity.user_id.clone(),
                                req.identity.role.clone(),
                                e.to_string(),
                            )
                            .with_ip_ua(req.ip_address.clone(), req.user_agent.clone()),
                        )
                        .await,
                    "proxy error",
                );
                Err(e)
            }
        }
    }
}

/// The registry metrics label plus the request's start time — the pair every
/// helper in `cache.rs`'s fetch/verify/evict/serve chain needs so it can label
/// its own metrics and, on its return path, call [`finish_request`] with the
/// same values [`ProxyService::handle`](super::ProxyService::handle) captured
/// at the top of the request. Grouped into one value instead of two loose
/// trailing parameters repeated across 8 functions.
#[derive(Clone)]
pub(super) struct RequestTiming {
    pub(super) registry_label: Arc<str>,
    pub(super) start: Instant,
}

/// Emit the terminal per-request metrics — the `batlehub_requests_total{outcome}`
/// counter and the `batlehub_request_duration_seconds` histogram — at a request's
/// exit point. Collapses the counter+histogram pair that every return path repeats.
pub(super) fn finish_request(registry_label: &Arc<str>, outcome: &'static str, start: Instant) {
    metrics::counter!("batlehub_requests_total", "registry" => Arc::clone(registry_label), "outcome" => outcome).increment(1);
    metrics::histogram!("batlehub_request_duration_seconds", "registry" => Arc::clone(registry_label))
        .record(start.elapsed().as_secs_f64());
}

/// Records a single upstream-latency sample under
/// `batlehub_upstream_request_duration_seconds{registry,operation}`, and feeds
/// the in-process rolling latency EMA used for upstream-health degradation.
pub(super) fn record_upstream_duration(
    registry_label: &Arc<str>,
    operation: &'static str,
    start: Instant,
    metrics_svc: &ProxyMetrics,
) {
    let elapsed = start.elapsed();
    metrics::histogram!(
        "batlehub_upstream_request_duration_seconds",
        "registry" => Arc::clone(registry_label),
        "operation" => operation
    )
    .record(elapsed.as_secs_f64());
    metrics_svc.record_upstream_latency(registry_label, elapsed.as_millis() as u64);
}

/// Times a call out to an upstream registry client and records it under
/// `batlehub_upstream_request_duration_seconds`, regardless of whether the call
/// succeeds — a hung or slow-failing upstream is exactly the "degraded" case this
/// metric exists to catch, so failures must count too.
///
/// Only appropriate for calls whose future resolves once the *entire* answer is
/// available, e.g. `resolve_metadata`. `fetch_artifact` returns as soon as response
/// headers arrive and hands back a lazily-consumed body stream, so timing its
/// future alone would only measure time-to-first-byte and miss a slow/degraded
/// body transfer — use [`time_upstream_stream`] for that instead.
pub(super) async fn time_upstream_call<T, E>(
    registry_label: &Arc<str>,
    operation: &'static str,
    metrics_svc: &ProxyMetrics,
    fut: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let start = Instant::now();
    let result = fut.await;
    record_upstream_duration(registry_label, operation, start, metrics_svc);
    result
}

/// Wraps a freshly-fetched artifact byte stream so `operation`'s latency is
/// recorded once the stream is fully drained — cleanly exhausted or ended by an
/// error — rather than when the initial response headers arrived. `start` should
/// be captured immediately before the `fetch_artifact` call that produced this
/// stream, so the recorded duration covers the whole transfer.
///
/// Every consumer of a `FetchedArtifact::stream` (the cached-fetch path, the
/// no-store passthrough, firewall-only mode, and cache warming) must route the
/// stream through this so a slow/degraded body transfer — not just a slow
/// response header — trips the upstream-latency alert.
pub(super) fn time_upstream_stream(
    registry_label: Arc<str>,
    operation: &'static str,
    start: Instant,
    metrics_svc: Arc<ProxyMetrics>,
    stream: ArtifactStream,
) -> ArtifactStream {
    Box::pin(stream::unfold(
        (stream, registry_label, operation, start, metrics_svc, false),
        |(mut stream, registry_label, operation, start, metrics_svc, done)| async move {
            if done {
                return None;
            }
            let next = stream.next().await;
            let finished = !matches!(next, Some(Ok(_)));
            if finished {
                record_upstream_duration(&registry_label, operation, start, &metrics_svc);
            }
            next.map(|item| {
                (
                    item,
                    (
                        stream,
                        registry_label,
                        operation,
                        start,
                        metrics_svc,
                        finished,
                    ),
                )
            })
        },
    ))
}

#[cfg(test)]
mod tests;
