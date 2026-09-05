use std::sync::Arc;

use actix_web::{get, post, route, web, HttpRequest, HttpResponse, Responder};
use sha2::{Digest, Sha256};

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::PackageId,
    error::CoreError,
    services::{LocalRegistryService, ProxyService, PublishRequest},
};

use super::common::{
    collect_payload, extract_signature_headers, proxy_stream, require_local_mode,
    require_registry_type, ArtifactSignature,
};
use crate::handlers::schemas::{ArtifactBytes, MessageResponse, UpstreamDocument};
use crate::{
    error::AppError, extractors::AuthIdentity, services::NotificationService, RegistryMap,
    RegistryModeMap,
};
use batlehub_core::entities::Action;

// ── Proxy routes ──────────────────────────────────────────────────────────────

/// Serve (and optionally merge) a conda channel's `repodata.json` for a
/// specific platform (e.g. `linux-64`, `noarch`).
///
/// - **Proxy mode**: stream `repodata.json` from upstream through the cache.
/// - **Local mode**: return only locally-published packages.
/// - **Hybrid mode**: merge upstream repodata with locally-published packages.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{platform}/repodata.json",
    tag = "proxy/conda",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("platform" = String, Path, description = "Platform string, e.g. linux-64 or noarch"),
    ),
    responses(
        (status = 200, description = "repodata.json", body = UpstreamDocument),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Channel not found"),
    ),
    security(("bearer_token" = [])),
)]
#[route(
    "/proxy/{registry}/{platform}/repodata.json",
    method = "GET",
    method = "HEAD"
)]
pub async fn conda_repodata(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, platform) = path.into_inner();
    require_registry_type(&registry, "conda", &map)?;

    let (body, synthesised) = repodata_bytes(
        svc,
        local_svc,
        &registry,
        &platform,
        identity,
        mode_map.get(&registry),
        batlehub_core::ports::DocumentKind::Versions,
    )
    .await?;

    let mut builder = HttpResponse::Ok();
    builder.content_type("application/json");
    mark_synthesised(&mut builder, synthesised);
    Ok(builder.body(body))
}

/// The bytes of one repodata document, mode-aware and filtered.
///
/// Shared by the plain route and both compressed ones (RFC 0009 §7.5) so the
/// three encodings cannot come to describe different channels — which is the
/// failure a second, parallel fetch path would eventually produce.
/// A repodata composed from the held set says so (RFC 0008-bis §4.2), on
/// every encoding it is served in.
fn mark_synthesised(builder: &mut actix_web::HttpResponseBuilder, synthesised: Option<u32>) {
    if let Some(held) = synthesised {
        builder.insert_header(("X-BatleHub-Listing", "synthesised"));
        builder.insert_header(("X-BatleHub-Listing-Held", held.to_string()));
    }
}

#[allow(clippy::too_many_arguments)]
async fn repodata_bytes(
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    registry: &str,
    platform: &str,
    identity: AuthIdentity,
    mode: RegistryMode,
    kind: batlehub_core::ports::DocumentKind,
) -> Result<(Vec<u8>, Option<u32>), AppError> {
    if mode == RegistryMode::Local {
        let repodata = local_svc
            .get_conda_repodata(registry, platform, &identity.0)
            .await
            .map_err(AppError::from)?;
        return Ok((serde_json::to_vec(&repodata).unwrap_or_default(), None));
    }

    // Proxy mode, and the upstream half of Hybrid. `multi_package_document`
    // rather than `proxy_stream`: `repodata.json` describes a whole channel, and
    // a blocked package left in it gets selected by the solver and then refused
    // at download — mid `conda install`, after the environment plan is fixed.
    //
    // Its blocked set comes from a 30-second snapshot rather than a per-request
    // query; see `ProxyService::blocked_in_registry_snapshot` for why, and the
    // admin guide for the operator-facing statement of the delay.
    //
    // Kept rather than moved: the Hybrid branch below needs the same identity to
    // filter the local half, and the two halves must be filtered for the *same*
    // caller or the merge describes a channel no one is entitled to.
    let caller = identity.0.clone();
    let (upstream, synthesised) =
        fetch_conda_index(svc, registry, platform, identity, kind).await?;

    if mode == RegistryMode::Hybrid {
        let local_repodata = local_svc
            .get_conda_repodata(registry, platform, &caller)
            .await
            .map_err(AppError::from)?;
        return Ok((merge_repodata(&upstream, &local_repodata), synthesised));
    }

    Ok((upstream, synthesised))
}

/// Fetch and filter one of a conda channel's index documents, as bytes.
///
/// Bytes rather than a `VersionDocument` because the Hybrid path has to merge
/// locally published packages into it before answering.
async fn fetch_conda_index(
    svc: web::Data<Arc<ProxyService>>,
    registry: &str,
    platform: &str,
    identity: AuthIdentity,
    kind: batlehub_core::ports::DocumentKind,
) -> Result<(Vec<u8>, Option<u32>), AppError> {
    // The *platform* is the coordinate: a conda listing is scoped to a subdir,
    // not to a package.
    let req = batlehub_core::services::ProxyRequest {
        package_id: PackageId::new(registry, platform, "__repodata__"),
        identity: identity.0,
        action: Action::ReleasesRead.to_owned(),
        ip_address: None,
        user_agent: None,
    };
    let doc = svc
        .multi_package_document(&req, kind, "")
        .await
        .map_err(AppError::from)?;
    let synthesised = doc.synthesised;
    let bytes = match doc.body {
        batlehub_core::ports::DocumentBody::Json(v) => serde_json::to_vec(&v).unwrap_or_default(),
        batlehub_core::ports::DocumentBody::Text(t) => t.into_bytes(),
    };
    Ok((bytes, synthesised))
}

/// `repodata.json.zst` — the first index request conda 23.x and mamba make.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{platform}/repodata.json.zst",
    tag = "proxy/conda",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("platform" = String, Path, description = "Platform string, e.g. linux-64 or noarch"),
    ),
    responses(
        (status = 200, description = "zstd-compressed repodata.json", body = ArtifactBytes, content_type = "application/zstd"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Channel not found"),
    ),
    security(("bearer_token" = [])),
)]
#[route(
    "/proxy/{registry}/{platform}/repodata.json.zst",
    method = "GET",
    method = "HEAD"
)]
pub async fn conda_repodata_zst(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, platform) = path.into_inner();
    serve_compressed_repodata(
        Encoding::Zstd,
        registry,
        platform,
        identity,
        svc,
        local_svc,
        map,
        mode_map,
    )
    .await
}

/// `repodata.json.bz2` — the older compressed encoding, for clients that
/// predate zstd support.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{platform}/repodata.json.bz2",
    tag = "proxy/conda",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("platform" = String, Path, description = "Platform string, e.g. linux-64 or noarch"),
    ),
    responses(
        (status = 200, description = "bzip2-compressed repodata.json", body = ArtifactBytes, content_type = "application/x-bzip2"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Channel not found"),
    ),
    security(("bearer_token" = [])),
)]
#[route(
    "/proxy/{registry}/{platform}/repodata.json.bz2",
    method = "GET",
    method = "HEAD"
)]
pub async fn conda_repodata_bz2(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, platform) = path.into_inner();
    serve_compressed_repodata(
        Encoding::Bzip2,
        registry,
        platform,
        identity,
        svc,
        local_svc,
        map,
        mode_map,
    )
    .await
}

/// `channeldata.json` — the cross-platform summary `conda search` reads.
///
/// A whole-channel document like `repodata.json`, so it filters through
/// `dispatch_multi` against the same 30-second snapshot. Its absence degraded
/// search rather than install, which is why it is here rather than in phase 1.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/channeldata.json",
    tag = "proxy/conda",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "channeldata.json", body = UpstreamDocument),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Channel not found"),
    ),
    security(("bearer_token" = [])),
)]
#[route("/proxy/{registry}/channeldata.json", method = "GET", method = "HEAD")]
pub async fn conda_channeldata(
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "conda", &map)?;

    // No platform: `channeldata.json` sits at the channel root and describes
    // every subdir at once.
    let (bytes, synthesised) = fetch_conda_index(
        svc,
        &registry,
        "_channeldata",
        identity,
        batlehub_core::ports::DocumentKind::CHANNELDATA,
    )
    .await?;

    let mut builder = HttpResponse::Ok();
    builder.content_type("application/json");
    mark_synthesised(&mut builder, synthesised);
    Ok(builder.body(bytes))
}

// ── Compressed repodata (RFC 0009 §7.5) ───────────────────────────────────────
//
// conda 23.x and mamba request `repodata.json.zst` **first** and fall back on
// 404. The `{filename}` route regex admits only `.tar.bz2`/`.conda`, so a `.zst`
// request did not reach a handler at all — it fell through the whole route table
// into the npm three-segment catch-all. Every client therefore paid the full
// uncompressed transfer of a document that runs to tens of megabytes and is
// fetched on every solve.
//
// The filter runs on the JSON and compression happens after, so RFC 0006's
// guarantee carries over unchanged: there is no second filter here, only a
// second encoding of the first one's output.

/// How a compressed repodata variant is encoded.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Zstd,
    Bzip2,
}

impl Encoding {
    fn suffix(self) -> &'static str {
        match self {
            Self::Zstd => "zst",
            Self::Bzip2 => "bz2",
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Self::Zstd => "application/zstd",
            Self::Bzip2 => "application/x-bzip2",
        }
    }

    fn compress(self, raw: &[u8]) -> Result<Vec<u8>, AppError> {
        match self {
            // Level 3 is zstd's own default and what conda-forge publishes at:
            // higher levels cost materially more CPU for a few percent of size
            // on a document we recompress whenever the block list changes.
            Self::Zstd => zstd::encode_all(raw, 3)
                .map_err(|e| AppError::internal(format!("compressing repodata: {e}"))),
            Self::Bzip2 => {
                use bzip2::write::BzEncoder;
                use std::io::Write;
                let mut enc = BzEncoder::new(Vec::new(), bzip2::Compression::default());
                enc.write_all(raw)
                    .and_then(|_| enc.finish())
                    .map_err(|e| AppError::internal(format!("compressing repodata: {e}")))
            }
        }
    }
}

/// How long a compressed copy of an upstream `repodata.json` may outlive the
/// fetch it was derived from.
///
/// It used to be stored with no expiry at all. Bounded rather than matched to
/// the registry's own `metadata_ttl` because a handler cannot read the policy;
/// the point is that the derived copy expires on its own, not that it expires
/// in step.
const COMPRESSED_REPODATA_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// Serve a compressed encoding of the filtered `repodata.json`.
///
/// Compressing tens of megabytes on every request is not affordable, and
/// caching the *filtered* document is forbidden — a cached filtered copy keeps
/// serving a version for the rest of its TTL after an operator blocked it
/// (RFC 0006 §4.2). Both are avoided by keying the cached compressed bytes on
/// the blocked-set fingerprint: a block change produces a different key, so the
/// entry filtered under the old list is never read rather than being raced
/// against a TTL.
///
/// That fingerprint covers *blocking* and nothing else, which is why this cache
/// is skipped entirely in local and hybrid mode. There the channel is generated
/// from the database, and publishing a package changes it without changing the
/// blocked set: the compressed copy — written with no expiry — kept describing
/// the channel as it was before the publish, for good. The plain
/// `repodata.json` is regenerated per request and was correct, so the two
/// encodings described different channels, and the one micromamba asks for
/// first is this one. Measured with micromamba 2.9.0 against a real server:
/// a package published after a client had probed once stayed invisible while
/// `curl` on the `.json` URL showed it (RFC 0009 §12.13).
///
/// In proxy mode the bytes derive from an upstream document that has its own
/// TTL, so they are cached — but bounded, so a derived copy cannot outlive
/// what it was derived from.
#[allow(clippy::too_many_arguments)]
async fn serve_compressed_repodata(
    encoding: Encoding,
    registry: String,
    platform: String,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<HttpResponse, AppError> {
    require_registry_type(&registry, "conda", &map)?;

    let mode = mode_map.get(&registry);
    // Local and hybrid channels are generated from the database on every
    // request; a cache keyed only on the blocked set cannot see a publish.
    let cacheable = mode == RegistryMode::Proxy;

    let fingerprint = svc
        .blocked_snapshot_fingerprint(&registry, batlehub_core::entities::RegistryKind::Conda)
        .await;
    let cache_key = format!(
        "repodata-{}:{registry}:{platform}:{fingerprint}",
        encoding.suffix()
    );

    let cached = if cacheable {
        svc.cache.get(&cache_key).await.ok().flatten()
    } else {
        None
    };
    if let Some(entry) = cached {
        if let Some(bytes) = entry
            .metadata
            .extra
            .get("compressed_b64")
            .and_then(|v| v.as_str())
            .and_then(|s| {
                use base64::{engine::general_purpose::STANDARD, Engine as _};
                STANDARD.decode(s).ok()
            })
        {
            // A composed repodata cached in its encoding is still composed:
            // the flag was stored beside the bytes.
            let synthesised = entry
                .metadata
                .extra
                .get("synthesised")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);
            let mut builder = HttpResponse::Ok();
            builder.content_type(encoding.content_type());
            builder.insert_header(("X-BatleHub-Cache", "hit"));
            mark_synthesised(&mut builder, synthesised);
            return Ok(builder.body(bytes));
        }
    }

    // The uncompressed path, filter and hybrid merge included — so the two
    // encodings cannot describe a different channel from the plain one.
    let (raw, synthesised) = repodata_bytes(
        svc.clone(),
        local_svc,
        &registry,
        &platform,
        identity,
        mode,
        batlehub_core::ports::DocumentKind::Versions,
    )
    .await?;

    let compressed = encoding.compress(&raw)?;

    let encoded = {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        STANDARD.encode(&compressed)
    };
    let entry = batlehub_core::ports::CacheEntry {
        metadata: batlehub_core::entities::PackageMetadata::minimal(
            PackageId::new(&registry, &platform, "__repodata__"),
            serde_json::json!({ "compressed_b64": encoded, "synthesised": synthesised }),
        ),
        cached_at: chrono::Utc::now(),
        expires_at: None,
    };
    if cacheable {
        if let Err(e) = svc
            .cache
            .set(&cache_key, entry, Some(COMPRESSED_REPODATA_TTL))
            .await
        {
            tracing::warn!(key = %cache_key, error = %e, "caching compressed repodata failed");
        }
    }

    let mut builder = HttpResponse::Ok();
    builder.content_type(encoding.content_type());
    builder.insert_header(("X-BatleHub-Cache", "miss"));
    mark_synthesised(&mut builder, synthesised);
    Ok(builder.body(compressed))
}

/// Merge a locally-built repodata JSON overlay into upstream `repodata.json` bytes.
fn merge_repodata(upstream_bytes: &[u8], local: &serde_json::Value) -> Vec<u8> {
    let mut upstream: serde_json::Value = match serde_json::from_slice(upstream_bytes) {
        Ok(v) => v,
        Err(_) => return serde_json::to_vec(local).unwrap_or_default(),
    };

    for key in ["packages", "packages.conda"] {
        if let Some(local_pkgs) = local.get(key).and_then(|v| v.as_object()) {
            let upstream_pkgs = upstream.get_mut(key).and_then(|v| v.as_object_mut());
            if let Some(up) = upstream_pkgs {
                for (filename, entry) in local_pkgs {
                    up.insert(filename.clone(), entry.clone());
                }
            } else {
                upstream[key] = local[key].clone();
            }
        }
    }

    serde_json::to_vec(&upstream).unwrap_or_default()
}

/// Serve the `current_repodata.json` (subset of `repodata.json` with latest
/// versions only).  Routed identically to `repodata.json` through the cache.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{platform}/current_repodata.json",
    tag = "proxy/conda",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("platform" = String, Path, description = "Platform string"),
    ),
    responses(
        (status = 200, description = "current_repodata.json", body = UpstreamDocument),
        (status = 404, description = "Not found"),
    ),
    security(("bearer_token" = [])),
)]
#[route(
    "/proxy/{registry}/{platform}/current_repodata.json",
    method = "GET",
    method = "HEAD"
)]
pub async fn conda_current_repodata(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, platform) = path.into_inner();
    require_registry_type(&registry, "conda", &map)?;

    if mode_map.get(&registry) == RegistryMode::Local {
        return Err(AppError::not_found(
            "current_repodata.json is not available for local-only conda registries".to_owned(),
        ));
    }

    let (body, synthesised) = fetch_conda_index(
        svc,
        &registry,
        &platform,
        identity,
        batlehub_core::ports::DocumentKind::CURRENT_REPODATA,
    )
    .await?;
    let mut builder = HttpResponse::Ok();
    builder.content_type("application/json");
    mark_synthesised(&mut builder, synthesised);
    Ok(builder.body(body))
}

/// Download a conda package file (`.conda` or `.tar.bz2`) through the proxy cache.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{platform}/{filename}",
    tag = "proxy/conda",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("platform" = String, Path, description = "Platform string"),
        ("filename" = String, Path, description = "Package filename"),
    ),
    responses(
        (status = 200, description = "Package bytes", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 404, description = "Package not found"),
    ),
    security(("bearer_token" = [])),
)]
// Regex constrains filename to .tar.bz2 and .conda extensions, preventing
// this route from shadowing the npm/cargo GET /proxy/{registry}/{name}/{version} handler.
#[get("/proxy/{registry}/{platform}/{filename:.+\\.(?:tar\\.bz2|conda)}")]
pub async fn conda_file_download(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    // `platform` is part of the route but not of the coordinate: a conda
    // package's identity is its name and version, and the same release is
    // served under several subdirs.
    let (registry, platform, filename) = path.into_inner();
    require_registry_type(&registry, "conda", &map)?;

    let mode = mode_map.get(&registry);

    if mode == RegistryMode::Local {
        // Look up by filename in index_metadata since package names may contain hyphens.
        let (name, version) = local_svc
            .find_conda_by_filename(&registry, &filename)
            .await
            .map_err(AppError::from)?
            .ok_or_else(|| AppError::not_found(format!("conda package not found: {filename}")))?;
        let bytes = local_svc
            .get_artifact(&registry, &name, &version, Action::ReleasesRead, &identity)
            .await
            .map_err(AppError::from)?;
        return Ok(HttpResponse::Ok()
            .content_type("application/octet-stream")
            .body(bytes));
    }

    if mode == RegistryMode::Hybrid {
        if let Some((name, version)) = local_svc
            .find_conda_by_filename(&registry, &filename)
            .await
            .map_err(AppError::from)?
        {
            match local_svc
                .get_artifact(&registry, &name, &version, Action::ReleasesRead, &identity)
                .await
            {
                Ok(bytes) => {
                    return Ok(HttpResponse::Ok()
                        .content_type("application/octet-stream")
                        .body(bytes));
                }
                Err(CoreError::NotFound(_)) => {}
                Err(e) => return Err(AppError::from(e)),
            }
        }
    }

    // Proxy through cache, addressed by the *package* coordinate the filename
    // encodes rather than by the filename stem and the platform.
    //
    // The coordinate is what the rule chain judges. Named
    // `("numpy-1.1.0-py311_0", "linux-64")` — as this route used to be — a block
    // recorded against `numpy@1.1.0` matches nothing and the download is
    // allowed, which would make conda the one ecosystem where hiding a version
    // from the channel index is the *only* half of a block that works. The
    // filename stays as the artifact sub-coordinate, so two builds of one
    // version keep distinct cache entries.
    let (name, version) = parse_conda_filename(&filename)
        .ok_or_else(|| AppError::bad_request(format!("unparseable conda filename: {filename}")))?;
    // The subdir travels in the selector: the coordinate stays the package's
    // name and version, which is what a block is placed on, and the client
    // reads the platform from the selector (`platform_and_file`).
    let pkg =
        PackageId::new(&registry, name, version).with_artifact(format!("{platform}/{filename}"));
    proxy_stream(
        svc,
        pkg,
        identity,
        Action::ReleasesRead,
        Some("application/octet-stream"),
    )
    .await
}

/// Split a conda filename into its `(name, version)`.
///
/// Conda filenames are `{name}-{version}-{build}.{tar.bz2,conda}` and **the name
/// may contain hyphens** (`ruamel-yaml-0.17.21-py311_0.conda`), so the split is
/// from the right: the last two fields are the build and the version, and
/// whatever precedes them is the name.
///
/// `None` for anything that does not have all three fields, which the caller
/// turns into a `400` rather than guessing at a coordinate the rule chain would
/// then judge.
fn parse_conda_filename(filename: &str) -> Option<(&str, &str)> {
    let stem = filename
        .strip_suffix(".conda")
        .or_else(|| filename.strip_suffix(".tar.bz2"))?;
    let (rest, _build) = stem.rsplit_once('-')?;
    let (name, version) = rest.rsplit_once('-')?;
    if name.is_empty() || version.is_empty() {
        return None;
    }
    Some((name, version))
}

/// Extract package name from a conda filename.
/// e.g. `numpy-1.26.0-py311h0_0.tar.bz2` → `numpy`
#[cfg(test)]
fn conda_package_name_from_filename(filename: &str) -> String {
    let stem = filename
        .strip_suffix(".conda")
        .or_else(|| filename.strip_suffix(".tar.bz2"))
        .unwrap_or(filename);
    // conda filename: {name}-{version}-{build}
    let parts: Vec<&str> = stem.splitn(3, '-').collect();
    parts[0].to_owned()
}

/// Extract "{version}-{build}" from a conda filename for use as a local registry version key.
/// e.g. `numpy-1.26.0-py311h0_0.tar.bz2` → `"1.26.0-py311h0_0"`
#[cfg(test)]
fn conda_version_from_filename(filename: &str) -> Option<String> {
    let stem = filename
        .strip_suffix(".conda")
        .or_else(|| filename.strip_suffix(".tar.bz2"))?;
    let mut parts = stem.splitn(3, '-');
    parts.next(); // skip name
    let version = parts.next()?;
    let build = parts.next().unwrap_or("");
    if build.is_empty() {
        Some(version.to_owned())
    } else {
        Some(format!("{version}-{build}"))
    }
}

// ── Publish route ─────────────────────────────────────────────────────────────

/// Publish a conda package (`.conda` or `.tar.bz2`) to a local/hybrid registry.
///
/// Accepts the raw package bytes as the request body.  The package name, version,
/// and build string are extracted from the `info/index.json` file inside the archive.
#[utoipa::path(
    post,
    path = "/proxy/{registry}/{platform}/",
    tag = "proxy/conda",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("platform" = String, Path, description = "Target platform, e.g. linux-64"),
    ),
    responses(
        (status = 200, description = "Package published", body = MessageResponse),
        (status = 400, description = "Malformed payload or signature headers"),
        (status = 403, description = "Access denied or quota exceeded"),
        (status = 409, description = "Version already published"),
        (status = 422, description = "Invalid conda package"),
    ),
    security(("bearer_token" = [])),
)]
#[allow(clippy::too_many_arguments)]
#[post("/proxy/{registry}/{platform}/")]
pub async fn conda_publish(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    payload: web::Payload,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let (registry, platform) = path.into_inner();
    require_registry_type(&registry, "conda", &map)?;
    require_local_mode(&registry, &mode_map)?;

    let data = collect_payload(payload).await?;

    let pkg_info = batlehub_adapters::registry::conda::parse_conda_metadata(&data)
        .map_err(|e| AppError::unprocessable(e.to_string()))?;

    let checksum = hex::encode(Sha256::digest(&data));

    // Build the filename for this package
    let ext = if data.len() >= 4 && &data[..4] == b"PK\x03\x04" {
        "conda"
    } else {
        "tar.bz2"
    };
    let filename = format!(
        "{}-{}-{}.{ext}",
        pkg_info.name, pkg_info.version, pkg_info.build
    );

    // version key = "{version}-{build}" to keep versions unique per build
    let version_key = format!("{}-{}", pkg_info.version, pkg_info.build);

    let index_metadata = serde_json::json!({
        "name": pkg_info.name,
        "version": pkg_info.version,
        "build": pkg_info.build,
        "build_number": pkg_info.build_number,
        "depends": pkg_info.depends,
        "subdir": pkg_info.subdir.unwrap_or_else(|| platform.clone()),
        "license": pkg_info.license,
        "sha256": checksum,
        "filename": filename,
    });

    let (signature_bytes, signature_type) =
        ArtifactSignature::split(extract_signature_headers(&req)?);

    super::common::publish_and_respond(
        &local_svc,
        &notification_svc,
        PublishRequest {
            unlisted: false,
            registry,
            name: pkg_info.name.clone(),
            version: version_key,
            artifact: data,
            checksum,
            index_metadata,
            publisher: identity.0,
            signature_bytes,
            signature_type,
        },
        actix_web::http::StatusCode::OK,
        MessageResponse::new(format!("Conda package published: {filename}")),
    )
    .await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_from_filename() {
        assert_eq!(
            conda_package_name_from_filename("numpy-1.26.0-py311h0_0.tar.bz2"),
            "numpy"
        );
        assert_eq!(
            conda_package_name_from_filename("bzip2-1.0.8-h5eee18b_5.conda"),
            "bzip2"
        );
    }

    #[test]
    fn version_from_filename() {
        assert_eq!(
            conda_version_from_filename("numpy-1.26.0-py311h0_0.tar.bz2"),
            Some("1.26.0-py311h0_0".to_owned())
        );
    }

    #[test]
    fn merge_repodata_combines_packages() {
        let upstream = serde_json::json!({
            "packages": { "pkgA-1.0-0.tar.bz2": { "name": "pkgA" } },
            "packages.conda": {}
        });
        let local = serde_json::json!({
            "packages": { "pkgB-1.0-0.tar.bz2": { "name": "pkgB" } },
            "packages.conda": {}
        });
        let upstream_bytes = serde_json::to_vec(&upstream).unwrap();
        let merged_bytes = merge_repodata(&upstream_bytes, &local);
        let merged: serde_json::Value = serde_json::from_slice(&merged_bytes).unwrap();
        assert!(merged["packages"].get("pkgA-1.0-0.tar.bz2").is_some());
        assert!(merged["packages"].get("pkgB-1.0-0.tar.bz2").is_some());
    }
}
