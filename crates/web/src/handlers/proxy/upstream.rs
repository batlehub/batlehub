//! The one place under `handlers/proxy/` that makes an outbound HTTP call.
//!
//! RFC 0009 §4.2. Every passthrough used to build its own `reqwest` call —
//! `forward_npm_audit`, the Go vulnerability database's `forward_get`, NuGet's
//! vulnerability pages — with no cache read and no cache write. So each failed
//! outright the moment its upstream was unreachable, including when we had
//! answered the identical request a minute earlier, and
//! `docs/use/vulnerability-proxy.md` presented all three as proxied features. A
//! vulnerability check that fails closed on upstream loss is the one most
//! likely to be running in a pipeline that must not stop.
//!
//! Routing every passthrough through here is the enforcement: the next one
//! inherits the three rungs by having nowhere else to go. The rungs themselves
//! are [`ProxyService::cached_passthrough`], which owns the policy; this module
//! owns only the transport, because core does not know what HTTP is.

use std::sync::Arc;
use std::time::Duration;

use actix_web::{http::StatusCode, HttpResponse};
use futures::StreamExt;

use batlehub_core::error::CoreError;
use batlehub_core::services::proxy::{FetchOutcome, UpstreamBytes};
use batlehub_core::services::ProxyService;

use crate::error::AppError;

/// TTL for a passthrough when the registry sets no `metadata_ttl`.
///
/// Without a fallback the first response would be stored with no expiry and
/// never refresh, which for an advisory database means pinning yesterday's
/// answer forever — the opposite of the problem this path exists to solve.
const DEFAULT_PASSTHROUGH_TTL: Duration = Duration::from_secs(30 * 60);

/// Ceiling on a buffered upstream body.
///
/// These responses are read whole and land base64-expanded (+33%) in the cache,
/// so the one path that buffers without a limit is the one that runs the
/// process out of memory on a hostile or misconfigured upstream. Matches the
/// ceiling `jetbrains_marketplace::cached_forward` already applies for the same
/// reason.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// The registry's metadata TTL, or [`DEFAULT_PASSTHROUGH_TTL`].
pub(crate) async fn passthrough_ttl(svc: &ProxyService, registry: &str) -> Duration {
    let hot = svc.hot.read().await;
    hot.policies
        .get(registry)
        .and_then(|p| p.metadata_ttl)
        .unwrap_or(DEFAULT_PASSTHROUGH_TTL)
}

/// Read a response body, refusing one that would not fit in memory.
async fn bounded_body(resp: reqwest::Response) -> Result<Vec<u8>, CoreError> {
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_BODY_BYTES {
            return Err(CoreError::Registry(format!(
                "upstream response is {len} bytes, over the {MAX_BODY_BYTES}-byte passthrough limit"
            )));
        }
    }
    // `content_length` is absent under chunked encoding, so the cap has to hold
    // while streaming as well as before it.
    let mut out: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| CoreError::Registry(format!("reading upstream response: {e}")))?;
        if out.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(CoreError::Registry(format!(
                "upstream response exceeded the {MAX_BODY_BYTES}-byte passthrough limit"
            )));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// One upstream request, described rather than performed.
pub struct Outbound {
    pub method: reqwest::Method,
    pub url: String,
    /// JSON request body, for the POST passthroughs. Part of the cache key at
    /// the call site, never here: `npm audit` POSTs the dependency set, so two
    /// projects asking one registry are two different questions.
    pub json_body: Option<serde_json::Value>,
}

impl Outbound {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: reqwest::Method::GET,
            url: url.into(),
            json_body: None,
        }
    }

    pub fn post_json(url: impl Into<String>, body: serde_json::Value) -> Self {
        Self {
            method: reqwest::Method::POST,
            url: url.into(),
            json_body: Some(body),
        }
    }
}

/// Perform `out`, through the cache, and render the result.
///
/// `cache_key` must be namespaced by the caller (`audit:`, `sumdb:`) and must
/// include everything that selects the response.
///
/// Three outcomes reach [`ProxyService::cached_passthrough`], and the split
/// matters:
///
/// - **2xx** — cacheable. The answer we want to still have during an outage.
/// - **4xx** — the upstream is up and said no. Forwarded verbatim, never
///   cached, and never replaced by a stale entry: answering a fresh `404` from
///   a stale `200` would be inventing data rather than surviving an outage.
/// - **5xx or a transport error** — an outage. This is what rung 3 exists for.
pub async fn cached_forward(
    svc: &Arc<ProxyService>,
    http: &reqwest::Client,
    registry: &str,
    cache_key: &str,
    out: Outbound,
) -> Result<HttpResponse, AppError> {
    cached_forward_with(svc, http, registry, cache_key, out, false).await
}

/// [`cached_forward`], and with `offline` the upstream is never dialled: a
/// cached answer is served, anything else is the outage rung — for an
/// air-gapped instance, whose upstream must not be asked at all.
pub async fn cached_forward_with(
    svc: &Arc<ProxyService>,
    http: &reqwest::Client,
    registry: &str,
    cache_key: &str,
    out: Outbound,
    offline: bool,
) -> Result<HttpResponse, AppError> {
    cached_forward_filtered(svc, http, registry, cache_key, out, offline, |body| body).await
}

/// [`cached_forward_with`], with `filter` applied to a `2xx` body on its way
/// out. The cache keeps upstream's bytes; the filter runs on every answer, so
/// a block or an age gate takes effect without a cache flush.
pub async fn cached_forward_filtered(
    svc: &Arc<ProxyService>,
    http: &reqwest::Client,
    registry: &str,
    cache_key: &str,
    out: Outbound,
    offline: bool,
    filter: impl FnOnce(Vec<u8>) -> Vec<u8>,
) -> Result<HttpResponse, AppError> {
    let ttl = passthrough_ttl(svc, registry).await;
    let result = svc
        .cached_passthrough(registry, cache_key, Some(ttl), || async {
            if offline {
                return Err(CoreError::Registry(
                    "air gap: this instance does not dial out".to_owned(),
                ));
            }
            let mut rb = http.request(out.method, &out.url);
            if let Some(body) = &out.json_body {
                rb = rb.json(body);
            }
            let resp = rb
                .send()
                .await
                .map_err(|e| CoreError::Registry(format!("upstream request failed: {e}")))?;

            let status = resp.status();
            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/json")
                .to_owned();
            let body = bounded_body(resp).await?;
            let bytes = UpstreamBytes { content_type, body };

            if status.is_success() {
                Ok(FetchOutcome::Cacheable(bytes))
            } else if status.is_server_error() {
                Err(CoreError::Registry(format!("upstream returned {status}")))
            } else {
                Ok(FetchOutcome::Definite {
                    status: status.as_u16(),
                    bytes,
                })
            }
        })
        .await
        .map_err(|e| AppError::bad_gateway(format!("upstream unavailable: {e}")))?;

    let status = StatusCode::from_u16(result.status).unwrap_or(StatusCode::OK);
    let body = if status.is_success() {
        filter(result.bytes.body)
    } else {
        result.bytes.body
    };
    Ok(HttpResponse::build(status)
        .content_type(result.bytes.content_type.clone())
        // Rung 3 is visible rather than silent: a client (or an operator
        // reading logs) can tell an answer served during an upstream outage
        // from a live one.
        .insert_header(("X-BatleHub-Cache", result.freshness.header_value()))
        .body(body))
}

/// A `GET` with its own total `timeout`, the body capped at `cap` bytes, and
/// any non-2xx an error — for a caller that runs its own
/// `ProxyService::cached_passthrough` closure (RFC 0045's registry archive),
/// where an upstream "no" must be served from the held copy, not forwarded.
pub(crate) async fn get_bounded(
    http: &reqwest::Client,
    url: &str,
    timeout: Duration,
    cap: usize,
) -> Result<Vec<u8>, CoreError> {
    let mut resp = http
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| CoreError::Registry(format!("upstream unreachable: {e}")))?;
    if !resp.status().is_success() {
        return Err(CoreError::Registry(format!(
            "upstream answered {}",
            resp.status()
        )));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| CoreError::Registry(format!("upstream body: {e}")))?
    {
        if body.len() + chunk.len() > cap {
            return Err(CoreError::Registry(format!(
                "upstream body over {cap} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
