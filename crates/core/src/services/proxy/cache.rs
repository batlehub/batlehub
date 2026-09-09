//! Artifact cache paths for [`ProxyService::handle`](super::ProxyService::handle):
//! serving a fresh cache hit (with optional re-serve verification) and the
//! upstream fetch-and-cache miss path. Split out of `handle.rs` to keep the
//! top-level orchestrator readable.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use bytes::Bytes;
use futures::{stream, StreamExt};

use crate::entities::{AccessEvent, PackageMetadata};
use crate::error::CoreError;
use crate::ports::{ArtifactMetaRecord, ByteStream, RegistryClient, StorageMeta};
use crate::services::cache_control::parse_cache_control;
use crate::services::hot_config::IntegrityPolicy;
use crate::services::integrity::{IntegrityOutcome, StreamingVerifier};

use super::handle::REVERIFY_BUFFER_LIMIT;
use super::{ProxyRequest, ProxyResponse, ProxyService, RequestTiming};

/// The artifact-cache storage key for `req`. A pure function of the package
/// coordinate, so functions below recompute it instead of taking it as a
/// parameter — `handle.rs` still holds its own copy for the `artifact_is_fresh`
/// check that picks between the cache-hit and fetch-and-cache paths.
/// Read the first chunk of a raw response and refuse it when it starts like
/// a script and the registry's policy is `deny`.
///
/// The chunk is put back, so the caller streams the whole file exactly as it
/// came: this peeks, it does not buffer the artifact.
async fn peek_for_script(
    stream: crate::ports::ArtifactStream,
    coordinate: &str,
) -> Result<crate::ports::ArtifactStream, CoreError> {
    use futures::StreamExt;
    let mut stream = stream;
    let first = stream.next().await;
    match first {
        Some(Ok(chunk)) => {
            if crate::entities::RawPolicy::starts_like_script(&chunk) {
                return Err(CoreError::AccessDenied(format!(
                    "RAW_SCRIPT: '{coordinate}' begins with a script preamble; this registry's \
                     raw.scripts is \"deny\""
                )));
            }
            let head = futures::stream::once(async move { Ok(chunk) });
            Ok(Box::pin(head.chain(stream)))
        }
        Some(Err(e)) => Err(e),
        None => Ok(Box::pin(futures::stream::empty())),
    }
}

fn artifact_key_for(req: &ProxyRequest) -> String {
    format!("artifact:{}", req.package_id.cache_key())
}

impl ProxyService {
    /// [`peek_for_script`] when this is a raw coordinate on a registry whose
    /// `raw.scripts` is `deny`; the stream untouched otherwise.
    async fn refuse_raw_script(
        &self,
        req: &ProxyRequest,
        stream: crate::ports::ArtifactStream,
    ) -> Result<crate::ports::ArtifactStream, CoreError> {
        use crate::entities::{ForgeCoordinate, ForgeKind, ScriptAction};
        let Some(ForgeKind::Raw { path, .. }) =
            ForgeCoordinate::from_package_id(&req.package_id).map(|c| c.kind)
        else {
            return Ok(stream);
        };
        let deny = {
            let hot = self.hot.read().await;
            hot.forge_raw
                .get(&req.package_id.registry)
                .is_some_and(|p| p.enabled && p.scripts == ScriptAction::Deny)
        };
        if !deny {
            return Ok(stream);
        }
        peek_for_script(stream, &path).await
    }

    /// Serve an artifact from a fresh cache hit. When `verify_on_serve` is set,
    /// the stored bytes are re-hashed against the recorded SHA-256 before being
    /// streamed (failing closed on a lookup error, evicting on a mismatch). See
    /// the module-level note on `REVERIFY_BUFFER_LIMIT` for the
    /// retain-vs-re-read trade-off.
    pub(super) async fn serve_cache_hit(
        &self,
        req: ProxyRequest,
        artifact_key: String,
        integrity: &IntegrityPolicy,
        timing: &RequestTiming,
    ) -> Result<ProxyResponse, CoreError> {
        let registry_name = req.package_id.registry.as_str();
        tracing::debug!(key = %artifact_key, "artifact cache hit");
        metrics::counter!("batlehub_artifact_cache_hits_total", "registry" => timing.registry_label.clone()).increment(1);
        self.metrics.record_artifact_hit(registry_name);
        let artifact = self.storage.retrieve(&artifact_key).await?.ok_or_else(|| {
            CoreError::Registry(format!(
                "artifact '{artifact_key}' vanished between exists and retrieve"
            ))
        })?;

        // Re-serve integrity verification (opt-in via `verify_on_serve`): re-hash
        // the stored bytes against the SHA-256 we computed when they were first
        // cached. Catches storage corruption or tampering of an already-cached
        // artifact. Off by default because it reads + hashes the bytes on every
        // cache hit. The hash is computed by streaming the stored bytes through a
        // `StreamingVerifier` (memory stays bounded regardless of artifact size);
        // a verified entry is then re-opened from storage to serve.
        let response_stream = if integrity.enabled && integrity.verify_on_serve {
            self.reverify_on_serve(&req, &artifact_key, artifact.stream, timing)
                .await?
        } else {
            artifact.stream
        };

        let meta_repo = Arc::clone(&self.artifact_meta);
        let key_clone = artifact_key.clone();
        tokio::spawn(async move {
            if let Err(e) = meta_repo.touch_artifact(&key_clone).await {
                tracing::warn!(key = %key_clone, error = %e, "touch_artifact failed");
            }
        });

        super::warn_if_audit_failed(
            self.repo
                .record_access(
                    AccessEvent::allowed_download(
                        req.package_id,
                        req.identity.user_id,
                        req.identity.role,
                    )
                    .with_ip_ua(req.ip_address.clone(), req.user_agent.clone()),
                )
                .await,
            "allowed download",
        );
        super::finish_request(&timing.registry_label, "allowed", timing.start);
        Ok(ProxyResponse::Stream(response_stream))
    }

    /// Re-serve verification for a cache hit: re-hash the stored bytes against the
    /// recorded SHA-256 and return the stream to serve. Serves as-is when no
    /// (parseable) checksum is recorded; fails closed (`IntegrityFailure`) when the
    /// checksum lookup fails or the digest mismatches. See [`serve_cache_hit`] and
    /// the `REVERIFY_BUFFER_LIMIT` note for the retain-vs-re-read trade-off.
    ///
    /// [`serve_cache_hit`]: Self::serve_cache_hit
    async fn reverify_on_serve(
        &self,
        req: &ProxyRequest,
        artifact_key: &str,
        stream: ByteStream,
        timing: &RequestTiming,
    ) -> Result<ByteStream, CoreError> {
        let registry_name = req.package_id.registry.as_str();

        let expected = match self.artifact_meta.get_artifact_checksum(artifact_key).await {
            // A recorded checksum: re-verify against it below.
            Ok(Some(c)) => c,
            // No checksum recorded (entry cached before `verify_on_serve` existed,
            // or never refreshed since): documented skip — serve as-is.
            Ok(None) => {
                tracing::debug!(key = %artifact_key, "no stored checksum for re-serve verification; serving as-is");
                return Ok(stream);
            }
            // The checksum lookup itself failed. `verify_on_serve` is an opt-in
            // guarantee that every served byte is re-verified, so fail closed
            // rather than serve possibly-corrupt bytes we cannot check. The cache
            // entry is left intact (the bytes may be fine; we just could not
            // confirm it this time).
            Err(e) => {
                let reason = format!(
                    "cannot re-verify cached artifact for {}: checksum lookup failed: {e}",
                    req.package_id,
                );
                tracing::warn!(registry = %registry_name, key = %artifact_key, error = %e, "re-serve checksum lookup failed; failing closed");
                metrics::counter!("batlehub_integrity_checks_total", "registry" => timing.registry_label.clone(), "outcome" => "lookup_failed", "phase" => "reserve").increment(1);
                super::finish_request(&timing.registry_label, "integrity_failed", timing.start);
                return Err(CoreError::IntegrityFailure(reason));
            }
        };

        // A recorded checksum that cannot be parsed: serve as-is, same as missing.
        let Some(verifier) = StreamingVerifier::new(&expected) else {
            metrics::counter!("batlehub_integrity_checks_total", "registry" => timing.registry_label.clone(), "outcome" => "unparseable", "phase" => "reserve").increment(1);
            tracing::warn!(registry = %registry_name, key = %artifact_key, "stored checksum could not be parsed; serving without re-verification");
            return Ok(stream);
        };

        // Stream the stored bytes through the hasher, retaining them up to
        // `REVERIFY_BUFFER_LIMIT` so a verified small artifact (the common
        // case) is served from the exact bytes we hashed — one read, no re-retrieve
        // race. A larger artifact drops the copy and is re-opened below.
        let (outcome, retained) = hash_stream_retaining(stream, verifier).await?;
        match outcome {
            IntegrityOutcome::Verified { algo } => {
                metrics::counter!("batlehub_integrity_checks_total", "registry" => timing.registry_label.clone(), "outcome" => "verified", "phase" => "reserve").increment(1);
                tracing::debug!(registry = %registry_name, key = %artifact_key, algo = algo.as_str(), "cached artifact re-verified on serve");
            }
            IntegrityOutcome::Mismatch {
                algo,
                expected,
                actual,
            } => {
                return Err(self
                    .evict_reverify_mismatch(
                        req,
                        artifact_key,
                        timing,
                        algo.as_str(),
                        &expected,
                        &actual,
                    )
                    .await);
            }
            // `StreamingVerifier::new` already rejected unparseable checksums, so
            // `finish` never returns `Unparseable`.
            IntegrityOutcome::Unparseable => {
                tracing::warn!(registry = %registry_name, key = %artifact_key, "re-serve verifier produced an unexpected unparseable outcome; serving without claiming verified");
            }
        }

        match retained {
            // Small artifact: serve the exact bytes we just verified.
            Some(buf) => Ok(Box::pin(futures::stream::once(async move {
                Ok::<Bytes, CoreError>(Bytes::from(buf))
            }))),
            // Oversized artifact: the verifying read consumed the stream, so re-open
            // a fresh one. A concurrent eviction between the two reads yields a clean
            // miss/error here, never unverified bytes.
            None => Ok(self
                .storage
                .retrieve(artifact_key)
                .await?
                .ok_or_else(|| {
                    CoreError::Registry(format!(
                        "artifact '{artifact_key}' vanished after re-serve verification"
                    ))
                })?
                .stream),
        }
    }

    /// Evict a corrupt cached artifact (bytes + recorded meta), audit, finish the
    /// request, and build the `IntegrityFailure` for a re-serve digest mismatch.
    async fn evict_reverify_mismatch(
        &self,
        req: &ProxyRequest,
        artifact_key: &str,
        timing: &RequestTiming,
        algo: &str,
        expected: &str,
        actual: &str,
    ) -> CoreError {
        let registry_name = req.package_id.registry.as_str();
        metrics::counter!("batlehub_integrity_checks_total", "registry" => timing.registry_label.clone(), "outcome" => "mismatch", "phase" => "reserve").increment(1);
        tracing::warn!(registry = %registry_name, key = %artifact_key, algo, %expected, %actual, "cached artifact failed re-serve integrity check; evicting");
        // Drop the corrupt entry so a later request re-fetches clean bytes.
        if let Err(e) = self.storage.delete(artifact_key).await {
            tracing::warn!(key = %artifact_key, error = %e, "failed to evict corrupt cached artifact");
        }
        if let Err(e) = self.artifact_meta.delete_artifact_meta(artifact_key).await {
            tracing::warn!(key = %artifact_key, error = %e, "failed to delete meta for corrupt artifact");
        }
        let reason = format!(
            "cached artifact failed re-serve integrity check for {}: {algo} digest mismatch (expected {expected}, got {actual})",
            req.package_id,
        );
        self.audit_integrity_failure(req, &reason, "reserve integrity mismatch", timing)
            .await;
        CoreError::IntegrityFailure(reason)
    }

    /// Cache miss: fetch the artifact from upstream and stream it to storage
    /// while enforcing the size limit and hashing it incrementally — peak memory
    /// stays bounded to one chunk regardless of artifact size. After the stream
    /// completes, the advertised-checksum verdict is checked; on a blocking
    /// mismatch the just-stored bytes are evicted and never served. The client is
    /// then served from the freshly cached entry (also streamed). When the
    /// upstream says `no-store`, falls back to a buffered serve-from-memory path
    /// (see [`Self::serve_no_store`]) since verify-before-serve has nothing to
    /// stream from.
    pub(super) async fn fetch_and_cache(
        &self,
        req: ProxyRequest,
        client: Arc<dyn RegistryClient>,
        metadata: PackageMetadata,
        integrity: &IntegrityPolicy,
        limit: u64,
        timing: &RequestTiming,
    ) -> Result<ProxyResponse, CoreError> {
        let registry_name = req.package_id.registry.as_str();
        // Pure function of `req`, cheap to recompute — the caller already holds its
        // own copy (needed for the `artifact_is_fresh` check before choosing this
        // path), so this isn't threaded through as a ninth parameter.
        let artifact_key = artifact_key_for(&req);
        tracing::debug!(key = %artifact_key, "artifact not cached, fetching from upstream");
        metrics::counter!("batlehub_artifact_cache_misses_total", "registry" => timing.registry_label.clone()).increment(1);
        self.metrics.record_artifact_miss(registry_name);
        let upstream_start = Instant::now();
        let mut upstream = self
            .fetch_artifact_or_record_error(&client, &req, &timing.registry_label, upstream_start)
            .await?;
        // Times the whole body transfer (not just time-to-headers) — see
        // `time_upstream_stream` for why.
        upstream.stream = super::time_upstream_stream(
            Arc::clone(&timing.registry_label),
            "fetch_artifact",
            upstream_start,
            Arc::clone(&self.metrics),
            upstream.stream,
        );

        // RFC 0019 §4.2 *Raw content*, phase 3 — the shebang half of the
        // script check, on the bytes rather than the name. Only under
        // `scripts = "deny"`: refusing costs the first chunk and nothing
        // else, while a `warn` that had to read bytes would mean buffering
        // every raw file to say something the response already says by name.
        upstream.stream = self
            .refuse_raw_script(&req, upstream.stream)
            .await
            .inspect_err(|_| {
                tracing::info!(package = %req.package_id, "raw: refused by scripts = deny");
            })?;

        let skip_artifact_cache = upstream
            .cache_control
            .as_deref()
            .map(|h| parse_cache_control(h).no_store)
            .unwrap_or(false);

        // ── Missing-checksum gate (no artifact bytes needed) ───────────────────
        // Decided up front so it applies equally to the streaming and no-store
        // paths, and so we never start a transfer that policy will reject.
        self.gate_missing_checksum(&req, &metadata, integrity, &artifact_key, timing)
            .await?;

        if skip_artifact_cache {
            tracing::debug!(key = %artifact_key, "upstream Cache-Control: no-store; skipping artifact cache");
            return self
                .serve_no_store(req, upstream.stream, &metadata, integrity, limit, timing)
                .await;
        }

        // ── Build the advertised-checksum verifier (None ⇒ no verification) ────
        // `None` covers: integrity disabled, no advertised checksum (gated above),
        // or an unparseable checksum (treated like missing, with a metric).
        let verifier = build_artifact_verifier(
            integrity,
            &metadata,
            &timing.registry_label,
            registry_name,
            &artifact_key,
        );

        // Only artifacts that can actually be *blocked* need to be hidden from
        // concurrent readers until verified. Stream those to a private staging
        // key and promote to `artifact_key` only after the digest verifies, so a
        // concurrent cache lookup never observes unverified bytes through the real
        // key. When nothing can block (no verifier, or warn-only), stream straight
        // to the real key — there is nothing to retract.
        let had_verifier = verifier.is_some();
        let must_stage = had_verifier && integrity.block_on_mismatch;
        let store_key = if must_stage {
            format!("staging:{}", uuid::Uuid::new_v4())
        } else {
            artifact_key.clone()
        };

        // ── Stream upstream → storage, hashing + size-limiting in flight ───────
        let outcome_slot: Arc<Mutex<Option<IntegrityOutcome>>> = Arc::new(Mutex::new(None));
        let instrumented =
            instrument_upstream(upstream.stream, limit, verifier, Arc::clone(&outcome_slot));
        let store_outcome = match self
            .storage
            .store_streaming(&store_key, instrumented, StorageMeta::default())
            .await
        {
            Ok(o) => o,
            Err(e) => {
                // Mid-stream failure (size-limit abort or upstream/backend error).
                // Backends clean up their own partial writes; drop the staging key
                // too in case the backend committed a partial entry under it.
                if must_stage {
                    if let Err(cleanup_err) = self.storage.delete(&store_key).await {
                        tracing::warn!(key = %store_key, error = %cleanup_err, "failed to delete staging artifact after mid-stream failure");
                    }
                }
                return Err(e);
            }
        };

        // ── Inspect the advertised-checksum verdict (post-stream) ──────────────
        let verify_outcome = outcome_slot.lock().expect("verifier mutex").take();
        self.enforce_verify_outcome(
            &req,
            &store_key,
            had_verifier,
            verify_outcome,
            integrity,
            timing,
        )
        .await?;

        // Only now — verified (or warn-only), never evicted — do the bytes count
        // as actually cached. Counting earlier (right after `store_streaming`)
        // would overstate the metric on a blocked checksum mismatch, since
        // `enforce_verify_outcome` above can still evict everything just written.
        metrics::counter!("batlehub_cached_bytes_total", "registry" => timing.registry_label.clone())
            .increment(store_outcome.size);

        // Verified (or warn-only): promote a staged blob to the real key now —
        // after verification — so the artifact only becomes visible to concurrent
        // readers as already-verified bytes. The promote re-streams the staged
        // bytes (bounded memory) and dedup-hits the existing blob, so it costs no
        // extra blob copy.
        if must_stage {
            self.promote_staged(&store_key, &artifact_key).await?;
        }

        // The digest computed by the streaming store is the same bare SHA-256 the
        // re-serve path expects, so persist it as the stored checksum directly —
        // no second hashing pass.
        if let Err(e) = self
            .artifact_meta
            .record_artifact(ArtifactMetaRecord {
                key: &artifact_key,
                registry: registry_name,
                package_name: &req.package_id.name,
                version: &req.package_id.version,
                size: Some(store_outcome.size),
                checksum: Some(&store_outcome.content_hash),
            })
            .await
        {
            tracing::warn!(key = %artifact_key, error = %e, "record_artifact failed");
        }

        self.maybe_introspect_artifact(
            registry_name,
            &artifact_key,
            &metadata,
            client.registry_type(),
        )
        .await;

        // ── Serve the client from the freshly cached bytes (streamed) ──────────
        let stored = self.storage.retrieve(&artifact_key).await?.ok_or_else(|| {
            CoreError::Registry(format!(
                "artifact '{artifact_key}' vanished immediately after caching"
            ))
        })?;

        super::warn_if_audit_failed(
            self.repo
                .record_access(
                    AccessEvent::allowed_download(
                        req.package_id,
                        req.identity.user_id,
                        req.identity.role,
                    )
                    .with_ip_ua(req.ip_address.clone(), req.user_agent.clone()),
                )
                .await,
            "allowed download",
        );
        super::finish_request(&timing.registry_label, "allowed", timing.start);
        Ok(ProxyResponse::Stream(stored.stream))
    }

    /// Enforce the integrity policy when upstream advertises no checksum. No-op
    /// unless integrity is on and the checksum is missing; fails closed (evicting
    /// nothing, since no bytes are stored yet) when metadata is required and the
    /// caller's role is not exempt.
    async fn gate_missing_checksum(
        &self,
        req: &ProxyRequest,
        metadata: &PackageMetadata,
        integrity: &IntegrityPolicy,
        artifact_key: &str,
        timing: &RequestTiming,
    ) -> Result<(), CoreError> {
        if !(integrity.enabled && metadata.checksum.is_none()) {
            return Ok(());
        }
        let registry_name = req.package_id.registry.as_str();
        metrics::counter!("batlehub_integrity_checks_total", "registry" => timing.registry_label.clone(), "outcome" => "missing").increment(1);
        if integrity.require_metadata && !integrity.bypass_roles.contains(&req.identity.role) {
            let reason = format!(
                "integrity policy requires checksum metadata, but upstream provided none for {}",
                req.package_id,
            );
            self.audit_integrity_failure(req, &reason, "integrity missing metadata", timing)
                .await;
            return Err(CoreError::IntegrityFailure(reason));
        }
        tracing::debug!(registry = %registry_name, key = %artifact_key, "upstream provided no integrity metadata");
        Ok(())
    }

    /// Inspect the post-stream advertised-checksum verdict and fail closed when
    /// required: a verifier that published no verdict (stream not consumed) or a
    /// blocking mismatch. In both cases the just-stored (staging) bytes are
    /// evicted so they are never promoted or served. `Ok(())` means proceed.
    async fn enforce_verify_outcome(
        &self,
        req: &ProxyRequest,
        store_key: &str,
        had_verifier: bool,
        verify_outcome: Option<IntegrityOutcome>,
        integrity: &IntegrityPolicy,
        timing: &RequestTiming,
    ) -> Result<(), CoreError> {
        let registry_name = req.package_id.registry.as_str();
        // Pure function of `req`; see `fetch_and_cache`'s matching comment.
        let artifact_key = artifact_key_for(req);

        // Fail closed: a verifier was installed but published no verdict, which
        // means the stream was not consumed to completion. We cannot claim the
        // bytes were verified, so evict and reject rather than serve them.
        if had_verifier && verify_outcome.is_none() {
            self.evict_staged(store_key, "failed to evict unverified artifact")
                .await;
            let reason = format!(
                "integrity verification did not complete for {}",
                req.package_id,
            );
            self.audit_integrity_failure(req, &reason, "integrity incomplete", timing)
                .await;
            return Err(CoreError::IntegrityFailure(reason));
        }

        if let Some(outcome) = &verify_outcome {
            if let Some(reason) = classify_integrity_outcome(
                outcome,
                integrity.block_on_mismatch,
                &timing.registry_label,
                registry_name,
                &artifact_key,
                &req.package_id,
            ) {
                self.evict_staged(store_key, "failed to evict mismatched artifact")
                    .await;
                self.audit_integrity_failure(req, &reason, "integrity mismatch", timing)
                    .await;
                return Err(CoreError::IntegrityFailure(reason));
            }
        }
        Ok(())
    }

    /// Best-effort eviction of a stored (staging) key, logging on failure.
    async fn evict_staged(&self, store_key: &str, warn_msg: &str) {
        if let Err(e) = self.storage.delete(store_key).await {
            tracing::warn!(key = %store_key, error = %e, "{warn_msg}");
        }
    }

    /// Record a proxy-error audit event and finish the request as integrity-failed.
    async fn audit_integrity_failure(
        &self,
        req: &ProxyRequest,
        reason: &str,
        audit_label: &str,
        timing: &RequestTiming,
    ) {
        super::warn_if_audit_failed(
            self.repo
                .record_access(
                    AccessEvent::proxy_error(
                        req.package_id.clone(),
                        req.identity.user_id.clone(),
                        req.identity.role.clone(),
                        reason.to_owned(),
                    )
                    .with_ip_ua(req.ip_address.clone(), req.user_agent.clone()),
                )
                .await,
            audit_label,
        );
        super::finish_request(&timing.registry_label, "integrity_failed", timing.start);
    }

    /// Promote verified staged bytes to the real artifact key, then drop the
    /// staging copy. The promote re-streams the staged bytes (bounded memory) and
    /// dedup-hits the existing blob, so it costs no extra blob copy.
    async fn promote_staged(&self, store_key: &str, artifact_key: &str) -> Result<(), CoreError> {
        let staged = self.storage.retrieve(store_key).await?.ok_or_else(|| {
            CoreError::Registry(format!(
                "staged artifact '{store_key}' vanished before promotion"
            ))
        })?;
        if let Err(e) = self
            .storage
            .store_streaming(artifact_key, staged.stream, StorageMeta::default())
            .await
        {
            if let Err(cleanup_err) = self.storage.delete(store_key).await {
                tracing::warn!(key = %store_key, error = %cleanup_err, "failed to delete staging artifact after promotion failure");
            }
            return Err(e);
        }
        if let Err(e) = self.storage.delete(store_key).await {
            tracing::warn!(key = %store_key, error = %e, "failed to delete staging artifact after promotion");
        }
        Ok(())
    }

    /// `no-store` fallback: the upstream forbids caching, so there is nothing to
    /// stream-then-verify from. Buffer the bytes (bounded by `limit`), run the
    /// one-shot integrity check to preserve the verify-before-serve guarantee,
    /// then serve from memory without persisting. Rare path; memory is not
    /// bounded here by design.
    async fn serve_no_store(
        &self,
        req: ProxyRequest,
        mut stream: ByteStream,
        metadata: &PackageMetadata,
        integrity: &IntegrityPolicy,
        limit: u64,
        timing: &RequestTiming,
    ) -> Result<ProxyResponse, CoreError> {
        let registry_name = req.package_id.registry.as_str();
        // Pure function of `req`; see `fetch_and_cache`'s matching comment.
        let artifact_key = artifact_key_for(&req);
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if buf.len() as u64 + chunk.len() as u64 > limit {
                return Err(CoreError::PayloadTooLarge(format!(
                    "artifact exceeds the {limit} byte limit"
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        let data = Bytes::from(buf);

        if integrity.enabled {
            use crate::services::integrity::verify;
            // Missing-checksum is gated by the caller; only Some(..) reaches here
            // with anything to check. Nothing is stored, so a blocking verdict just
            // returns — there is no cached entry to evict.
            if let Some(expected) = metadata.checksum.as_deref() {
                // `verify` hashes the whole buffer synchronously; offload it to a
                // blocking task so a large artifact's SHA computation doesn't stall
                // this Tokio worker's other in-flight requests.
                let expected_owned = expected.to_owned();
                let data_for_hash = data.clone();
                let outcome =
                    tokio::task::spawn_blocking(move || verify(&expected_owned, &data_for_hash))
                        .await
                        .map_err(|e| {
                            CoreError::Other(anyhow::anyhow!("integrity check task panicked: {e}"))
                        })?;
                if let Some(reason) = classify_integrity_outcome(
                    &outcome,
                    integrity.block_on_mismatch,
                    &timing.registry_label,
                    registry_name,
                    &artifact_key,
                    &req.package_id,
                ) {
                    self.audit_integrity_failure(&req, &reason, "integrity mismatch", timing)
                        .await;
                    return Err(CoreError::IntegrityFailure(reason));
                }
            }
        }

        super::warn_if_audit_failed(
            self.repo
                .record_access(
                    AccessEvent::allowed_download(
                        req.package_id,
                        req.identity.user_id,
                        req.identity.role,
                    )
                    .with_ip_ua(req.ip_address.clone(), req.user_agent.clone()),
                )
                .await,
            "allowed download",
        );
        super::finish_request(&timing.registry_label, "allowed", timing.start);
        Ok(ProxyResponse::Stream(Box::pin(stream::once(async move {
            Ok(data)
        }))))
    }
}

/// Drain `stream` through `verifier`, retaining the bytes up to
/// `REVERIFY_BUFFER_LIMIT`. Returns the final digest verdict plus the
/// retained bytes (`None` once the cap is crossed — the caller re-opens storage).
async fn hash_stream_retaining(
    mut stream: ByteStream,
    mut verifier: StreamingVerifier,
) -> Result<(IntegrityOutcome, Option<Vec<u8>>), CoreError> {
    let mut retained: Option<Vec<u8>> = Some(Vec::new());
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        verifier.update(&chunk);
        if let Some(buf) = retained.as_mut() {
            if buf.len() + chunk.len() > REVERIFY_BUFFER_LIMIT {
                // Over the cap: stop retaining and hash-only from here.
                retained = None;
            } else {
                buf.extend_from_slice(&chunk);
            }
        }
    }
    Ok((verifier.finish(), retained))
}

/// Build the advertised-checksum verifier for the streaming path. Returns `None`
/// when integrity is disabled, no checksum is advertised (gated earlier), or the
/// checksum is unparseable (treated like missing, with a metric).
fn build_artifact_verifier(
    integrity: &IntegrityPolicy,
    metadata: &PackageMetadata,
    registry_label: &Arc<str>,
    registry_name: &str,
    artifact_key: &str,
) -> Option<StreamingVerifier> {
    if !integrity.enabled {
        return None;
    }
    let checksum = metadata.checksum.as_deref()?;
    match StreamingVerifier::new(checksum) {
        Some(v) => Some(v),
        None => {
            metrics::counter!("batlehub_integrity_checks_total", "registry" => registry_label.clone(), "outcome" => "unparseable").increment(1);
            tracing::warn!(registry = %registry_name, key = %artifact_key, checksum = checksum, "advertised checksum could not be parsed; skipping verification");
            None
        }
    }
}

/// Record the metric + log line for an advertised-checksum verdict and decide
/// whether the download must be blocked. Returns `Some(reason)` when the caller
/// must fail the request (a mismatch under `block_on_mismatch`), `None`
/// otherwise. Shared by the streaming path and the `no-store` path so the
/// Verified/Mismatch/Unparseable ladder lives in exactly one place; each caller
/// does its own eviction/audit/finish on a `Some`.
fn classify_integrity_outcome(
    outcome: &IntegrityOutcome,
    block_on_mismatch: bool,
    registry_label: &Arc<str>,
    registry_name: &str,
    key: &str,
    package_id: &crate::entities::PackageId,
) -> Option<String> {
    match outcome {
        IntegrityOutcome::Verified { algo } => {
            metrics::counter!("batlehub_integrity_checks_total", "registry" => Arc::clone(registry_label), "outcome" => "verified").increment(1);
            tracing::debug!(registry = %registry_name, key = %key, algo = algo.as_str(), "artifact integrity verified");
            None
        }
        IntegrityOutcome::Mismatch {
            algo,
            expected,
            actual,
        } => {
            metrics::counter!("batlehub_integrity_checks_total", "registry" => Arc::clone(registry_label), "outcome" => "mismatch").increment(1);
            tracing::warn!(registry = %registry_name, key = %key, algo = algo.as_str(), %expected, %actual, "artifact integrity mismatch");
            block_on_mismatch.then(|| {
                format!(
                    "integrity check failed for {package_id}: {} digest mismatch (expected {expected}, got {actual})",
                    algo.as_str(),
                )
            })
        }
        // `StreamingVerifier::new` rejects unparseable checksums, so the streaming
        // path never yields this; the `no-store` one-shot `verify` can.
        IntegrityOutcome::Unparseable => {
            metrics::counter!("batlehub_integrity_checks_total", "registry" => Arc::clone(registry_label), "outcome" => "unparseable").increment(1);
            tracing::warn!(registry = %registry_name, key = %key, "advertised checksum could not be parsed; skipping verification");
            None
        }
    }
}

/// State threaded through [`instrument_upstream`]'s `unfold`.
struct InstrumentState {
    stream: ByteStream,
    limit: u64,
    seen: u64,
    verifier: Option<StreamingVerifier>,
    outcome: Arc<Mutex<Option<IntegrityOutcome>>>,
    done: bool,
}

/// Wrap an upstream byte stream so that, as it is consumed, it (1) enforces the
/// `limit` (yielding [`CoreError::PayloadTooLarge`] and stopping), and (2) feeds
/// an optional [`StreamingVerifier`], publishing the final [`IntegrityOutcome`]
/// into `outcome` when the stream completes cleanly. Peak extra memory is one
/// chunk — the artifact is never buffered whole.
fn instrument_upstream(
    stream: ByteStream,
    limit: u64,
    verifier: Option<StreamingVerifier>,
    outcome: Arc<Mutex<Option<IntegrityOutcome>>>,
) -> ByteStream {
    let state = InstrumentState {
        stream,
        limit,
        seen: 0,
        verifier,
        outcome,
        done: false,
    };
    Box::pin(stream::unfold(state, |mut st| async move {
        if st.done {
            return None;
        }
        match st.stream.next().await {
            Some(Ok(chunk)) => {
                st.seen += chunk.len() as u64;
                if st.seen > st.limit {
                    st.done = true;
                    let err = CoreError::PayloadTooLarge(format!(
                        "artifact exceeds the {} byte limit",
                        st.limit
                    ));
                    return Some((Err(err), st));
                }
                if let Some(v) = st.verifier.as_mut() {
                    v.update(&chunk);
                }
                Some((Ok(chunk), st))
            }
            Some(Err(e)) => {
                st.done = true;
                Some((Err(e), st))
            }
            None => {
                // Clean end of stream: finalize the digest verdict. Returning
                // `None` ends the stream, so `st` is dropped here.
                if let Some(v) = st.verifier.take() {
                    *st.outcome.lock().expect("verifier mutex") = Some(v.finish());
                }
                None
            }
        }
    }))
}
