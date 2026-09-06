//! The air gap on the wire (RFC 0008 §6.4): the catch-all sink, and the
//! record an operator reads to build the next bundle.
//!
//! # The sink
//!
//! `mise plan --emit-mise-toml` appends a final rewrite rule mapping anything
//! not already covered onto `/_air-gap/unmirrored/{host}/…`. This route is
//! where those land. It **never fetches anything** — there is no HTTP client
//! in scope, so it cannot become an SSRF surface — and it answers `501` while
//! recording the host. That turns "a host nobody predicted" from a connect
//! timeout on a disconnected workstation into a line in the console.
//!
//! # The record
//!
//! `GET /api/v1/admin/air-gap/missing` is the list, most-asked first, and
//! `DELETE` purges it. Both are `system:read` / `system:write`: the miss log
//! describes the *instance*, not a package, and it is the same reader who
//! looks at the config and the health page.

use std::sync::Arc;

use actix_web::{delete, get, post, web, HttpResponse, Responder};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use batlehub_core::{
    entities::{Action, BundleImport, ContentMiss, MissFilter, MissKind, RecordedMiss},
    ports::MissRecorder,
    services::ProxyService,
};

use crate::{error::AppError, extractors::AuthIdentity};

/// The first path segment the catch-all rule carries is the host, so the
/// recorder never has to parse the tail as a URL.
fn host_of(tail: &str) -> &str {
    tail.split('/').next().unwrap_or(tail)
}

/// The sink for a host nothing rewrites.
#[utoipa::path(
    get,
    path = "/_air-gap/unmirrored/{tail}",
    tag = "air-gap",
    params(("tail" = String, Path, description = "`{host}/{rest of the original URL}`")),
    responses(
        (status = 501, description = "Recorded; this host has no mirror on this instance"),
    ),
)]
#[get("/_air-gap/unmirrored/{tail:.*}")]
pub async fn unmirrored_sink(
    path: web::Path<String>,
    svc: web::Data<Arc<ProxyService>>,
) -> Result<HttpResponse, AppError> {
    let tail = path.into_inner();
    let host = host_of(&tail).to_owned();
    let (record, recorder) = {
        let hot = svc.hot.read().await;
        (hot.air_gap.record_misses, hot.miss_recorder.clone())
    };
    if let (true, Some(recorder)) = (record, recorder) {
        let miss = ContentMiss {
            // Not a registry: this is the point. The record says so in the
            // one field an operator groups by, rather than inventing a name
            // that would collide with a real registry.
            registry: "(unmirrored)".to_owned(),
            storage_key: host.clone(),
            kind: MissKind::UnmirroredHost,
            coordinate: Some(tail.clone()),
            requested_version: None,
            held_versions: Vec::new(),
        };
        if let Err(e) = recorder.record(&miss, Utc::now()).await {
            tracing::warn!(host = %host, error = %e, "air gap: could not record an unmirrored host");
        }
    }
    tracing::info!(host = %host, "air gap: request for a host with no mirror");
    Ok(HttpResponse::NotImplemented().json(serde_json::json!({
        "error": "Not Implemented",
        "code": "unmirrored_host",
        "message": format!(
            "no registry on this instance mirrors '{host}'. Nothing was fetched. Add a registry \
             for it and rebuild the bundle, or drop the tool that needs it."
        ),
        "host": host,
        "bundle_hint": "batlehub-cli admin air-gap-missing --kind unmirrored_host",
    })))
}

#[derive(Deserialize, IntoParams)]
pub struct MissingQuery {
    pub registry: Option<String>,
    /// `artifact`, `document`, `checksum`, `ref` or `unmirrored_host`.
    pub kind: Option<String>,
    #[serde(default)]
    pub page: u64,
    #[serde(default = "default_per_page")]
    pub per_page: u64,
}

fn default_per_page() -> u64 {
    100
}

#[derive(Serialize, ToSchema)]
pub struct MissingResponse {
    pub items: Vec<RecordedMiss>,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
    /// Whether this instance is actually air-gapped. A connected instance
    /// answers with an empty list and this `false`, which is a different
    /// fact from "nothing is missing".
    pub air_gapped: bool,
    /// Proxy registries holding no cached artifact at all (RFC 0008 §4.5).
    ///
    /// On an air-gapped instance such a registry answers `503` to
    /// *everything*, and that is worth saying plainly rather than leaving an
    /// operator to read it off an empty miss log. Empty on a connected
    /// instance, where a registry with no cache is simply one nobody has
    /// asked for yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub empty_registries: Vec<String>,
}

fn parse_kind(raw: Option<&str>) -> Result<Option<MissKind>, AppError> {
    match raw.filter(|s| !s.is_empty()) {
        Some(s) => MissKind::parse(s).map(Some).ok_or_else(|| {
            AppError::bad_request(format!(
                "unknown kind '{s}' (expected artifact, document, checksum, ref or \
                 unmirrored_host)"
            ))
        }),
        None => Ok(None),
    }
}

async fn recorder_of(svc: &ProxyService) -> Result<(Arc<dyn MissRecorder>, bool), AppError> {
    let (recorder, air_gapped) = {
        let hot = svc.hot.read().await;
        (hot.miss_recorder.clone(), hot.air_gap.enabled)
    };
    let recorder = recorder.ok_or_else(|| {
        AppError::service_unavailable(
            "this deployment records no misses: it has no database, so there is nothing to list",
        )
    })?;
    Ok((recorder, air_gapped))
}

/// What this instance was asked for and did not hold.
#[utoipa::path(
    get,
    path = "/api/v1/admin/air-gap/missing",
    tag = "back-office",
    params(MissingQuery),
    responses(
        (status = 200, description = "Recorded misses, most-asked first", body = MissingResponse),
        (status = 403, description = "`system:read` required"),
        (status = 503, description = "No store to record into"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/air-gap/missing")]
pub async fn list_missing(
    query: web::Query<MissingQuery>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(&identity, Action::SystemRead, None, &hot).await?;
    let (recorder, air_gapped) = recorder_of(&svc).await?;
    let (page, per_page) = crate::handlers::clamp_pagination(query.page, query.per_page);
    let filter = MissFilter {
        registry: query.registry.clone(),
        kind: parse_kind(query.kind.as_deref())?,
        limit: per_page,
        offset: page * per_page,
    };
    let count_filter = MissFilter {
        limit: 0,
        offset: 0,
        ..filter.clone()
    };
    let (items, total) = tokio::try_join!(recorder.list(&filter), recorder.count(&count_filter))
        .map_err(AppError::from)?;
    Ok(web::Json(MissingResponse {
        items,
        total,
        page: query.page,
        per_page: query.per_page,
        air_gapped,
        empty_registries: if air_gapped {
            empty_registries(&svc).await
        } else {
            Vec::new()
        },
    }))
}

/// The proxy registries with nothing cached under them.
///
/// One `stat_by_prefix` per registry over the key namespace the proxy writes
/// to. Cheap enough for an admin read, and it stays true after an import —
/// which a figure taken once at boot would not.
pub async fn empty_registries(svc: &ProxyService) -> Vec<String> {
    let names: Vec<String> = {
        let hot = svc.hot.read().await;
        hot.registries.keys().cloned().collect()
    };
    let mut empty = Vec::new();
    for name in names {
        match svc
            .storage
            .stat_by_prefix(&format!("artifact:{name}/"))
            .await
        {
            // A failure to count is not evidence of emptiness, and saying
            // "this registry is empty" when the store did not answer would
            // be the worst of the three outcomes.
            Ok((0, _)) => empty.push(name),
            _ => continue,
        }
    }
    empty.sort();
    empty
}

#[derive(Deserialize, IntoParams)]
pub struct PurgeQuery {
    /// Forget rows last seen before this instant. Absent forgets every row
    /// older than the configured retention, which is the ordinary sweep.
    pub before: Option<DateTime<Utc>>,
    pub registry: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct PurgeMissingResponse {
    pub deleted: u64,
}

/// Forget recorded misses.
#[utoipa::path(
    delete,
    path = "/api/v1/admin/air-gap/missing",
    tag = "back-office",
    params(PurgeQuery),
    responses(
        (status = 200, description = "How many rows were forgotten", body = PurgeMissingResponse),
        (status = 403, description = "`system:write` required"),
        (status = 503, description = "No store to purge"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/api/v1/admin/air-gap/missing")]
pub async fn purge_missing(
    query: web::Query<PurgeQuery>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(&identity, Action::SystemWrite, None, &hot).await?;
    let (recorder, _) = recorder_of(&svc).await?;
    let before = match query.before {
        Some(b) => b,
        None => {
            let days = { svc.hot.read().await.air_gap.miss_retention_days };
            // `0` means "keep until purged by hand", and a purge *is* by
            // hand — so with no cutoff and no retention the sweep is a no-op
            // rather than a silent wipe of the whole table.
            if days == 0 {
                return Ok(web::Json(PurgeMissingResponse { deleted: 0 }));
            }
            Utc::now() - Duration::days(days as i64)
        }
    };
    let deleted = recorder
        .purge(before, query.registry.as_deref())
        .await
        .map_err(AppError::from)?;
    tracing::info!(deleted, by = ?identity.0.user_id, "air gap: missing-content purge");
    Ok(web::Json(PurgeMissingResponse { deleted }))
}

// ── The bundle ───────────────────────────────────────────────────────────────

#[derive(Serialize, ToSchema)]
pub struct BundleImportResponse {
    pub bundle_id: String,
    pub signer_key: String,
    /// Blobs written. A bundle carrying bytes this instance already holds
    /// writes them again under their key — the store dedups by content, so
    /// the count is of keys, not of bytes moved.
    pub imported: u64,
    pub entries: u64,
    /// Entries refused: a blob that did not hash to its name, a key that is
    /// not a storage key, a registry this instance does not have.
    pub rejected: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rejections: Vec<String>,
    /// `true` when this bundle id was already imported and nothing was
    /// written again.
    pub already_imported: bool,
}

/// The most a bundle may weigh in one request.
///
/// An import is an administrator action on a trusted file, not an open
/// upload, but the body is still read into memory before the signature is
/// checked — so it is bounded, and the number is stated rather than implied.
const MAX_BUNDLE_BYTES: usize = 4 * 1024 * 1024 * 1024;

/// The registry-kind facts a synthesised listing needs from an imported
/// artifact's bytes, by kind (RFC 0008-bis §13.4), merged over the facts
/// the manifest carried for the entry (§13.7) — what the connected side's
/// documents said that the bytes do not. `Null` for every kind whose keys
/// say enough.
fn listing_facts_for(
    kinds: &std::collections::HashMap<String, String>,
    registry: &str,
    coordinate: &batlehub_core::entities::PackageId,
    bytes: &[u8],
    carried: Option<&serde_json::Value>,
) -> serde_json::Value {
    let from_bytes = listing_facts_from_bytes(kinds, registry, coordinate, bytes);
    match (carried.filter(|c| c.is_object()), from_bytes) {
        (None, facts) => facts,
        (Some(carried), serde_json::Value::Null) => carried.clone(),
        (Some(carried), serde_json::Value::Object(read)) => {
            // Both keyed by kind; what the bytes say about a kind wins over
            // what a document said, because the bytes are what is served.
            let mut merged = carried.as_object().cloned().unwrap_or_default();
            for (kind, facts) in read {
                match (merged.get_mut(&kind), facts) {
                    (Some(serde_json::Value::Object(into)), serde_json::Value::Object(from)) => {
                        into.extend(from);
                    }
                    (_, facts) => {
                        merged.insert(kind, facts);
                    }
                }
            }
            serde_json::Value::Object(merged)
        }
        (Some(_), facts) => facts,
    }
}

fn listing_facts_from_bytes(
    kinds: &std::collections::HashMap<String, String>,
    registry: &str,
    coordinate: &batlehub_core::entities::PackageId,
    bytes: &[u8],
) -> serde_json::Value {
    match kinds.get(registry).map(String::as_str) {
        Some("cargo") => batlehub_adapters::listing_facts::cargo_index_facts(
            &coordinate.name,
            &coordinate.version,
            bytes,
        )
        .map(|facts| serde_json::json!({ "cargo": facts }))
        .unwrap_or(serde_json::Value::Null),
        // The compact index carries each gem's runtime dependencies inline,
        // from the gemspec inside the `.gem`.
        Some("rubygems") => batlehub_adapters::registry::rubygems::parse_gem_bytes(bytes)
            .map(|g| {
                serde_json::json!({ "rubygems": {
                    "platform": g.platform,
                    "dependencies": g.dependencies.iter().map(|d| serde_json::json!({
                        "name": d.name, "requirement": d.requirement,
                    })).collect::<Vec<_>>(),
                } })
            })
            .unwrap_or(serde_json::Value::Null),
        // `repodata.json` is a copy of each package's `info/index.json`.
        Some("conda") => batlehub_adapters::registry::conda::parse_conda_metadata(bytes)
            .map(|c| {
                serde_json::json!({ "conda": {
                    "name": c.name, "version": c.version, "build": c.build,
                    "build_number": c.build_number, "depends": c.depends,
                    "subdir": c.subdir, "license": c.license,
                } })
            })
            .unwrap_or(serde_json::Value::Null),
        // The registration page's catalog entry is the `.nuspec`.
        Some("nuget") => crate::handlers::proxy::nuget::nuspec::extract_nuspec_from_nupkg(bytes)
            .and_then(|x| crate::handlers::proxy::nuget::nuspec::parse_nuspec(&x))
            .map(|n| {
                serde_json::json!({ "nuget": {
                    "id": n.id, "version": n.version, "description": n.description,
                    "authors": n.authors, "tags": n.tags,
                } })
            })
            .unwrap_or(serde_json::Value::Null),
        // `p2` is `composer.json`, from inside the dist zip.
        Some("composer") => batlehub_adapters::registry::composer::parse_composer_zip(
            &bytes::Bytes::copy_from_slice(bytes),
            Some(&coordinate.version),
        )
        .map(|c| serde_json::json!({ "composer": c.composer_json }))
        .unwrap_or(serde_json::Value::Null),
        // A provider's checksum list names each archive's file, which the
        // composed download document is looked up by (§13.7). The keys and
        // protocols the same document needs are not in any artifact: they
        // arrive as the entry's carried facts.
        Some("terraform") if coordinate.artifact.as_deref() == Some("shasums") => {
            batlehub_adapters::listing_facts::terraform_shasums_facts(bytes)
                .map(|facts| serde_json::json!({ "terraform": facts }))
                .unwrap_or(serde_json::Value::Null)
        }
        _ => serde_json::Value::Null,
    }
}

/// Import a bundle.
#[utoipa::path(
    post,
    path = "/api/v1/admin/bundle/import",
    tag = "back-office",
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses(
        (status = 200, description = "What was imported", body = BundleImportResponse),
        (status = 400, description = "Not a bundle, or a manifest this build cannot read"),
        (status = 403, description = "`system:write` required, or the signature does not verify"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/bundle/import")]
pub async fn import_bundle(
    mut payload: web::Payload,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    history: Option<web::Data<Arc<dyn batlehub_core::ports::BundleHistory>>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(&identity, Action::SystemWrite, None, &hot).await?;
    let body = read_bundle_body(&mut payload).await?;

    // The manifest's bytes as written, then its signature, *before* a blob
    // is looked at: an unauthenticated bundle never reaches the store.
    let manifest_bytes =
        batlehub_core::services::bundle::manifest_bytes_of(&body[..]).map_err(AppError::from)?;
    let read = batlehub_core::services::bundle::read_bundle(&body[..]).map_err(AppError::from)?;
    let (trusted, air_gapped, registries, verdicts, refs, kinds) = {
        let hot = svc.hot.read().await;
        (
            hot.air_gap.bundle_trusted_keys.clone(),
            hot.air_gap.enabled,
            hot.registries.keys().cloned().collect::<Vec<_>>(),
            hot.verdicts.clone(),
            hot.ref_resolutions.clone(),
            hot.registries
                .iter()
                .map(|(name, client)| (name.clone(), client.registry_type().to_owned()))
                .collect::<std::collections::HashMap<String, String>>(),
        )
    };
    batlehub_core::services::bundle::verify_manifest_signature(
        &trusted,
        &read.signature,
        &manifest_bytes,
    )
    .map_err(AppError::from)?;
    let signer_key = signer_of(&trusted, &read.signature, &manifest_bytes);

    if let Some(history) = &history {
        if history
            .seen(&read.manifest.bundle_id)
            .await
            .unwrap_or(false)
        {
            return Ok(web::Json(BundleImportResponse {
                bundle_id: read.manifest.bundle_id.clone(),
                signer_key,
                imported: 0,
                entries: read.manifest.entries.len() as u64,
                rejected: 0,
                rejections: vec![],
                already_imported: true,
            }));
        }
    }

    let mut rejections: Vec<String> = read
        .rejected
        .iter()
        .map(|d| format!("blob {d}: its bytes do not hash to its name"))
        .collect();
    let mut imported = 0u64;
    let ctx = ImportCtx {
        svc: &svc,
        kinds: &kinds,
        registries: &registries,
        air_gapped,
        verdicts: &verdicts,
        refs: &refs,
        bundle_id: &read.manifest.bundle_id,
        created_at: read.manifest.created_at,
    };
    for entry in &read.manifest.entries {
        if ctx.import_entry(entry, &read.blobs, &mut rejections).await {
            imported += 1;
        }
    }

    let record = BundleImport {
        bundle_id: read.manifest.bundle_id.clone(),
        signer_key: signer_key.clone(),
        imported_at: Utc::now(),
        imported_by: identity.0.user_id.clone(),
        entries: read.manifest.entries.len() as u64,
        blobs: imported,
        rejected: rejections.len() as u64,
        rejected_sample: rejections.first().cloned(),
        created_from: read.manifest.source_plan.clone(),
    };
    if let Some(history) = &history {
        if let Err(e) = history.record(&record).await {
            tracing::warn!(error = %e, "air gap: could not record the bundle import");
        }
    }
    tracing::info!(
        bundle = %record.bundle_id,
        imported,
        rejected = record.rejected,
        by = ?identity.0.user_id,
        "air gap: bundle imported"
    );
    Ok(web::Json(BundleImportResponse {
        bundle_id: record.bundle_id,
        signer_key,
        imported,
        entries: record.entries,
        rejected: record.rejected,
        rejections,
        already_imported: false,
    }))
}

/// The request body, capped. A bundle arrives as one upload, so the cap is
/// enforced as the chunks arrive rather than after.
async fn read_bundle_body(payload: &mut web::Payload) -> Result<Vec<u8>, AppError> {
    use futures::StreamExt;
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = payload.next().await {
        let chunk = chunk.map_err(|e| AppError::bad_request(e.to_string()))?;
        if body.len() + chunk.len() > MAX_BUNDLE_BYTES {
            return Err(AppError::bad_request(format!(
                "a bundle may not exceed {MAX_BUNDLE_BYTES} bytes in one request"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Everything one bundle entry is imported against: the stores it writes to,
/// and the parts of the manifest that are the same for every entry.
struct ImportCtx<'a> {
    svc: &'a ProxyService,
    /// Registry name → its `registry_type`, for the listing facts.
    kinds: &'a std::collections::HashMap<String, String>,
    registries: &'a [String],
    air_gapped: bool,
    verdicts: &'a Option<Arc<dyn batlehub_core::ports::VerdictRepository>>,
    refs: &'a Option<Arc<dyn batlehub_core::ports::RefResolutionRepository>>,
    bundle_id: &'a str,
    created_at: chrono::DateTime<Utc>,
}

impl ImportCtx<'_> {
    /// Every guard the two existing funnels apply, applied here too: a bundle
    /// is untrusted input that *names storage keys*, and without this it would
    /// be a way to plant content at any key. `Some` is the rejection.
    fn refuse(&self, entry: &batlehub_core::services::bundle::BundleEntry) -> Option<String> {
        if !self.registries.iter().any(|r| r == &entry.registry) {
            return Some(format!(
                "{}: this instance has no registry named '{}'",
                entry.key, entry.registry
            ));
        }
        if let (Some(name), Some(version)) = (&entry.package_name, &entry.version) {
            if let Err(e) = batlehub_core::services::validate_coordinate(name, version, None) {
                return Some(format!("{}: {e}", entry.key));
            }
        }
        if let Err(e) = batlehub_core::services::validate_path_safe("bundle key", &entry.key) {
            return Some(format!("{}: {e}", entry.key));
        }
        // The key has to live under the registry the entry names. `import_entry`
        // writes `artifact:{key}` and `write_meta` writes `meta:{key}`, and those
        // are exactly `proxy_artifact_key`/`proxy_meta_key` —
        // `{registry}/{name}/{version}[/{artifact}]`. Without this check an entry
        // declaring one (known) registry while naming another registry's key
        // plants its bytes *and* a matching metadata entry in that other
        // registry's cache namespace, which then serves them as its own, while
        // `coordinate_of` files the carried verdict under the declared registry
        // so nothing in the verdict store looks wrong.
        if !entry.key.starts_with(&format!("{}/", entry.registry)) {
            return Some(format!(
                "{}: this key is not under the registry the entry names ('{}'); a bundle entry \
                 may only write into its own registry",
                entry.key, entry.registry
            ));
        }
        None
    }

    /// The metadata entry that lets the read path find the imported bytes.
    ///
    /// Without it an imported artifact is unreachable: `ProxyService` resolves
    /// metadata *before* it looks at the cache, and on an air-gapped instance
    /// the resolve has no upstream to ask — so a bundle would import cleanly,
    /// report its blobs written, and serve `503` for every one of them. The
    /// two keys are the same coordinate under two prefixes (`artifact:` and
    /// `meta:`), which is why one reported key is enough to write both.
    ///
    /// `published_at` is deliberately `None`: this instance knows when the
    /// bundle was made, not when the upstream published, and dating an
    /// artifact by its import would make every age gate read `fresh`. The
    /// judgement that *did* have the date is the verdict, made on the
    /// connected side and carried across.
    async fn write_meta(
        &self,
        entry: &batlehub_core::services::bundle::BundleEntry,
        coordinate: &batlehub_core::entities::PackageId,
        bytes: &[u8],
        rejections: &mut Vec<String>,
    ) {
        let entry_meta = batlehub_core::ports::CacheEntry {
            metadata: batlehub_core::entities::PackageMetadata {
                id: coordinate.clone(),
                published_at: None,
                download_url: None,
                checksum: Some(entry.digest.clone()),
                is_signed: None,
                cache_control: None,
                // RFC 0008-bis §13.4: the listing facts a synthesised document
                // needs and the key does not carry, read off the bytes once,
                // here. cargo's sparse-index line needs the crate's
                // dependencies and features.
                extra: listing_facts_for(
                    self.kinds,
                    &entry.registry,
                    coordinate,
                    bytes,
                    entry.facts.as_ref(),
                ),
            },
            cached_at: Utc::now(),
            // No expiry on a disconnected instance: there is nothing to
            // refresh from, and an entry that expired would take the artifact
            // it finds with it. On a *connected* one — the staging instance
            // that builds bundles, §4.5's second warning — it expires at once,
            // so the import seeds bytes without pinning a metadata answer the
            // upstream would have given better.
            expires_at: (!self.air_gapped).then(Utc::now),
        };
        if let Err(e) = self
            .svc
            .cache
            .set(&format!("meta:{}", entry.key), entry_meta, None)
            .await
        {
            rejections.push(format!(
                "{}: stored, but the metadata entry that finds it was not written: {e}",
                entry.key
            ));
        }
    }

    /// RFC 0008 §13.3: the ref → commit pair, so the disconnected instance can
    /// answer `tarball/main` without resolving anything.
    ///
    /// This is not a nicety on a forge registry: a ref is resolved *before*
    /// anything is fetched, and an instance with no forge to ask would refuse
    /// every coordinate in the bundle it just accepted. The TTL is frozen
    /// under `air_gap.enabled` for the same reason, so a row written here is
    /// never re-asked.
    async fn write_ref(
        &self,
        entry: &batlehub_core::services::bundle::BundleEntry,
        rejections: &mut Vec<String>,
    ) {
        let (Some(store), Some(r)) = (self.refs, &entry.git_ref) else {
            return;
        };
        // A kind this instance does not understand is refused rather than
        // defaulted. Guessing `tag` for a branch would lose the `MUTABLE_REF`
        // finding that makes following a branch visible — a silent downgrade
        // of a warning, which is the worst way to be wrong about a ref.
        let Ok(kind) = r.kind.parse::<batlehub_core::entities::RefKind>() else {
            rejections.push(format!(
                "{}: stored, but its ref names a kind this instance does not know ('{}')",
                entry.key, r.kind
            ));
            return;
        };
        let resolution = batlehub_core::ports::StoredRefResolution {
            kind,
            sha: r.sha.clone(),
            resolved_at: entry.verified_at.unwrap_or(self.created_at),
            previous: None,
        };
        if let Err(e) = store
            .upsert(&entry.registry, &r.owner_repo, &r.git_ref, &resolution)
            .await
        {
            rejections.push(format!(
                "{}: stored, but the ref resolution it carried was not recorded: {e}",
                entry.key
            ));
        }
    }

    /// RFC 0008 §13 decision 4: the judgement crosses the gap with the bytes.
    ///
    /// A disconnected instance cannot re-run a scanner, so an imported
    /// artifact with no verdict is `SCAN_PENDING` on a registry with
    /// `[security]` — fail-closed, and the bundle it just accepted would serve
    /// nothing. The row says where the judgement came from: `policy_ref` names
    /// the bundle, never the local policy, so nothing here can read as a check
    /// this instance made.
    async fn write_verdict(
        &self,
        entry: &batlehub_core::services::bundle::BundleEntry,
        coordinate: &batlehub_core::entities::PackageId,
        rejections: &mut Vec<String>,
    ) {
        let (Some(store), Some(state)) = (self.verdicts, carried_verdict(entry)) else {
            return;
        };
        let verdict = batlehub_core::entities::Verdict {
            package: coordinate.clone(),
            state,
            reason_codes: entry
                .reason_codes
                .iter()
                .filter_map(|c| c.parse().ok())
                .collect(),
            findings: vec![],
            policy_ref: format!("bundle:{}", self.bundle_id),
            available_at: None,
            evaluated_at: entry.verified_at.unwrap_or(self.created_at),
            last_scanned_at: entry.verified_at,
            scanners_done: vec![],
        };
        if let Err(e) = store.upsert(&verdict).await {
            rejections.push(format!(
                "{}: stored, but the verdict it carried was not recorded: {e}",
                entry.key
            ));
        }
    }

    /// One entry: the bytes, then everything that has to exist for them to be
    /// found and judged. `true` when the bytes landed.
    async fn import_entry(
        &self,
        entry: &batlehub_core::services::bundle::BundleEntry,
        blobs: &std::collections::BTreeMap<String, Vec<u8>>,
        rejections: &mut Vec<String>,
    ) -> bool {
        if let Some(reason) = self.refuse(entry) {
            rejections.push(reason);
            return false;
        }
        let Some(bytes) = blobs.get(&entry.digest) else {
            rejections.push(format!(
                "{}: the bundle carries no blob for digest {}",
                entry.key, entry.digest
            ));
            return false;
        };
        // The coordinate this entry names, worked out once: the metadata row,
        // the cache entry and the verdict all file under it.
        let coordinate = coordinate_of(entry);
        // The `artifact:` prefix is the one the whole tree shares — the
        // proxy's cache write, the eviction sweep, and the key the response
        // reported to whoever built this bundle.
        let storage_key = format!("artifact:{}", entry.key);
        if let Err(e) = self
            .svc
            .storage
            .store(
                &storage_key,
                bytes::Bytes::from(bytes.clone()),
                Default::default(),
            )
            .await
        {
            rejections.push(format!("{}: {e}", entry.key));
            return false;
        }
        if let Err(e) = self
            .svc
            .artifact_meta
            .record_artifact(batlehub_core::ports::ArtifactMetaRecord {
                key: &storage_key,
                registry: &entry.registry,
                package_name: &coordinate.name,
                version: &coordinate.version,
                size: Some(entry.size),
                checksum: Some(&entry.digest),
            })
            .await
        {
            // The bytes are in; the row that lets the cache find them is not.
            // Reported rather than swallowed: the artifact would be served but
            // never expired or evicted.
            rejections.push(format!(
                "{}: stored, but its metadata row failed: {e}",
                entry.key
            ));
        }
        self.write_meta(entry, &coordinate, bytes, rejections).await;
        self.write_ref(entry, rejections).await;
        self.write_verdict(entry, &coordinate, rejections).await;
        true
    }
}

/// The coordinate a bundle entry names.
///
/// The entry's own `package_name`/`version` when it has them; otherwise read
/// back out of the key, which is `{registry}/{name}/{version}[/{artifact}]`
/// — the shape `PackageId::cache_key` writes and every read path derives.
/// The name may contain slashes (`owner/repo`, `@scope/pkg`), so the split is
/// from the right, and an entry that will not split keeps the whole tail as a
/// version rather than inventing one.
fn coordinate_of(
    entry: &batlehub_core::services::bundle::BundleEntry,
) -> batlehub_core::entities::PackageId {
    if let (Some(name), Some(version)) = (&entry.package_name, &entry.version) {
        return batlehub_core::entities::PackageId::new(&entry.registry, name, version);
    }
    let rest = entry
        .key
        .strip_prefix(&format!("{}/", entry.registry))
        .unwrap_or(&entry.key);
    let parts: Vec<&str> = rest.rsplitn(3, '/').collect();
    match parts.as_slice() {
        // `{name…}/{version}/{artifact}`
        [_artifact, version, name] => {
            batlehub_core::entities::PackageId::new(&entry.registry, *name, *version)
        }
        [version, name] => {
            batlehub_core::entities::PackageId::new(&entry.registry, *name, *version)
        }
        _ => batlehub_core::entities::PackageId::new(&entry.registry, rest, ""),
    }
}

/// The verdict an entry carries, when it carries one this instance can read.
///
/// Only a *served* state crosses: an `allowed` or `warned` judgement made on
/// the connected side is evidence the disconnected instance may act on. A
/// `denied` or `quarantined` entry has no business being in a bundle at all —
/// `seed --verify` exits non-zero on one — and importing it as a stored
/// refusal would let a bundle *plant* a hold on this instance, which is a
/// lever the signer of a bundle should not have.
fn carried_verdict(
    entry: &batlehub_core::services::bundle::BundleEntry,
) -> Option<batlehub_core::entities::VerdictState> {
    match entry.verdict.as_deref()?.parse().ok()? {
        s @ (batlehub_core::entities::VerdictState::Allowed
        | batlehub_core::entities::VerdictState::Warned) => Some(s),
        _ => None,
    }
}

/// Which of the trusted keys signed it — for the history row, so an operator
/// can tell two signers apart.
fn signer_of(trusted: &[String], signature: &[u8], bytes: &[u8]) -> String {
    trusted
        .iter()
        .find(|k| {
            batlehub_core::services::signature::verify_ed25519(
                std::slice::from_ref(*k),
                signature,
                bytes,
            )
        })
        .cloned()
        .unwrap_or_else(|| "unknown".to_owned())
}

#[derive(Serialize, ToSchema)]
pub struct BundleHistoryResponse {
    pub items: Vec<BundleImport>,
}

/// What came across the gap, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/admin/bundle",
    tag = "back-office",
    responses(
        (status = 200, description = "Imported bundles", body = BundleHistoryResponse),
        (status = 403, description = "`system:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/bundle")]
pub async fn list_bundles(
    identity: AuthIdentity,
    history: Option<web::Data<Arc<dyn batlehub_core::ports::BundleHistory>>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(&identity, Action::SystemRead, None, &hot).await?;
    let items = match history {
        Some(h) => h.list(100).await.map_err(AppError::from)?,
        None => vec![],
    };
    Ok(web::Json(BundleHistoryResponse { items }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_is_the_first_segment_and_the_tail_is_never_parsed() {
        assert_eq!(
            host_of("binaries.sonarsource.com/x/y.tar.gz"),
            "binaries.sonarsource.com"
        );
        assert_eq!(host_of("example.com"), "example.com");
        assert_eq!(host_of(""), "");
    }

    #[test]
    fn an_unknown_kind_is_refused_rather_than_ignored() {
        assert!(parse_kind(Some("artifact")).unwrap() == Some(MissKind::Artifact));
        assert!(parse_kind(None).unwrap().is_none());
        assert!(parse_kind(Some("")).unwrap().is_none());
        assert!(parse_kind(Some("nope")).is_err());
    }
}
