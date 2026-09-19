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
    req: HttpRequest,
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, platform) = path.into_inner();
    require_registry_type(&registry, "conda", &map)?;
    let mode = mode_map.get(&registry);

    // **This route has a cache again.** Moving it onto the byte path took its
    // document cache away with the parsed path: `multi_package_document` read
    // and wrote the `doc:` entry, and `multi_package_document_stream` goes
    // straight to the socket. The compressed routes kept theirs, so the
    // regression was asymmetric and invisible beside them — fifty clients
    // inside one TTL pulled the whole channel index fifty times.
    //
    // Proxy mode only, for the same reason as those routes: a local or hybrid
    // channel is built from the database on every request, and a key that sees
    // only the blocked set cannot see a publish. The fingerprint is a lookup of
    // its own, so it is not computed for a registry that cannot use it.
    let keys = if mode == RegistryMode::Proxy {
        let fingerprint = svc
            .blocked_snapshot_fingerprint(&registry, batlehub_core::entities::RegistryKind::Conda)
            .await;
        Some((
            format!("repodata-json:{registry}:{platform}:{fingerprint}"),
            index_storage_key(&registry, &platform, PLAIN_INDEX_LABEL, &fingerprint),
        ))
    } else {
        None
    };

    // Ahead of the probe, as on the compressed routes and for the reason stated
    // there: a warm entry answers a `HEAD` without leaving the process at all.
    if let Some((cache_key, storage_key)) = &keys {
        if let Ok(Some(entry)) = svc.cache.get(cache_key).await {
            let synthesised = entry
                .metadata
                .extra
                .get("synthesised")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);
            if let Ok(Some(stored)) = svc.storage.retrieve(storage_key).await {
                use futures::StreamExt;
                let mut builder = HttpResponse::Ok();
                builder.content_type("application/json");
                builder.insert_header(("X-BatleHub-Cache", "hit"));
                mark_synthesised(&mut builder, synthesised);
                let body = stored.stream.filter_map(|chunk| async move {
                    chunk.ok().map(Ok::<bytes::Bytes, actix_web::Error>)
                });
                return Ok(builder.streaming(body));
            }
        }
    }

    // A probe is answered with a probe. Without this, micromamba's `HEAD` of
    // every subdir pulled the whole index upstream and discarded it.
    if req.method() == actix_web::http::Method::HEAD {
        if let Some(answer) = probe_index(
            &svc,
            &registry,
            &platform,
            &identity,
            mode.clone(),
            batlehub_core::ports::DocumentKind::Versions,
            &[batlehub_core::ports::DocumentEncoding::Identity],
            "application/json",
        )
        .await?
        {
            return Ok(answer);
        }
    }

    // The whole channel, unparsed, when there is nothing to take out of it —
    // see `ProxyService::multi_package_document_stream`. Only `Identity` is
    // accepted here: this route's client asked for the uncompressed document
    // and gets it, rather than a `.zst` it never said it could read.
    //
    // An error here falls through to the parsed path rather than failing the
    // request: `repodata_bytes` is where `serve_stale_metadata` lives, and the
    // byte path taking precedence must not quietly cost this route its
    // stale-on-error behaviour. Nothing has been written to the client yet, so
    // the fall-through is free.
    let streamed = match streamed_index(
        &svc,
        &registry,
        &platform,
        &identity,
        mode.clone(),
        batlehub_core::ports::DocumentKind::Versions,
        &[batlehub_core::ports::DocumentEncoding::Identity],
    )
    .await
    {
        Ok(streamed) => streamed,
        Err(e) => {
            tracing::debug!(
                registry = %registry,
                platform = %platform,
                error = %e,
                "conda: the byte path failed; falling back to the parsed path, which can serve \
                 a stale index"
            );
            None
        }
    };

    match streamed {
        // Nothing to take out. Buffered up to `MAX_CACHEABLE_PLAIN_INDEX_BYTES`
        // so the answer can be stored on the way past; a channel bigger than
        // that goes socket-to-socket as before, because holding
        // conda-forge's 424 MiB `linux-64` index in memory to cache it would
        // cost more than the re-fetch it saves. Its clients read the `.zst`,
        // which has its own cache.
        Some(batlehub_core::services::StreamedIndex::AsIs(doc)) => {
            match buffer_or_stream(doc, MAX_CACHEABLE_PLAIN_INDEX_BYTES).await? {
                BufferedOrStream::Buffered(bytes) => {
                    if let Some((cache_key, storage_key)) = &keys {
                        store_plain_index(
                            &svc,
                            &registry,
                            &platform,
                            cache_key,
                            storage_key,
                            bytes.clone(),
                        )
                        .await;
                    }
                    return Ok(HttpResponse::Ok()
                        .content_type("application/json")
                        .insert_header(("X-BatleHub-Cache", "miss"))
                        .body(bytes));
                }
                BufferedOrStream::TooBig(stream) => {
                    return Ok(stream_to_client("application/json", stream))
                }
            }
        }
        // Something to take out: buffered, but the *compressed* form, and
        // filtered as it decodes rather than parsed into a `Value` first.
        Some(batlehub_core::services::StreamedIndex::Filter { doc, blocked }) => {
            return filtered_index_response(
                doc,
                blocked,
                None,
                "application/json",
                "the channel index",
            )
            .await
        }
        None => {}
    }

    let (body, synthesised) = repodata_bytes(
        svc,
        local_svc,
        &registry,
        &platform,
        identity,
        mode,
        batlehub_core::ports::DocumentKind::Versions,
    )
    .await?;

    let mut builder = HttpResponse::Ok();
    builder.content_type("application/json");
    mark_synthesised(&mut builder, synthesised);
    Ok(builder.body(body))
}

/// Try the byte path: the upstream index, streamed, when nothing has to be taken
/// out of it.
///
/// `None` means the request needs the parsed path — something is blocked, the
/// registry is local or hybrid (the local half has to be merged in), or the
/// channel does not publish the encoding asked for.
///
/// Proxy mode only. A local channel is built from the database on every request
/// and there is no upstream to stream; a hybrid one has to merge, and a merge
/// needs both documents in hand.
async fn streamed_index(
    svc: &web::Data<Arc<ProxyService>>,
    registry: &str,
    platform: &str,
    identity: &AuthIdentity,
    mode: RegistryMode,
    kind: batlehub_core::ports::DocumentKind,
    accept: &[batlehub_core::ports::DocumentEncoding],
) -> Result<Option<batlehub_core::services::StreamedIndex>, AppError> {
    if mode != RegistryMode::Proxy {
        return Ok(None);
    }
    let req = batlehub_core::services::ProxyRequest {
        package_id: PackageId::new(registry, platform, "__repodata__"),
        identity: identity.0.clone(),
        action: Action::ReleasesRead.to_owned(),
        ip_address: identity.1.ip.clone(),
        user_agent: identity.1.user_agent.clone(),
    };
    svc.multi_package_document_stream(&req, kind, accept)
        .await
        .map_err(AppError::from)
}

/// Answer a `HEAD` by asking the upstream the same question.
///
/// `Ok(None)` means "no cheap answer" and the caller carries on to the body
/// path, which is what every non-conda kind gets. conda clients probe before
/// they fetch, so this is the difference between a `HEAD` costing a round trip
/// and a `HEAD` costing 57 MiB.
///
/// The length and validators are whatever the service could promise — it drops
/// them for a registry that filters, since the filtered index is not the
/// document upstream described.
#[allow(clippy::too_many_arguments)]
async fn probe_index(
    svc: &web::Data<Arc<ProxyService>>,
    registry: &str,
    platform: &str,
    identity: &AuthIdentity,
    mode: RegistryMode,
    kind: batlehub_core::ports::DocumentKind,
    accept: &[batlehub_core::ports::DocumentEncoding],
    content_type: &str,
) -> Result<Option<HttpResponse>, AppError> {
    if mode != RegistryMode::Proxy {
        return Ok(None);
    }
    let req = batlehub_core::services::ProxyRequest {
        package_id: PackageId::new(registry, platform, "__repodata__"),
        identity: identity.0.clone(),
        action: Action::ReleasesRead.to_owned(),
        ip_address: identity.1.ip.clone(),
        user_agent: identity.1.user_agent.clone(),
    };
    let Some(probe) = svc
        .multi_package_document_probe(&req, kind, accept)
        .await
        .map_err(AppError::from)?
    else {
        return Ok(None);
    };

    // Whatever the probe carries is relayed; the service has already dropped the
    // fields it cannot promise for a registry that filters.
    let mut builder = HttpResponse::Ok();
    builder.content_type(content_type);
    if let Some(len) = probe.content_length {
        builder.insert_header((actix_web::http::header::CONTENT_LENGTH, len.to_string()));
    }
    if let Some(etag) = probe.etag {
        builder.insert_header((actix_web::http::header::ETAG, etag));
    }
    if let Some(modified) = probe.last_modified {
        builder.insert_header((actix_web::http::header::LAST_MODIFIED, modified));
    }
    Ok(Some(builder.finish()))
}

// ── CEP-16: the sharded index ────────────────────────────────────────────────
//
// A conda channel publishes its index twice. `repodata.json` is every package
// record in the subdir — 424 MiB for `conda-forge/linux-64` — and
// `repodata_shards.msgpack.zst` is a **568 KB** map of package name to the
// sha256 of that package's own shard, each shard a few tens of kilobytes at
// `{subdir}/{sha256}.msgpack.zst`. A client that speaks CEP-16 fetches the map
// and then only the shards it needs, which for a typical install is under a
// megabyte against 55 MiB compressed.
//
// micromamba asks for the sharded index **first**, before anything else. Until
// these two routes existed it got a `404` and fell back to the monolith, which
// is how one `conda install` came to cost this proxy several gigabytes.
//
// Two things make shards unusually good here rather than merely smaller:
// they are **content-addressed**, so a shard is immutable and cacheable
// forever with no TTL and no revalidation; and a shard is one package's
// records, so what a client reads is scoped to what it asked about.

/// The shard index's content type, as the channel serves it.
const SHARDS_CONTENT_TYPE: &str = "application/octet-stream";

/// The most this proxy will decompress of a shard index to inspect it.
///
/// The index is ~2–5 MiB decompressed; the bound is what stops a channel from
/// answering with a zstd bomb on a route that has to look inside.
const MAX_SHARD_INDEX_BYTES: usize = 64 * 1024 * 1024;

/// Whether a shard index is safe to relay: it must route the client back here.
///
/// `info.base_url` and `info.shards_base_url` are resolved **relative to the
/// index's own URL** when empty, which is what conda-forge publishes and what
/// makes a pass-through correct — the client resolves them against *our* URL
/// and comes back to us for every shard and every package.
///
/// A channel that sets either to an absolute URL sends the client somewhere
/// else, past the rules, the cache and the audit trail. That is the same hole
/// RFC 0009 §12 records against Terraform and the one the Open VSX
/// `files.download` bug re-opened, so it is checked rather than assumed.
///
/// The check is deliberately blunt: **any** absolute URL anywhere in the
/// decompressed index disqualifies it. Reading msgpack properly would mean
/// parsing an untrusted document to decide whether to trust it, for a
/// conservative answer this already gives — and being wrong costs a fallback to
/// `repodata.json`, never a leak.
fn shard_index_routes_here(compressed: &[u8]) -> bool {
    use std::io::Read;
    let mut decoded = Vec::new();
    let Ok(decoder) = zstd::Decoder::new(compressed) else {
        return false;
    };
    // One byte past the bound, so the limit can be *detected* rather than
    // silently applied. `Take::read_to_end` stops at its limit and returns
    // `Ok`, so reading exactly `MAX_SHARD_INDEX_BYTES` would scan a prefix of
    // a document this function then declares safe to relay in full — the bomb
    // it exists to refuse would pass by being large enough.
    if decoder
        .take(MAX_SHARD_INDEX_BYTES as u64 + 1)
        .read_to_end(&mut decoded)
        .is_err()
    {
        return false;
    }
    if decoded.len() > MAX_SHARD_INDEX_BYTES {
        return false;
    }
    // Both schemes case-insensitively, and each against a window its own
    // length. `windows(7)` with `starts_with(b"https:/")` is an equality test
    // on a 7-byte slice, so it matched only lowercase — and `HTTPS://host/`
    // in `info.base_url` routed every shard and package fetch off this proxy,
    // which is the one thing this function exists to prevent.
    let absolute = decoded
        .windows(7)
        .any(|w| w.eq_ignore_ascii_case(b"http://"))
        || decoded
            .windows(8)
            .any(|w| w.eq_ignore_ascii_case(b"https://"));
    !absolute
}

/// `repodata_shards.msgpack.zst` — CEP-16's index of shards.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{platform}/repodata_shards.msgpack.zst",
    tag = "proxy/conda",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("platform" = String, Path, description = "Platform string, e.g. linux-64 or noarch"),
    ),
    responses(
        (status = 200, description = "The shard index, as the channel published it", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "The channel publishes no shard index, or this registry cannot serve one"),
    ),
    security(("bearer_token" = [])),
)]
#[route(
    "/proxy/{registry}/{platform}/repodata_shards.msgpack.zst",
    method = "GET",
    method = "HEAD"
)]
pub async fn conda_repodata_shards(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, platform) = path.into_inner();
    require_registry_type(&registry, "conda", &map)?;
    let mode = mode_map.get(&registry);
    let kind = batlehub_core::ports::DocumentKind::REPODATA_SHARDS;
    let accept = [batlehub_core::ports::DocumentEncoding::Zstd];

    let absent = || AppError::not_found("this channel serves no sharded index here");

    if req.method() == actix_web::http::Method::HEAD {
        if let Some(answer) = probe_index(
            &svc,
            &registry,
            &platform,
            &identity,
            mode.clone(),
            kind,
            &accept,
            SHARDS_CONTENT_TYPE,
        )
        .await?
        {
            return Ok(answer);
        }
        return Err(absent());
    }

    match streamed_index(&svc, &registry, &platform, &identity, mode, kind, &accept).await? {
        Some(batlehub_core::services::StreamedIndex::AsIs(doc)) => {
            // Buffered rather than streamed, because it has to be looked at
            // before it is relayed — 568 KB, once per TTL.
            let bytes = collect_index(doc.stream, "the shard index").await?;
            if !shard_index_routes_here(&bytes) {
                tracing::warn!(
                    registry = %registry,
                    platform = %platform,
                    "this channel's shard index carries absolute URLs, which would send clients \
                     past this proxy — serving repodata.json instead"
                );
                return Err(absent());
            }
            Ok(HttpResponse::Ok()
                .content_type(SHARDS_CONTENT_TYPE)
                .insert_header(("X-BatleHub-Cache", "stream"))
                .body(bytes))
        }
        // Nothing here can take a package out of a msgpack index, and serving
        // an unfiltered one would hand a client the records of a package this
        // registry blocks. So: no shards, and the client falls back to
        // `repodata.json`, which *is* filtered.
        Some(batlehub_core::services::StreamedIndex::Filter { .. }) => {
            tracing::info!(
                registry = %registry,
                "not serving a sharded index for a registry with blocked packages; \
                 clients will use repodata.json, which is filtered"
            );
            Err(absent())
        }
        None => Err(absent()),
    }
}

/// One shard — `{sha256}.msgpack.zst`, one package's records.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{platform}/{shard}.msgpack.zst",
    tag = "proxy/conda",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("platform" = String, Path, description = "Platform string"),
        ("shard" = String, Path, description = "The shard's sha256, as the index named it"),
    ),
    responses(
        (status = 200, description = "Shard bytes", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "No such shard, or this registry serves no shards"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/{platform}/{shard:[0-9a-fA-F]+}.msgpack.zst")]
pub async fn conda_shard(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, platform, shard) = path.into_inner();
    require_registry_type(&registry, "conda", &map)?;

    // The route's pattern already excludes anything but hex; the length is what
    // says this is a sha256 rather than a file that happens to be hex-named.
    if shard.len() != 64 {
        return Err(AppError::not_found("not a shard"));
    }
    if mode_map.get(&registry) != RegistryMode::Proxy {
        return Err(AppError::not_found("this registry serves no shards"));
    }
    // A shard is only coherent with an index this proxy served, and a registry
    // with blocks serves none — so a shard asked for here came from an index
    // this proxy did not give out.
    if svc
        .registry_blocks_anything(&registry, batlehub_core::entities::RegistryKind::Conda)
        .await
    {
        return Err(AppError::not_found("this registry serves no shards"));
    }

    // Content-addressed, so the coordinate *is* the hash and the bytes under it
    // never change: cached once, correct forever. `__shards__` is a synthetic
    // package name in the same spirit as `__repodata__` — a shard belongs to the
    // subdir, not to a package this registry could block.
    let pkg = PackageId::new(&registry, "__shards__", &shard)
        .with_artifact(format!("{platform}/{shard}.msgpack.zst"));
    proxy_stream(
        svc,
        pkg,
        identity,
        Action::ReleasesRead,
        Some(SHARDS_CONTENT_TYPE),
    )
    .await
}

/// Where one encoding of one subdir's index is stored, for one blocked set.
///
/// The fingerprint is part of the key because the filtered document changes with
/// the blocked set, exactly as it is part of the cache key beside it.
/// `encoding` is the label the *route* stores under — the two compressed
/// encodings' own suffixes, and `json` for the plain route — and it sits
/// **above** the fingerprint in the key so that [`index_storage_prefix`] can
/// sweep one encoding's stale fingerprints without touching another's.
fn index_storage_key(registry: &str, platform: &str, encoding: &str, fingerprint: &str) -> String {
    format!(
        "index/{registry}/{platform}/{encoding}/{fingerprint}/{}",
        index_file_name(encoding)
    )
}

/// The file name one encoding's index is stored under — the channel's own
/// spelling of it, `repodata.json` plus the compression extension.
///
/// So that a blob's name describes its bytes. The encoding is already a
/// directory segment above, and naming the file for it too is mild
/// duplication against a real trap: the filesystem backend writes these as
/// actual files, and an object called `repodata.json` holding zstd misleads
/// anyone reading the storage backend directly.
fn index_file_name(encoding: &str) -> String {
    if encoding == PLAIN_INDEX_LABEL {
        "repodata.json".to_owned()
    } else {
        format!("repodata.json.{encoding}")
    }
}

/// Every fingerprint stored for one subdir **in one encoding**.
///
/// Scoped to the encoding because the writer below sweeps this prefix before it
/// stores: a subdir-wide prefix also matched the *current* fingerprint's other
/// encoding, so a channel read alternately as `.zst` and `.bz2` had each write
/// delete the other's blob and neither route ever served a cache hit.
fn index_storage_prefix(registry: &str, platform: &str, encoding: &str) -> String {
    format!("index/{registry}/{platform}/{encoding}/")
}

/// The largest channel index this proxy will hold in memory to filter.
///
/// Only the *filtering* path buffers at all — the pass-through streams — and it
/// buffers the compressed form, which is 55 MiB for conda-forge's biggest
/// subdir. 512 MiB is far above that and still a bound, so a channel that grows
/// past anything reasonable fails with a sentence rather than with the OOM
/// killer.
const MAX_FILTERABLE_INDEX_BYTES: usize = 512 * 1024 * 1024;

/// The storage label the plain `repodata.json` route caches under.
///
/// A label rather than an [`Encoding`], because that enum is the *compressed*
/// routes' dispatch and the identity encoding is not one of its arms.
const PLAIN_INDEX_LABEL: &str = "json";

/// The largest uncompressed index this proxy will hold in order to cache it.
///
/// Well under [`MAX_FILTERABLE_INDEX_BYTES`] on purpose. Caching means holding
/// the whole document, and for conda-forge's `linux-64` that is 424 MiB per
/// concurrent request — more than the upstream re-fetch it would save. Above
/// this bound the route streams socket-to-socket as it did before, which is the
/// case the byte path was built for; below it — every channel that is not
/// conda-forge — the answer is stored and the next reader gets it for free.
const MAX_CACHEABLE_PLAIN_INDEX_BYTES: usize = 64 * 1024 * 1024;

/// A document small enough to hold, or the same document as a stream again.
enum BufferedOrStream {
    Buffered(bytes::Bytes),
    TooBig(batlehub_core::ports::ArtifactStream),
}

/// Read up to `limit` bytes of `doc`; hand back the whole thing if it fits and
/// an equivalent stream if it does not.
///
/// The bytes already read are put back in front of the remainder, so the
/// caller's response is byte-identical either way — the difference is only
/// whether the answer could also be stored.
async fn buffer_or_stream(
    doc: batlehub_core::ports::StreamedDocument,
    limit: usize,
) -> Result<BufferedOrStream, AppError> {
    use futures::StreamExt;
    let mut stream = doc.stream;
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(AppError::from)?;
        buf.extend_from_slice(&chunk);
        if buf.len() > limit {
            let head = futures::stream::once(async move {
                Ok::<bytes::Bytes, CoreError>(bytes::Bytes::from(buf))
            });
            return Ok(BufferedOrStream::TooBig(Box::pin(head.chain(stream))));
        }
    }
    Ok(BufferedOrStream::Buffered(bytes::Bytes::from(buf)))
}

/// Store one plain index and point the cache entry at it.
///
/// Best-effort throughout, exactly as the compressed routes are: the bytes are
/// already in hand, so neither a failed store nor a failed cache write is a
/// failed request.
async fn store_plain_index(
    svc: &web::Data<Arc<ProxyService>>,
    registry: &str,
    platform: &str,
    cache_key: &str,
    storage_key: &str,
    body: bytes::Bytes,
) {
    // Every other fingerprint of this subdir *in this encoding* is stale by
    // construction — the fingerprint is the blocked set — so the old ones go.
    // Scoped to the encoding, so this does not delete the `.zst` blob the
    // compressed route just wrote for the same fingerprint.
    let prefix = index_storage_prefix(registry, platform, PLAIN_INDEX_LABEL);
    if let Err(e) = svc.storage.delete_by_prefix(&prefix).await {
        tracing::debug!(prefix = %prefix, error = %e, "sweeping stale channel indexes failed");
    }
    let meta = batlehub_core::ports::StorageMeta {
        content_type: Some("application/json".to_owned()),
        size: Some(body.len() as u64),
        checksum: None,
    };
    if let Err(e) = svc.storage.store(storage_key, body, meta).await {
        tracing::warn!(key = %storage_key, error = %e, "storing the channel index failed");
        return;
    }
    let entry = batlehub_core::ports::CacheEntry {
        metadata: batlehub_core::entities::PackageMetadata::minimal(
            PackageId::new(registry, platform, "__repodata__"),
            serde_json::json!({ "storage_key": storage_key, "synthesised": null }),
        ),
        cached_at: chrono::Utc::now(),
        expires_at: None,
    };
    if let Err(e) = svc
        .cache
        .set(cache_key, entry, Some(COMPRESSED_REPODATA_TTL))
        .await
    {
        tracing::warn!(key = %cache_key, error = %e, "caching the channel index failed");
    }
}

/// Collect a streamed document, refusing one past the bound.
async fn collect_index(
    stream: batlehub_core::ports::ArtifactStream,
    what: &str,
) -> Result<Vec<u8>, AppError> {
    use futures::StreamExt;
    let mut stream = stream;
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(AppError::from)?;
        if buf.len() + chunk.len() > MAX_FILTERABLE_INDEX_BYTES {
            return Err(AppError::bad_gateway(format!(
                "{what} is larger than {} MiB, which is the most this proxy will hold in memory \
                 to filter blocked packages out of it",
                MAX_FILTERABLE_INDEX_BYTES / (1024 * 1024)
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Decode, filter, re-encode — the blocked case, off the async runtime.
///
/// One package entry is in memory at a time
/// (`blocking::conda_stream::filter_repodata`), so what this costs is the input
/// buffer plus the output buffer rather than a `serde_json::Value` of the whole
/// channel. CPU work on hundreds of megabytes, hence `web::block`.
fn filter_index(
    input: Vec<u8>,
    from: batlehub_core::ports::DocumentEncoding,
    to: Option<Encoding>,
    blocked: &batlehub_core::services::blocking::MultiPackageBlocks,
) -> Result<Vec<u8>, AppError> {
    use batlehub_core::ports::DocumentEncoding as In;
    use batlehub_core::services::blocking::conda_stream::filter_repodata;
    use std::io::Cursor;

    let bad = |e: std::io::Error| AppError::internal(format!("filtering the channel index: {e}"));
    let reader: Box<dyn std::io::Read> = match from {
        In::Identity => Box::new(Cursor::new(input)),
        In::Zstd => Box::new(zstd::Decoder::new(Cursor::new(input)).map_err(bad)?),
        In::Bzip2 => Box::new(bzip2::read::BzDecoder::new(Cursor::new(input))),
    };
    let oops = |e: serde_json::Error| {
        AppError::bad_gateway(format!("the channel index could not be filtered: {e}"))
    };

    match to {
        None => {
            let mut out = Vec::new();
            filter_repodata(reader, &mut out, blocked).map_err(oops)?;
            Ok(out)
        }
        Some(Encoding::Zstd) => {
            let mut enc = zstd::Encoder::new(Vec::new(), 3).map_err(bad)?;
            filter_repodata(reader, &mut enc, blocked).map_err(oops)?;
            enc.finish().map_err(bad)
        }
        Some(Encoding::Bzip2) => {
            let enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
            let mut enc = enc;
            filter_repodata(reader, &mut enc, blocked).map_err(oops)?;
            enc.finish().map_err(bad)
        }
    }
}

/// The blocked half of the byte path: buffer, filter as it decodes, answer.
async fn filtered_index_response(
    doc: batlehub_core::ports::StreamedDocument,
    blocked: batlehub_core::services::blocking::MultiPackageBlocks,
    to: Option<Encoding>,
    content_type: &str,
    what: &str,
) -> Result<HttpResponse, AppError> {
    let from = doc.encoding;
    let input = collect_index(doc.stream, what).await?;
    let out = web::block(move || filter_index(input, from, to, &blocked))
        .await
        .map_err(|e| AppError::internal(format!("filtering the channel index: {e}")))??;
    Ok(HttpResponse::Ok()
        .content_type(content_type)
        .insert_header(("X-BatleHub-Cache", "filtered"))
        .body(out))
}

/// Stream a document straight to the client, chunk by chunk.
///
/// Takes the stream rather than the `StreamedDocument` it came from: the plain
/// route reaches here having already read a bounded prefix to decide whether
/// the answer was small enough to cache, and hands back the prefix and the
/// remainder rejoined.
fn stream_to_client(
    content_type: &str,
    stream: batlehub_core::ports::ArtifactStream,
) -> HttpResponse {
    use futures::StreamExt;
    let body = stream
        .filter_map(|chunk| async move { chunk.ok().map(Ok::<bytes::Bytes, actix_web::Error>) });
    HttpResponse::Ok()
        .content_type(content_type)
        .insert_header(("X-BatleHub-Cache", "stream"))
        .streaming(body)
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
        ip_address: identity.1.ip.clone(),
        user_agent: identity.1.user_agent.clone(),
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
    req: HttpRequest,
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, platform) = path.into_inner();
    serve_compressed_repodata(
        req,
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
    req: HttpRequest,
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, platform) = path.into_inner();
    serve_compressed_repodata(
        req,
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
    req: HttpRequest,
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

    // A probe is answered with a probe — but only past the cache, so a warm
    // entry still answers a `HEAD` without leaving the process at all.
    let probing = req.method() == actix_web::http::Method::HEAD;

    let fingerprint = svc
        .blocked_snapshot_fingerprint(&registry, batlehub_core::entities::RegistryKind::Conda)
        .await;
    let cache_key = format!(
        "repodata-{}:{registry}:{platform}:{fingerprint}",
        encoding.suffix()
    );

    // The bytes live in the **storage backend**; the cache entry is a pointer to
    // them plus the freshness the TTL enforces.
    //
    // They used to live in the cache entry itself, base64-encoded inside a
    // `serde_json::Value`. For `conda-forge/linux-64` that is a 57 MiB payload
    // turned into a 77 MiB string, a JSON document built around it and a
    // serialisation of the whole thing — per write, and again per read to decode
    // it. Bytes belong where bytes go, and a hit now *streams* out of storage
    // instead of being decoded into memory first.
    let storage_key = index_storage_key(&registry, &platform, encoding.suffix(), &fingerprint);
    if cacheable {
        if let Ok(Some(entry)) = svc.cache.get(&cache_key).await {
            // A composed repodata cached in its encoding is still composed:
            // the flag is stored beside the pointer.
            let synthesised = entry
                .metadata
                .extra
                .get("synthesised")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);
            // The entry says the bytes were stored; storage says whether they
            // still are. A key that has been swept is a miss, not an error.
            if let Ok(Some(stored)) = svc.storage.retrieve(&storage_key).await {
                use futures::StreamExt;
                let mut builder = HttpResponse::Ok();
                builder.content_type(encoding.content_type());
                builder.insert_header(("X-BatleHub-Cache", "hit"));
                mark_synthesised(&mut builder, synthesised);
                let body = stored.stream.filter_map(|chunk| async move {
                    chunk.ok().map(Ok::<bytes::Bytes, actix_web::Error>)
                });
                return Ok(builder.streaming(body));
            }
        }
    }

    // The channel's own compressed file, when there is nothing to take out of
    // it. This is the case that matters: `conda-forge/linux-64` publishes
    // `repodata.json.zst` at 55 MiB against the 424 MiB it decompresses to, and
    // building the uncompressed document in order to compress it again is how
    // this request came to cost ~11 GB and time out. Accepting *only* this
    // route's own encoding keeps the answer honest — a `.bz2` request is not
    // served zstd bytes.
    let accepted = match encoding {
        Encoding::Zstd => batlehub_core::ports::DocumentEncoding::Zstd,
        Encoding::Bzip2 => batlehub_core::ports::DocumentEncoding::Bzip2,
    };
    if probing {
        if let Some(answer) = probe_index(
            &svc,
            &registry,
            &platform,
            &identity,
            mode.clone(),
            batlehub_core::ports::DocumentKind::Versions,
            &[accepted],
            encoding.content_type(),
        )
        .await?
        {
            return Ok(answer);
        }
    }

    let streamed = streamed_index(
        &svc,
        &registry,
        &platform,
        &identity,
        mode.clone(),
        batlehub_core::ports::DocumentKind::Versions,
        &[accepted],
    )
    .await?;

    // Buffered rather than streamed on this route, deliberately: the compressed
    // form is 55 MiB where the document is 424 MiB, and holding it is what lets
    // the cache below keep working — a streamed answer would be a cache that
    // never fills and an upstream fetch on every request.
    let (compressed, synthesised) = match streamed {
        Some(batlehub_core::services::StreamedIndex::AsIs(doc)) => {
            (collect_index(doc.stream, "the channel index").await?, None)
        }
        Some(batlehub_core::services::StreamedIndex::Filter { doc, blocked }) => {
            let from = doc.encoding;
            let input = collect_index(doc.stream, "the channel index").await?;
            let out = web::block(move || filter_index(input, from, Some(encoding), &blocked))
                .await
                .map_err(|e| AppError::internal(format!("filtering the channel index: {e}")))??;
            (out, None)
        }
        // The parsed path: local, hybrid, or a channel that publishes no
        // compressed index of its own. Filter and hybrid merge included — so
        // the two encodings cannot describe a different channel from the plain
        // one.
        None => {
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
            (encoding.compress(&raw)?, synthesised)
        }
    };

    // `Bytes` so handing the same buffer to storage and to the client is a
    // refcount rather than a second 57 MiB copy.
    let body = bytes::Bytes::from(compressed);

    if cacheable {
        // Every other fingerprint of this subdir is stale by construction — the
        // fingerprint *is* the blocked set — so the old ones go. Worst case for
        // a concurrent reader is a cache miss and a re-fetch.
        let prefix = index_storage_prefix(&registry, &platform, encoding.suffix());
        if let Err(e) = svc.storage.delete_by_prefix(&prefix).await {
            tracing::debug!(prefix = %prefix, error = %e, "sweeping stale channel indexes failed");
        }
        let meta = batlehub_core::ports::StorageMeta {
            content_type: Some(encoding.content_type().to_owned()),
            size: Some(body.len() as u64),
            checksum: None,
        };
        match svc.storage.store(&storage_key, body.clone(), meta).await {
            Ok(()) => {
                let entry = batlehub_core::ports::CacheEntry {
                    metadata: batlehub_core::entities::PackageMetadata::minimal(
                        PackageId::new(&registry, &platform, "__repodata__"),
                        serde_json::json!({
                            "storage_key": storage_key,
                            "synthesised": synthesised,
                        }),
                    ),
                    cached_at: chrono::Utc::now(),
                    expires_at: None,
                };
                if let Err(e) = svc
                    .cache
                    .set(&cache_key, entry, Some(COMPRESSED_REPODATA_TTL))
                    .await
                {
                    tracing::warn!(key = %cache_key, error = %e, "caching the channel index failed");
                }
            }
            // Storing is an optimisation; failing to store is not a failed
            // request. The bytes are already in hand.
            Err(e) => {
                tracing::warn!(key = %storage_key, error = %e, "storing the channel index failed")
            }
        }
    }

    let mut builder = HttpResponse::Ok();
    builder.content_type(encoding.content_type());
    builder.insert_header(("X-BatleHub-Cache", "miss"));
    mark_synthesised(&mut builder, synthesised);
    Ok(builder.body(body))
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
            .get_artifact(
                &registry,
                &name,
                &version,
                Action::ReleasesRead,
                &identity,
                &identity.1,
            )
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
                .get_artifact(
                    &registry,
                    &name,
                    &version,
                    Action::ReleasesRead,
                    &identity,
                    &identity.1,
                )
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

    fn zstd(payload: &[u8]) -> Vec<u8> {
        zstd::encode_all(payload, 3).unwrap()
    }

    /// The shape conda-forge publishes: every URL in the index is relative, so
    /// the client resolves every shard and every package against the URL it
    /// asked *us* for, and comes back here.
    #[test]
    fn a_relative_shard_index_may_be_relayed() {
        // Not real msgpack — the guard reads bytes, not structure, which is the
        // point of it.
        let index = b"\x83\xa4infoshards_base_url\xa0base_url\xa0linux-64";
        assert!(shard_index_routes_here(&zstd(index)));
    }

    /// A channel that names an absolute host sends clients past this proxy —
    /// past the rules, the cache and the audit trail. It is refused, and the
    /// client falls back to `repodata.json`, which this proxy does filter.
    #[test]
    fn an_absolute_shard_base_url_is_refused() {
        for hostile in [
            &b"shards_base_url https://fast.prefix.dev/conda-forge/linux-64/"[..],
            &b"base_url HTTP://cdn.example/pkgs/"[..],
            &b"http://plain-http.example/"[..],
            // The scheme is case-insensitive in the URL grammar, and the guard
            // used to compare `https:/` as a 7-byte equality — so only the
            // lowercase spelling was caught and a channel could route every
            // shard and package off this proxy by shouting.
            &b"base_url HTTPS://cdn.attacker.example/"[..],
            &b"base_url HttPs://cdn.attacker.example/"[..],
            &b"base_url hTTp://cdn.attacker.example/"[..],
        ] {
            assert!(
                !shard_index_routes_here(&zstd(hostile)),
                "should have been refused: {}",
                String::from_utf8_lossy(hostile)
            );
        }
    }

    /// Bytes that are not zstd at all, and a document that decompresses past the
    /// bound, are both "cannot vouch for this" rather than "fine".
    #[test]
    fn an_unreadable_shard_index_is_refused() {
        assert!(!shard_index_routes_here(b"not zstd at all"));
        assert!(!shard_index_routes_here(&[]));
    }

    /// The bound has to *refuse*, not truncate. `Take::read_to_end` stops at
    /// its limit and returns `Ok`, so scanning exactly `MAX_SHARD_INDEX_BYTES`
    /// meant a document larger than that was vetted on its prefix and then
    /// relayed whole — an absolute URL past the bound went unseen.
    ///
    /// Compresses to a few hundred bytes, so this costs nothing to run.
    #[test]
    fn a_shard_index_past_the_bound_is_refused_rather_than_truncated() {
        let mut oversized = vec![b'x'; MAX_SHARD_INDEX_BYTES + 1];
        // Past the bound, where a truncating scan would never look.
        oversized.extend_from_slice(b"base_url https://cdn.attacker.example/");
        assert!(
            !shard_index_routes_here(&zstd(&oversized)),
            "a document too large to vet must not be vouched for"
        );

        // And one byte under it is still read and judged on its contents.
        let mut just_inside = vec![b'x'; MAX_SHARD_INDEX_BYTES - 64];
        just_inside.extend_from_slice(b"base_url https://cdn.attacker.example/");
        assert!(!shard_index_routes_here(&zstd(&just_inside)));
    }

    /// **The sweep must not delete the other encoding's blob.**
    ///
    /// The writer deletes `index_storage_prefix` before it stores, and the
    /// prefix used to be the whole subdir — which also matched the *current*
    /// fingerprint's other encoding. A channel read alternately as `.zst` and
    /// `.bz2` therefore had each write evict the other's bytes, and neither
    /// route ever served a cache hit. Putting the encoding above the
    /// fingerprint in the key is what scopes the sweep.
    #[test]
    fn the_stale_sweep_is_scoped_to_one_encoding() {
        let zst = index_storage_key("chan", "linux-64", Encoding::Zstd.suffix(), "fp1");
        let bz2 = index_storage_key("chan", "linux-64", Encoding::Bzip2.suffix(), "fp1");
        let json = index_storage_key("chan", "linux-64", PLAIN_INDEX_LABEL, "fp1");
        let zst_stale = index_storage_key("chan", "linux-64", Encoding::Zstd.suffix(), "fp0");

        let sweep = index_storage_prefix("chan", "linux-64", Encoding::Zstd.suffix());

        assert!(zst.starts_with(&sweep), "{zst} vs {sweep}");
        assert!(
            zst_stale.starts_with(&sweep),
            "a stale fingerprint of the same encoding is what the sweep is for"
        );
        assert!(
            !bz2.starts_with(&sweep),
            "the bz2 blob for the same fingerprint must survive: {bz2}"
        );
        assert!(!json.starts_with(&sweep), "so must the plain one: {json}");

        // And every encoding still lives under the subdir, so a registry-wide
        // sweep elsewhere still finds all of them.
        for key in [&zst, &bz2, &json] {
            assert!(key.starts_with("index/chan/linux-64/"), "{key}");
        }

        // The blob is named for what is in it. The filesystem backend writes
        // these as real files, so an object called `repodata.json` holding
        // zstd would mislead anyone reading storage directly.
        assert!(zst.ends_with("/repodata.json.zst"), "{zst}");
        assert!(bz2.ends_with("/repodata.json.bz2"), "{bz2}");
        assert!(json.ends_with("/repodata.json"), "{json}");
    }

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
