//! Cached upstream forwarding for query/list endpoints.
//!
//! Unlike the npm-audit passthrough (plain forward, nothing persisted), these
//! helpers persist successful response bodies in the `CacheStore` (reached
//! through the already-injected `ProxyService`, whose `cache` field is public)
//! as synthetic `CacheEntry` values, and fall back to the stale entry when the
//! upstream is unreachable — so the fixed-URL blobs the IDE fetches at startup
//! (`brokenPlugins.json`, `pluginsXMLIds.json`, …) and repeat searches keep
//! working after upstream loss.
//!
//! Keys live in a dedicated `fwd:` namespace so they can never collide with
//! the `meta:`/`artifact:` keys the proxy pipeline owns.

use std::sync::Arc;

use actix_web::HttpResponse;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use bytes::{Bytes, BytesMut};
use chrono::Utc;
use futures::StreamExt;
use sha2::{Digest, Sha256};

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    ports::CacheEntry,
    services::ProxyService,
};

use crate::{error::AppError, UpstreamMap};

/// Query parameters stripped from forward cache keys: per-request / per-machine
/// noise that would fragment the cache into single-hit entries and bloat it.
/// Keep this list in sync with the risk note in the implementation plan.
const VOLATILE_PARAMS: &[&str] = &["uuid", "machineid", "clientid", "_"];

/// TTL applied to forwarded blobs when the registry has no `metadata_ttl`:
/// without a fallback the first response would be cached without expiry and
/// never refresh (`brokenPlugins.json` & co. do change upstream over time).
const DEFAULT_FORWARD_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Ceiling on a forwarded upstream body. These endpoints carry JSON/XML lists
/// (the largest, `pluginsXMLIds.json`, is a few MiB); every other upstream
/// path is size-bounded, so this one must be too — the body is buffered whole
/// and lands base64-expanded (+33%) in the cache.
const MAX_FORWARD_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// A forwarded (or cache-served) upstream response body.
pub struct ForwardedBody {
    pub content_type: String,
    pub body: Bytes,
}

impl ForwardedBody {
    pub fn into_response(self) -> HttpResponse {
        HttpResponse::Ok()
            .content_type(self.content_type)
            .body(self.body)
    }
}

/// Cache key for a GET forward: `fwd:{registry}:{path}?{sorted params}`, with
/// volatile params dropped (same request ± volatile param → same key).
pub fn forward_cache_key(registry: &str, path: &str, query_string: &str) -> String {
    let mut params: Vec<(String, String)> = form_urlencoded::parse(query_string.as_bytes())
        .filter(|(k, _)| !VOLATILE_PARAMS.contains(&k.to_ascii_lowercase().as_str()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    params.sort();
    // Re-encode the decoded pairs: joining them raw would collapse distinct
    // requests into one key (`?a=x%26b%3Dy` vs `?a=x&b=y`).
    let mut ser = form_urlencoded::Serializer::new(String::new());
    for (k, v) in &params {
        ser.append_pair(k, v);
    }
    let qs = ser.finish();
    format!("fwd:{registry}:{path}?{qs}")
}

/// Cache key for the compatible-updates POST: build + hash of the sorted xmlIds.
pub fn compatible_updates_cache_key(registry: &str, build: &str, xml_ids: &[String]) -> String {
    let mut sorted = xml_ids.to_vec();
    sorted.sort();
    let digest = hex::encode(Sha256::digest(sorted.join("\n").as_bytes()));
    format!("fwd:{registry}:api/search/updates/compatible?build={build}&ids={digest}")
}

fn entry_from_body(registry: &str, content_type: &str, body: &Bytes) -> CacheEntry {
    // Off-label but deliberate: the body rides in `PackageMetadata.extra` (the
    // CacheStore only knows how to hold metadata). base64 keeps it JSON-safe
    // regardless of content.
    let metadata = PackageMetadata::minimal(
        PackageId::registry_document(registry, "_forward", "_"),
        serde_json::json!({
            "body_b64": B64.encode(body),
            "content_type": content_type,
        }),
    );
    CacheEntry {
        metadata,
        cached_at: Utc::now(),
        expires_at: None,
    }
}

fn body_from_entry(entry: &CacheEntry) -> Option<ForwardedBody> {
    let extra = &entry.metadata.extra;
    let body = B64.decode(extra.get("body_b64")?.as_str()?).ok()?;
    Some(ForwardedBody {
        content_type: extra
            .get("content_type")
            .and_then(|v| v.as_str())
            .unwrap_or("application/json")
            .to_owned(),
        body: Bytes::from(body),
    })
}

async fn forward_ttl(svc: &ProxyService, registry: &str) -> std::time::Duration {
    let hot = svc.hot.read().await;
    hot.policies
        .get(registry)
        .and_then(|p| p.metadata_ttl)
        .unwrap_or(DEFAULT_FORWARD_TTL)
}

async fn serve_stale_or(
    svc: &ProxyService,
    registry: &str,
    cache_key: &str,
    err: AppError,
) -> Result<ForwardedBody, AppError> {
    // Bounded by the registry's own `serve_stale_metadata`, like every other
    // stale-on-error path (RFC 0009 §4.2). This used to serve stale
    // unconditionally, so an operator who turned stale serving off — because for
    // their estate a stale answer is worse than none — still got one here.
    if !svc.serves_stale(registry).await {
        return Err(err);
    }
    match svc.cache.get_stale(cache_key).await {
        Ok(Some(stale)) => {
            if let Some(body) = body_from_entry(&stale) {
                tracing::warn!(key = %cache_key, "upstream unavailable; serving stale forwarded body");
                return Ok(body);
            }
            Err(err)
        }
        _ => Err(err),
    }
}

async fn handle_upstream_response(
    svc: &ProxyService,
    registry: &str,
    cache_key: &str,
    result: Result<reqwest::Response, reqwest::Error>,
) -> Result<ForwardedBody, AppError> {
    let resp = match result {
        Ok(resp) => resp,
        Err(e) => {
            return serve_stale_or(
                svc,
                registry,
                cache_key,
                AppError::bad_gateway(format!("upstream request failed: {e}")),
            )
            .await;
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let err = if status == reqwest::StatusCode::NOT_FOUND {
            AppError::not_found("not found upstream".to_owned())
        } else {
            AppError::bad_gateway(format!("upstream returned {status}"))
        };
        // A 404 is an authoritative answer, not an outage — only server-side
        // failures fall back to stale.
        if status.is_server_error() {
            return serve_stale_or(svc, registry, cache_key, err).await;
        }
        return Err(err);
    }

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_owned();
    if let Some(len) = resp.content_length() {
        if len > MAX_FORWARD_BODY_BYTES {
            return Err(AppError::bad_gateway(format!(
                "upstream body of {len} bytes exceeds the {MAX_FORWARD_BODY_BYTES}-byte forward limit"
            )));
        }
    }
    let mut buf = BytesMut::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|e| AppError::bad_gateway(format!("upstream response read failed: {e}")))?;
        if buf.len() as u64 + chunk.len() as u64 > MAX_FORWARD_BODY_BYTES {
            return Err(AppError::bad_gateway(format!(
                "upstream body exceeds the {MAX_FORWARD_BODY_BYTES}-byte forward limit"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    let body = buf.freeze();

    let ttl = forward_ttl(svc, registry).await;
    if let Err(e) = svc
        .cache
        .set(
            cache_key,
            entry_from_body(registry, &content_type, &body),
            Some(ttl),
        )
        .await
    {
        tracing::warn!(key = %cache_key, error = %e, "failed to cache forwarded body (non-fatal)");
    }

    Ok(ForwardedBody { content_type, body })
}

fn upstream_base(upstream_map: &UpstreamMap, registry: &str) -> Result<String, AppError> {
    upstream_map
        .upstream_for(registry)
        .map(|u| u.trim_end_matches('/').to_owned())
        .ok_or_else(|| AppError::not_found(format!("no upstream configured for '{registry}'")))
}

/// Cache-first GET forward: fresh cache entry → served directly; miss →
/// upstream fetch, persisted on success; upstream failure → stale entry.
pub async fn cached_forward_get(
    svc: &Arc<ProxyService>,
    upstream_map: &UpstreamMap,
    client: &reqwest::Client,
    registry: &str,
    path_and_query: &str,
    cache_key: &str,
) -> Result<ForwardedBody, AppError> {
    if let Ok(Some(entry)) = svc.cache.get(cache_key).await {
        if let Some(body) = body_from_entry(&entry) {
            return Ok(body);
        }
    }

    let upstream = upstream_base(upstream_map, registry)?;
    let url = format!("{upstream}/{}", path_and_query.trim_start_matches('/'));
    let result = client.get(&url).send().await;
    handle_upstream_response(svc, registry, cache_key, result).await
}

/// Cache-first POST forward (JSON body), same freshness/stale semantics as
/// [`cached_forward_get`]. The caller supplies the cache key — POST bodies
/// don't key themselves (cf. [`compatible_updates_cache_key`]).
pub async fn cached_forward_post_json(
    svc: &Arc<ProxyService>,
    upstream_map: &UpstreamMap,
    client: &reqwest::Client,
    registry: &str,
    path: &str,
    body: &serde_json::Value,
    cache_key: &str,
) -> Result<ForwardedBody, AppError> {
    if let Ok(Some(entry)) = svc.cache.get(cache_key).await {
        if let Some(cached) = body_from_entry(&entry) {
            return Ok(cached);
        }
    }

    let upstream = upstream_base(upstream_map, registry)?;
    let url = format!("{upstream}/{}", path.trim_start_matches('/'));
    let result = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(body)
        .send()
        .await;
    handle_upstream_response(svc, registry, cache_key, result).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_drops_volatile_params_and_sorts() {
        let a = forward_cache_key(
            "jbm",
            "api/search/plugins",
            "search=rust&max=10&uuid=abc-123",
        );
        let b = forward_cache_key("jbm", "api/search/plugins", "max=10&search=rust");
        let c = forward_cache_key(
            "jbm",
            "api/search/plugins",
            "max=10&search=rust&UUID=zzz&machineId=m1",
        );
        assert_eq!(a, b, "same request ± volatile param must share one key");
        assert_eq!(a, c, "volatile params are dropped case-insensitively");
        assert_eq!(a, "fwd:jbm:api/search/plugins?max=10&search=rust");
    }

    #[test]
    fn key_differs_on_meaningful_params() {
        let a = forward_cache_key("jbm", "api/search/plugins", "search=rust");
        let b = forward_cache_key("jbm", "api/search/plugins", "search=go");
        assert_ne!(a, b);
    }

    #[test]
    fn compatible_updates_key_is_order_insensitive() {
        let a = compatible_updates_cache_key(
            "jbm",
            "IU-241.1",
            &["b.plugin".into(), "a.plugin".into()],
        );
        let b = compatible_updates_cache_key(
            "jbm",
            "IU-241.1",
            &["a.plugin".into(), "b.plugin".into()],
        );
        assert_eq!(a, b);
        let c = compatible_updates_cache_key("jbm", "IU-242.1", &["a.plugin".into()]);
        assert_ne!(a, c);
    }

    #[test]
    fn body_round_trips_through_entry() {
        let body = Bytes::from_static(b"{\"plugins\":[]}");
        let entry = entry_from_body("jbm", "application/json", &body);
        let back = body_from_entry(&entry).unwrap();
        assert_eq!(back.body, body);
        assert_eq!(back.content_type, "application/json");
    }
}
