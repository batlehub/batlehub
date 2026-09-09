use super::{
    artifact_storage_key, AccessEvent, Bytes, CoreError, Identity, LocalRegistryService, PackageId,
    PublishedPackage, StreamExt,
};
use crate::entities::{Action, CallerNet};
use crate::services::authz::filter::Readable;

/// What [`LocalRegistryService::load_visible_versions_reporting`] returned.
///
/// The flag is only meaningful when `versions` is empty; a non-empty listing has
/// nothing to explain.
pub(super) struct VisibleVersions {
    pub versions: Vec<PublishedPackage>,
    /// Something was visible until the grant filter removed the last of it —
    /// §4.4 rule 2 taken to its end. Distinct from "nothing was ever here",
    /// because on a Hybrid registry the two get different answers.
    pub withheld_by_grants: bool,
}

/// Every stored grant row in one registry, fetched once for a whole document.
///
/// # The N+1 this exists to close
///
/// `filter_by_grants` asks two questions per package — the version rows under
/// this name, and the package node — and a whole-registry document asks it once
/// per package. For a caller the config tiers already grant `releases:read` the
/// filter returns before either query, which is almost every caller; but the
/// caller this feature exists *for* is precisely the one those tiers do not
/// satisfy, so the fast path misses exactly the case that matters. On a registry
/// with no version-tier rows at all it was still one wasted `LIKE` per package —
/// the §13.2 shape measured at 806× the cached document.
///
/// The document already fetches both sets registry-wide, to build `Readable` and
/// the cache key. Handing them down turns 2N queries into the 2 that were being
/// issued anyway.
#[derive(Default)]
pub(super) struct RegistryGrantRows {
    package: Vec<crate::ports::StoredGrant>,
    version: Vec<crate::ports::StoredGrant>,
}

impl RegistryGrantRows {
    /// The version rows under `package`, matched on the `package@` boundary —
    /// the same rule `version_grants_for_package` applies in SQL, so the
    /// prefetched path and the querying one cannot disagree about which rows
    /// belong to a name.
    fn versions_of<'a>(&'a self, package: &str) -> Vec<&'a crate::ports::StoredGrant> {
        let prefix = format!("{package}@");
        self.version
            .iter()
            .filter(|g| g.node_key.starts_with(&prefix))
            .collect()
    }

    fn package_rows<'a>(&'a self, package: &str) -> Vec<&'a crate::ports::StoredGrant> {
        self.package
            .iter()
            .filter(|g| g.node_key == package)
            .collect()
    }
}

/// The result of asking the document cache for a whole-registry document.
///
/// A named type rather than `Result<Arc<String>, (String, u64)>`, because the
/// miss arm now carries the [`Readable`] the document must be filtered with and
/// a tuple of three would not say which is which. That coupling is the point:
/// the cache key is a digest of the read set, so handing the two back together
/// is what stops a caller from keying on one resolution and filtering with
/// another.
pub(super) enum DocumentSlot {
    /// A current entry. Serve it as-is.
    Hit(std::sync::Arc<String>),
    Miss {
        /// Where to store the built document. **Empty when there is nothing to
        /// store under** — no cache configured, or no configured hierarchy to
        /// key against — and [`LocalRegistryService::store_document`] treats an
        /// empty key as "do not store".
        key: String,
        /// The registry's generation as of *before* the build.
        generation: u64,
        /// What this caller may read, resolved once.
        readable: Readable,
        /// The registry's stored grant rows, fetched once. Passed back down to
        /// `load_visible_versions_in` so the per-package filter costs no query.
        grants: std::sync::Arc<RegistryGrantRows>,
    },
}

impl DocumentSlot {
    /// A miss that will never be stored: build the document, serve it, keep
    /// nothing.
    fn uncached(readable: Readable, grants: std::sync::Arc<RegistryGrantRows>) -> Self {
        DocumentSlot::Miss {
            key: String::new(),
            generation: 0,
            readable,
            grants,
        }
    }
}

impl LocalRegistryService {
    /// Return the sparse index file content (newline-delimited JSON) for a Cargo crate.
    /// Returns `CoreError::NotFound` if the crate has never been published here.
    pub async fn get_index(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
    ) -> Result<String, CoreError> {
        let versions = self
            .load_visible_versions_or_not_found(registry, name, identity, "crate")
            .await?;
        let lines = versions
            .iter()
            .map(|v| serde_json::to_string(&v.index_metadata))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| CoreError::Registry(e.to_string()))?;
        Ok(lines.join("\n"))
    }

    /// Build an npm packument for all published versions, rewriting `dist.tarball`
    /// to point at `base_url`.
    ///
    /// `base_url` is the **registry's** public base as seen by the requesting
    /// client — `https://npm.acme.io` on a host-routed request,
    /// `https://hub.example.com/proxy/npm1` on the subpath. The web layer builds
    /// it with `registry_public_base`; nothing here re-derives the ingress shape.
    pub async fn get_npm_packument(
        &self,
        registry: &str,
        name: &str,
        base_url: &str,
        identity: &Identity,
    ) -> Result<serde_json::Value, CoreError> {
        let versions = self
            .load_visible_versions_or_not_found(registry, name, identity, "package")
            .await?;

        let base = base_url.trim_end_matches('/');
        let mut versions_map = serde_json::Map::new();
        let mut time_map = serde_json::Map::new();
        let mut latest = String::new();

        for pkg in &versions {
            let mut meta = pkg.index_metadata.clone();
            if let Some(obj) = meta.as_object_mut() {
                let dist = obj.entry("dist").or_insert_with(|| serde_json::json!({}));
                if let Some(d) = dist.as_object_mut() {
                    d.insert(
                        "tarball".to_owned(),
                        serde_json::json!(format!(
                            "{base}/{name}/{version}/tarball",
                            version = pkg.version
                        )),
                    );
                }
            }
            time_map.insert(
                pkg.version.clone(),
                serde_json::json!(pkg.published_at.to_rfc3339()),
            );
            versions_map.insert(pkg.version.clone(), meta);
            if !Self::is_prerelease(&pkg.version) {
                latest = pkg.version.clone();
            }
        }

        // When no stable version is visible, fall back to the newest pre-release so that
        // `dist-tags.latest` is always a valid (non-empty) version string.
        if latest.is_empty() {
            if let Some(p) = versions.last() {
                latest = p.version.clone();
            }
        }

        Ok(serde_json::json!({
            "name": name,
            "_id": name,
            "dist-tags": { "latest": latest },
            "versions": versions_map,
            "time": time_map
        }))
    }

    /// Return a single npm version metadata object with `dist.tarball` rewritten
    /// to point at `base_url` (the registry's public base — see
    /// [`LocalRegistryService::get_npm_packument`]).
    pub async fn get_npm_version(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        base_url: &str,
        identity: &Identity,
    ) -> Result<serde_json::Value, CoreError> {
        self.check_read_access(registry, name, identity).await?;
        self.check_prerelease_access(registry, version, identity)
            .await?;
        let versions = self.backend.get_versions(registry, name).await?;
        let pkg = versions
            .into_iter()
            .find(|v| v.version == version)
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "{}@{} not found in local registry '{}'",
                    name, version, registry
                ))
            })?;

        let base = base_url.trim_end_matches('/');
        let mut meta = pkg.index_metadata.clone();
        if let Some(obj) = meta.as_object_mut() {
            let dist = obj.entry("dist").or_insert_with(|| serde_json::json!({}));
            if let Some(d) = dist.as_object_mut() {
                d.insert(
                    "tarball".to_owned(),
                    serde_json::json!(format!("{base}/{name}/{version}/tarball")),
                );
            }
        }
        Ok(meta)
    }

    /// The gate every local artifact read passes, without reading anything.
    ///
    /// Three checks, in the order a refusal is cheapest: the registry's rule
    /// chain, the package's visibility, then the pre-release gate. A denial of
    /// any of them is recorded as a denied download, so a local refusal appears
    /// in the audit trail exactly as the proxy path's does.
    ///
    /// Handlers that build their own storage key — Maven's multi-file artifacts,
    /// the Terraform provider binary — call this and then read the key. Handlers
    /// that want the plain `{registry}/{name}/{version}` artifact call
    /// [`Self::get_artifact`], which is this plus the read.
    ///
    /// Returns the version row it judged, so the caller's own use of it — the
    /// re-serve checksum, the download-signature check — does not query for it a
    /// second time. `None` means storage may hold bytes this instance has no
    /// metadata for, which the gates treat as "unknown" and a `verify_*` policy
    /// treats as a refusal.
    pub async fn authorize_artifact_read(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        action: Action,
        identity: &Identity,
        net: &CallerNet,
    ) -> Result<Option<PublishedPackage>, CoreError> {
        super::validate_coordinate(name, version, None)?;
        match self
            .read_gates(registry, name, version, action, identity)
            .await
        {
            Ok(row) => Ok(row),
            Err(e) => {
                self.record_download(
                    PackageId::new(registry, name, version),
                    identity,
                    net,
                    Some(e.to_string()),
                )
                .await;
                Err(e)
            }
        }
    }

    /// The three gates [`Self::authorize_artifact_read`] applies, in the order a
    /// refusal is cheapest — and short-circuiting, which is the point of it
    /// being its own function: an array of already-awaited `Result`s runs every
    /// check before the loop can look at the first one, so a caller the rule
    /// chain has already refused would still pay for a visibility lookup and a
    /// pre-release check.
    async fn read_gates(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        action: Action,
        identity: &Identity,
    ) -> Result<Option<PublishedPackage>, CoreError> {
        // The version's own row, because the chain is about to judge it. The
        // gate rules read `published_at` and `is_signed`, and this service holds
        // both — `synthetic_metadata`'s `None`s would make
        // `require_signed_release` and `release_age.deny_missing_timestamp`
        // refuse every artifact in the registry rather than gate any of it. The
        // proxy path resolves upstream metadata before evaluating for the same
        // reason; this is the local half of that.
        //
        // Errors are not swallowed: a repository that cannot answer must not
        // become "unknown metadata" and then, depending on the policy, "allow".
        let row = self
            .backend
            .get_versions(registry, name)
            .await?
            .into_iter()
            .find(|p| p.version == version);
        let pkg_id = PackageId::new(registry, name, version);
        // The registry rule chain — RBAC, block list, release-age, licence and
        // signature gates. It runs here rather than being left to each handler
        // because for a *local* read there is no proxy fall-through to run it
        // later, and eight handlers were found serving bytes without it (the
        // 2026-08-26 survey, findings 4-10). `resource_type` is the caller's:
        // a Go module zip is `source:read` where its `.info` is
        // `releases:read`, and the proxy fall-through distinguishes them the
        // same way.
        match &row {
            Some(pkg) => {
                let metadata = crate::entities::PackageMetadata {
                    id: pkg_id,
                    published_at: Some(pkg.published_at),
                    download_url: None,
                    checksum: Some(pkg.checksum.clone()),
                    // The stored signature, as the download-time verification
                    // reads it: bytes present means this version was published
                    // signed. *Non-empty* bytes — `""` is valid base64, so rows
                    // written before the publish edge rejected an empty
                    // signature hold `Some(vec![])`, and reporting those as
                    // signed hands `require_signed_release` a `true` no key ever
                    // backed.
                    is_signed: Some(
                        pkg.signature_bytes
                            .as_deref()
                            .is_some_and(|b| !b.is_empty()),
                    ),
                    extra: serde_json::Value::Null,
                    cache_control: None,
                };
                crate::services::authz::authorize_read_against(
                    &self.hot, &metadata, identity, action,
                )
                .await?;
            }
            // **No row: the chain runs, minus the two rules that would be
            // judging a version this instance does not have.**
            //
            // A Hybrid registry reaches here for everything it proxies. Judged
            // against `synthetic_metadata`, `release_age.deny_missing_timestamp`
            // and `require_signed_release.deny_missing_signature` read absent as
            // deny — so a registry with either configured would answer `403` to
            // every proxied artifact instead of falling through to the upstream,
            // which resolves the real metadata and runs the same chain on it.
            //
            // Everything else still judges: RBAC, and — the reason this is not
            // simply "rbac only" — `block_list`. An operator blocking a version
            // this instance never published must still see it refused, and
            // refused with `AccessDenied`: `NotFound` is what a Hybrid
            // fall-through reads as "ask upstream". `authorize_unheld_read` owns
            // that split.
            //
            // In Local mode there is no fall-through, and the read that follows
            // finds nothing and answers `404`. Bytes sitting in storage with no
            // row is a store inconsistency, not a publish; `verify_on_serve` and
            // `verify_on_download` both fail closed on it a few lines below.
            None => {
                crate::services::authz::authorize_unheld_read(&self.hot, &pkg_id, identity, action)
                    .await?;
            }
        }
        self.check_visibility(registry, name, identity).await?;
        // Defense in depth: gate pre-release/beta access on the artifact bytes
        // themselves, not just on the metadata endpoints. Most download handlers
        // call `check_prerelease_access` before this, but at least the conda
        // handler reaches here directly — enforcing it for every current and
        // future caller closes that fail-open gap. The check is idempotent, so
        // callers that already gate keep working unchanged.
        self.check_prerelease_access(registry, version, identity)
            .await?;
        Ok(row)
    }

    /// Retrieve the raw artifact bytes for download.
    ///
    /// `resource_type` is the permission the read needs — normally
    /// [`crate::entities::Action::ReleasesRead`], `Action::SourceRead` for the
    /// paths that serve source archives. It is a parameter rather than a
    /// constant so that every call site has to name what it is serving, which
    /// is the same question the proxy fall-through answers for the same route.
    pub async fn get_artifact(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        action: Action,
        identity: &Identity,
        net: &CallerNet,
    ) -> Result<Bytes, CoreError> {
        let row = self
            .authorize_artifact_read(registry, name, version, action, identity, net)
            .await?;
        let key = artifact_storage_key(registry, name, version);
        let artifact = self.storage.retrieve(&key).await?.ok_or_else(|| {
            CoreError::NotFound(format!(
                "{}/{}@{} not found in local registry",
                registry, name, version
            ))
        })?;
        let mut buf = Vec::new();
        let mut stream = artifact.stream;
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk?);
        }
        let bytes = Bytes::from(buf);

        // Per-registry integrity (re-serve checksum) and signature policies.
        let (integrity, signing) = {
            let hot = self.hot.read().await;
            (
                hot.integrity.get(registry).cloned().unwrap_or_default(),
                hot.signing.get(registry).cloned().unwrap_or_default(),
            )
        };

        let verify_checksum = integrity.enabled && integrity.verify_on_serve;
        if verify_checksum || signing.verify_on_download {
            // Both checks need the stored per-version metadata (checksum +
            // signature) — the same row the gate above already judged, so it is
            // reused rather than queried a second time. These are opt-in
            // guarantees that every served byte is verified, so we fail closed:
            // a missing row for bytes that exist in storage is an inconsistency
            // we refuse to serve unverified rather than silently skipping the
            // check. (A lookup *error* never reaches here: `read_gates`
            // propagates it.)
            let meta = row.ok_or_else(|| {
                CoreError::IntegrityFailure(format!(
                    "cannot verify {registry}/{name}@{version}: no published metadata found for stored artifact"
                ))
            })?;
            if verify_checksum {
                self.reverify_checksum_on_serve(registry, name, version, &meta.checksum, &bytes)?;
            }
            if signing.verify_on_download {
                Self::verify_download_signature(
                    registry,
                    name,
                    version,
                    &signing,
                    meta.signature_bytes.as_deref(),
                    meta.signature_type.as_deref(),
                    &bytes,
                )?;
            }
        }

        self.record_download(PackageId::new(registry, name, version), identity, net, None)
            .await;
        Ok(bytes)
    }

    /// [`Self::get_artifact`] for a coordinate whose bytes do **not** live at
    /// `{registry}/{name}/{version}`.
    ///
    /// Maven publishes several files per version (`maven_artifact_storage_key`)
    /// and a Terraform provider one archive per platform
    /// (`terraform_provider_binary_storage_key`), so those handlers compute the
    /// key themselves. Before this existed they also read `storage` themselves,
    /// which is how both ended up serving bytes with no visibility check at all
    /// (survey findings 6 and 7) — the storage handle is public and reading it
    /// directly skips every gate. Going through here instead means the gate is
    /// not something the handler has to remember.
    ///
    /// `Ok(None)` for a key that is not present, so a Hybrid caller can fall
    /// through to the upstream; the gate has already run at that point, which is
    /// deliberate — whether a package exists locally is not a reason to skip
    /// authorizing the caller.
    ///
    /// The re-serve checksum and signature verification `get_artifact` performs
    /// are **not** applied: both compare against the version's single recorded
    /// `checksum`, which describes the primary artifact and not a sibling file.
    ///
    /// The coordinate arrives as a whole [`PackageId`] rather than three strings
    /// because its `artifact` field is load-bearing here: it is the file's own
    /// name within the version — `secret-lib-1.2.3.jar`, `…jar.sha1`,
    /// `acme.crypto.2.1.0.nupkg` — and it decides both the coordinate the audit
    /// trail records (the proxy path has always carried it, via `with_artifact`)
    /// and whether this read counts as a download at all, per
    /// [`PackageId::is_verification_sidecar`]. Leaving `artifact` unset records a
    /// plain download, which is right for a version that is a single file.
    pub async fn get_artifact_at_key(
        &self,
        pkg: &PackageId,
        key: &str,
        action: Action,
        identity: &Identity,
        net: &CallerNet,
    ) -> Result<Option<Bytes>, CoreError> {
        self.authorize_artifact_read(
            &pkg.registry,
            &pkg.name,
            &pkg.version,
            action,
            identity,
            net,
        )
        .await?;
        let Some(stored) = self.storage.retrieve(key).await? else {
            return Ok(None);
        };
        let mut buf = Vec::new();
        let mut stream = stored.stream;
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk?);
        }
        let bytes = Bytes::from(buf);
        self.record_download(pkg.clone(), identity, net, None).await;
        Ok(Some(bytes))
    }

    /// Record a download attempt through `package_repo`, when configured.
    ///
    /// Mirrors `ProxyService::handle`'s `AccessEvent::allowed_download`/`denied_download`
    /// recording so Local/Hybrid-mode reads produce the same audit trail as the
    /// proxy-fallback path, instead of the audit gap this closes: no-op when
    /// `package_repo` is `None` (audit logging is opt-in, matching `quota`/`ownership`).
    ///
    /// `net` is the second half of that mirroring. The proxy path threads the
    /// address and agent through `ProxyRequest`; this path built the event from
    /// the `Identity` alone, which left `audit pulls` reporting a `count` with no
    /// `source_ip` and no `client_user_agent` beside it — on precisely the
    /// deployments that publish their own packages (RFC 0018 §13.10). A caller
    /// with no request behind it passes [`CallerNet::unknown`].
    /// The coordinate arrives as a whole [`PackageId`] rather than as its parts,
    /// which is also what keeps the parameter list readable: the artifact name
    /// is what lets a multi-file version be told apart here, and the proxy path
    /// has always carried it (`with_artifact` in the Maven and NuGet handlers).
    /// Without it every file of a version collapsed onto one coordinate, and
    /// `allowed_read` could not see that a `.sha1` is not a download.
    async fn record_download(
        &self,
        pkg: PackageId,
        identity: &Identity,
        net: &CallerNet,
        denial_reason: Option<String>,
    ) {
        let Some(repo) = self.package_repo.as_ref() else {
            return;
        };
        let event = match denial_reason {
            Some(reason) => AccessEvent::denied_download(
                pkg,
                identity.user_id.clone(),
                identity.role.clone(),
                reason,
            ),
            None => AccessEvent::allowed_read(pkg, identity.user_id.clone(), identity.role.clone()),
        }
        .with_ip_ua(net.ip.clone(), net.user_agent.clone());
        if let Err(e) = repo.record_access(event).await {
            tracing::warn!(error = %e, "audit log write failed for local registry download");
        }
    }

    /// Re-verify stored bytes against the SHA-256 recorded at publish time.
    /// A mismatch means the stored artifact was corrupted or tampered with.
    fn reverify_checksum_on_serve(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        expected: &str,
        bytes: &Bytes,
    ) -> Result<(), CoreError> {
        use crate::services::integrity::{verify, IntegrityOutcome};
        match verify(expected, bytes) {
            IntegrityOutcome::Verified { algo } => {
                metrics::counter!("batlehub_integrity_checks_total", "registry" => registry.to_owned(), "outcome" => "verified", "phase" => "reserve").increment(1);
                tracing::debug!(
                    registry,
                    name,
                    version,
                    algo = algo.as_str(),
                    "local artifact re-verified on serve"
                );
                Ok(())
            }
            IntegrityOutcome::Mismatch {
                algo,
                expected,
                actual,
            } => {
                metrics::counter!("batlehub_integrity_checks_total", "registry" => registry.to_owned(), "outcome" => "mismatch", "phase" => "reserve").increment(1);
                tracing::warn!(registry, name, version, algo = algo.as_str(), %expected, %actual, "local artifact failed re-serve integrity check");
                Err(CoreError::IntegrityFailure(format!(
                    "stored artifact failed integrity check for {registry}/{name}@{version}: {} digest mismatch",
                    algo.as_str(),
                )))
            }
            IntegrityOutcome::Unparseable => {
                metrics::counter!("batlehub_integrity_checks_total", "registry" => registry.to_owned(), "outcome" => "unparseable", "phase" => "reserve").increment(1);
                tracing::warn!(
                    registry,
                    name,
                    version,
                    "stored checksum could not be parsed; serving without re-verification"
                );
                Ok(())
            }
        }
    }

    /// Verify a stored `ed25519` detached signature against the registry's
    /// trusted keys. Non-`ed25519` types and absent signatures are not verified
    /// here (publish-time `signing.required` governs presence).
    fn verify_download_signature(
        registry: &str,
        name: &str,
        version: &str,
        signing: &crate::services::hot_config::SigningConfig,
        sig_bytes: Option<&[u8]>,
        sig_type: Option<&str>,
        bytes: &Bytes,
    ) -> Result<(), CoreError> {
        use crate::services::signature::{verify_ed25519, ED25519_SIG_TYPE};
        let (sig, ty) = match (sig_bytes, sig_type) {
            (Some(sig), Some(ty)) => (sig, ty),
            // No signature at all. Whether an unsigned artifact may exist here is
            // publish-time `signing.required`'s question, and it has already been
            // answered — so this is a skip, not a refusal.
            (None, _) => {
                metrics::counter!("batlehub_signature_checks_total", "registry" => registry.to_owned(), "outcome" => "skipped").increment(1);
                return Ok(());
            }
            // Bytes with no type. This used to take the same branch as "no
            // signature", which made supplying *no* type strictly weaker than
            // supplying a bogus one: `X-Signature-Type: pgp` was refused below,
            // and omitting the header entirely served the artifact unverified
            // (survey finding 13). The publish edge no longer accepts the pair,
            // but rows stored before it did still exist, and this is where they
            // are met.
            (Some(_), None) => {
                metrics::counter!("batlehub_signature_checks_total", "registry" => registry.to_owned(), "outcome" => "mismatch").increment(1);
                tracing::warn!(
                    registry,
                    name,
                    version,
                    "refusing to serve: artifact carries signature bytes with no signature type while verify_on_download is enabled"
                );
                return Err(CoreError::IntegrityFailure(format!(
                    "cannot verify {registry}/{name}@{version}: the stored signature names no \
                     type; refusing to serve unverified"
                )));
            }
        };
        if !ty.eq_ignore_ascii_case(ED25519_SIG_TYPE) {
            // Only Ed25519 is verifiable here (rsa/PGP are banned). We reach this
            // function only when `verify_on_download` is enabled, i.e. the operator
            // asked for every served byte to be verified — so an artifact carrying
            // a signature we *cannot* verify must fail closed, not be waved through
            // as "skipped". (An absent signature is handled above and governed by
            // publish-time `signing.required`.)
            metrics::counter!("batlehub_signature_checks_total", "registry" => registry.to_owned(), "outcome" => "mismatch").increment(1);
            tracing::warn!(
                registry,
                name,
                version,
                signature_type = ty,
                "refusing to serve: artifact signature type is not verifiable (only ed25519 is supported) while verify_on_download is enabled"
            );
            return Err(CoreError::IntegrityFailure(format!(
                "cannot verify {registry}/{name}@{version}: signature type '{ty}' is not supported (only ed25519); refusing to serve unverified"
            )));
        }
        if verify_ed25519(&signing.trusted_keys, sig, bytes) {
            metrics::counter!("batlehub_signature_checks_total", "registry" => registry.to_owned(), "outcome" => "verified").increment(1);
            tracing::debug!(
                registry,
                name,
                version,
                "ed25519 artifact signature verified on download"
            );
            Ok(())
        } else {
            metrics::counter!("batlehub_signature_checks_total", "registry" => registry.to_owned(), "outcome" => "mismatch").increment(1);
            tracing::warn!(
                registry,
                name,
                version,
                "ed25519 artifact signature failed verification against trusted keys"
            );
            Err(CoreError::IntegrityFailure(format!(
                "artifact signature verification failed for {registry}/{name}@{version}"
            )))
        }
    }

    // ── Visibility helpers ────────────────────────────────────────────────────

    /// Check whether `identity` is allowed to access `package` given its
    /// current visibility setting.
    ///
    /// - `Public`   → always allowed (even anonymous).
    /// - `Internal` → requires at least `Role::User`.
    /// - `Team`     → requires membership in the group owning the longest-prefix
    ///   namespace claim covering this package. When **no** claim covers it,
    ///   access is **denied** — see `check_team_visibility`, which refuses rather
    ///   than falling back to `Internal`, so a deleted or never-created claim
    ///   cannot silently widen a team package to every authenticated user.
    ///
    /// Admins bypass all checks. When no `team_namespace` port is configured,
    /// access is always permitted.
    ///
    /// The explore listing applies the same three rules in SQL — see
    /// `LOCAL_VISIBILITY_PREDICATE` in `crates/adapters/src/db/packages/mod.rs`.
    /// The two must stay in agreement: a listing more permissive than this
    /// discloses the names of packages this method would refuse to serve.
    pub async fn check_visibility(
        &self,
        registry: &str,
        package: &str,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        self.authorizer()
            .check_visibility(registry, package, identity)
            .await
    }

    /// Which packages in `registry` this caller may read (RFC 0015 §4.4).
    ///
    /// The **whole configured hierarchy** — the registry node and every
    /// `[[registries.namespaces]]` block — resolved once against this caller, and
    /// then one further query for the registry's package-tier grants, made only
    /// when the config tiers did not already grant the read on everything. Never
    /// one query per package: that is the N+1 phase 0b measured at 806× the
    /// cached document on the M corpus (§13.2), and it is the reason this helper
    /// exists rather than a `resolve` call inside each loop.
    ///
    /// Resolving only the *registry* node — which is what this did first — made
    /// the namespace tier invisible to every whole-registry document, both by
    /// dropping packages a namespace grant reaches and by listing packages a
    /// namespace seal withholds. [`Readable`] carries the reasoning; the tier is
    /// config-declared and its `match` is a string comparison, so there was never
    /// a cost that justified leaving it out.
    ///
    /// A registry with no configured hierarchy answers
    /// [`Readable::Everything`] — the same reading `authorize_grants` takes for
    /// an unknown registry, and for the same reason: that is a routing question
    /// answered `404` by the handler, not an authorization one. Filtering a
    /// document to nothing because a test fixture built no grants would be a
    /// refusal wearing an empty document's clothes.
    pub(super) async fn readable_packages(
        &self,
        registry: &str,
        identity: &Identity,
    ) -> Result<(Readable, std::sync::Arc<RegistryGrantRows>), CoreError> {
        use crate::entities::{Action, Subject};
        use crate::services::authz::filter::Readable;

        let (grants, instance, repo) = {
            let hot = self.hot.read().await;
            (
                hot.grants.get(registry).cloned(),
                hot.instance.clone(),
                hot.grant_repo.clone(),
            )
        };
        let Some(grants) = grants else {
            return Ok((Readable::Everything, Default::default()));
        };

        let subject = Subject::Identity(identity.clone());
        let readable =
            Readable::from_registry(instance.as_deref(), &grants, Action::ReleasesRead, &subject);

        // The fast path fetches nothing: a caller the config tiers already grant
        // the read to has no package- or version-tier row that could widen it
        // further.
        let (true, Some(repo)) = (readable.needs_package_grants(), repo) else {
            return Ok((readable, Default::default()));
        };
        let package_rows = repo.package_grants_in_registry(registry).await?;
        let rows = package_rows
            .iter()
            .cloned()
            .map(|g| (g.node_key, g.subject, g.actions));

        let readable = readable.with_package_grants(rows, Action::ReleasesRead, &subject);

        // The version tier, for the same reason it is applied on the keyed path
        // above: this set gates the name, and a name dropped here never reaches
        // the per-version filter. Both paths must answer alike — a document
        // built with a cache and one built without differing in *what they show*
        // would be the cache deciding authorization.
        let version_rows = repo.version_grants_in_registry(registry).await?;
        let version_keys: Vec<&str> = version_rows
            .iter()
            .filter(|g| g.actions.contains(&Action::ReleasesRead) && g.subject.matches(&subject))
            .map(|g| g.node_key.as_str())
            .collect();
        let readable = readable.with_version_grants(version_keys);

        Ok((
            readable,
            std::sync::Arc::new(RegistryGrantRows {
                package: package_rows,
                version: version_rows,
            }),
        ))
    }

    /// Every whole-registry document for `registry` is now stale.
    ///
    /// Called by every write. A generation bump rather than a TTL, because a TTL
    /// heals eventually and a resolver does not wait: conda's
    /// `repodata.json.zst` was keyed on a fingerprint a publish did not change,
    /// and a client that had probed the channel once kept being served
    /// pre-publish bytes while the uncompressed document showed the new package
    /// (`publish_traversal_guards.rs`). This is that bug made unrepresentable.
    pub(super) async fn invalidate_documents(&self, registry: &str) {
        let cache = { self.hot.read().await.document_cache.clone() };
        if let Some(cache) = cache {
            cache.invalidate_registry(registry).await;
        }
    }

    /// A cached whole-registry document, or everything needed to build and
    /// store one.
    ///
    /// The generation in [`DocumentSlot::Miss`] is read *before* the document is
    /// built, so a publish landing mid-render invalidates the result rather than
    /// being stamped with a value that postdates it.
    ///
    /// # The read set is resolved here, not by the caller
    ///
    /// The key is a digest of what the caller may see (see
    /// [`DocumentAudience`]), so the slot hands back the very
    /// [`Readable`] the document must be filtered with. A caller that resolved
    /// its own would be a second answer to one question, and the two would be
    /// free to disagree — which is exactly the disagreement the key exists to
    /// make impossible.
    ///
    /// # One query the fast path did not used to make
    ///
    /// [`Self::readable_packages`] skips the package-tier query whenever the
    /// broad tiers already grant the read, and that is still right *for
    /// filtering*: grants only widen, so there is nothing left to filter.
    /// It is not right for the **key**. §4.5's `private` visibility drops
    /// everything inherited and admits only a grant written on the package
    /// itself, so two callers that the fast path collapses into
    /// [`Readable::Everything`] — both hold the registry-tier read, neither has
    /// anything to filter — are still entitled to different documents. The rows
    /// have to be fetched to know which. One indexed query per whole-registry
    /// request, over rows that are few because an operator wrote each one
    /// deliberately; the N+1 phase 0b measured at 806× was a query *per
    /// package*, and this is not that.
    pub(super) async fn cached_document(
        &self,
        registry: &str,
        document: &str,
        identity: &Identity,
    ) -> Result<DocumentSlot, CoreError> {
        use crate::entities::Subject;
        use crate::services::authz::filter::{document_cache_key, DocumentAudience, Readable};

        let (grants, instance, repo, cache, beta) = {
            let hot = self.hot.read().await;
            (
                hot.grants.get(registry).cloned(),
                hot.instance.clone(),
                hot.grant_repo.clone(),
                hot.document_cache.clone(),
                hot.beta_channel.get(registry).cloned(),
            )
        };

        let subject = Subject::Identity(identity.clone());
        let Some(grants) = grants else {
            // No configured hierarchy — `readable_packages` answers
            // `Everything` here for the same reason, and there is no node to
            // key against.
            return Ok(DocumentSlot::uncached(
                Readable::Everything,
                Default::default(),
            ));
        };
        let Some(cache) = cache else {
            // No cache: build it every time. A missing cache is a slow answer,
            // never a wrong one — and with nothing to key, the read set is all
            // that is wanted, on `readable_packages`' own fast path.
            let (readable, grants) = self.readable_packages(registry, identity).await?;
            return Ok(DocumentSlot::uncached(readable, grants));
        };

        let mut readable =
            Readable::from_registry(instance.as_deref(), &grants, Action::ReleasesRead, &subject);
        let mut listable =
            Readable::from_registry(instance.as_deref(), &grants, Action::ReleasesList, &subject);

        // One fetch, three consumers: the read set, the list set, and §4.5's
        // `private` audience. Fetching it once is also what keeps them
        // consistent — three queries could observe three different states.
        let mut local_read_grants: Vec<String> = Vec::new();
        // §4.4 rule 3 — the version tier changes these bytes too, because
        // `filter_by_grants` consults it for every version of every package this
        // document lists. Keyed on the node keys that grant *this* caller the
        // read, so two callers differing by one version row cannot share an
        // entry.
        let mut version_read_grants: Vec<String> = Vec::new();
        let mut fetched = RegistryGrantRows::default();
        if let Some(repo) = repo {
            fetched.package = repo.package_grants_in_registry(registry).await?;
            let rows: Vec<(String, _, Vec<Action>)> = fetched
                .package
                .iter()
                .cloned()
                .map(|g| (g.node_key, g.subject, g.actions))
                .collect();
            local_read_grants.extend(
                rows.iter()
                    .filter(|(_, matcher, actions)| {
                        actions.contains(&Action::ReleasesRead) && matcher.matches(&subject)
                    })
                    .map(|(name, _, _)| name.clone()),
            );
            local_read_grants.sort();
            local_read_grants.dedup();

            readable = readable.with_package_grants(rows.clone(), Action::ReleasesRead, &subject);
            listable = listable.with_package_grants(rows, Action::ReleasesList, &subject);

            fetched.version = repo.version_grants_in_registry(registry).await?;
            version_read_grants.extend(
                fetched
                    .version
                    .iter()
                    .filter(|g| {
                        g.actions.contains(&Action::ReleasesRead) && g.subject.matches(&subject)
                    })
                    .map(|g| g.node_key.clone()),
            );
            version_read_grants.sort();
            version_read_grants.dedup();

            // The outer gate of every whole-registry document is
            // `readable.contains(name)`, and until this line the version tier
            // never reached it: a caller whose only grant was on one version was
            // dropped at the name, before `load_visible_versions` — and so
            // before `filter_by_grants` — could put that version back. See
            // `Readable::with_version_grants`.
            readable = readable.with_version_grants(version_read_grants.iter().map(String::as_str));
        }

        let beta_member = match beta {
            Some(port) => port.is_member(registry, identity).await?,
            None => false,
        };
        let audience = DocumentAudience::new(
            identity,
            &readable,
            &listable,
            &local_read_grants,
            &version_read_grants,
            beta_member,
        );
        let key = document_cache_key(&format!("{registry}/{document}"), &audience);

        let generation = cache.generation(registry).await;
        match cache.get(registry, &key).await {
            Some(body) => Ok(DocumentSlot::Hit(body)),
            None => Ok(DocumentSlot::Miss {
                key,
                generation,
                readable,
                grants: std::sync::Arc::new(fetched),
            }),
        }
    }

    /// Store a document built after `generation` was read.
    pub(super) async fn store_document(&self, key: String, body: &str, generation: u64) {
        if key.is_empty() {
            return;
        }
        let cache = { self.hot.read().await.document_cache.clone() };
        if let Some(cache) = cache {
            cache
                .put(key, std::sync::Arc::new(body.to_owned()), generation)
                .await;
        }
    }

    /// Everything RFC 0015 §4.1 says applies to one coordinate, composed.
    ///
    /// Delegates to [`resolve_policy`](crate::services::authz::resolve_policy),
    /// which is where it lives so that `explain` (§4.8) can answer the same
    /// question without a `LocalRegistryService` — a diagnostic that resolved
    /// policy by a second route is a diagnostic that can disagree with the thing
    /// it describes, which §11.6 calls worse than none.
    pub(super) async fn resolve_policy(
        &self,
        registry: &str,
        package: &str,
        version: Option<&str>,
    ) -> Result<crate::entities::ResolvedPolicy, CoreError> {
        crate::services::authz::resolve_policy(&self.hot, registry, package, version).await
    }

    /// The [`Authorizer`](crate::services::authz::Authorizer) for this service.
    ///
    /// Built from the handles this service already holds rather than stored
    /// beside them. Storing it would mean two owners of the same ports, and a
    /// `LocalRegistryService` whose `team_namespace` was swapped after
    /// construction — which `authz_matrix.rs`'s visibility fixture does, and so
    /// does `make_local_cargo_ownership_app` — would keep authorizing against
    /// the old one. Two answers to one question is the defect the funnel exists
    /// to remove; a cheap constructor is a small price for not reintroducing it
    /// one layer down.
    pub fn authorizer(&self) -> crate::services::authz::Authorizer {
        crate::services::authz::Authorizer::new(self.hot.clone(), self.team_namespace.clone())
    }

    /// Locally published packages matching `query`, as search hits the caller is
    /// allowed to know exist.
    ///
    /// The web layer used to build this itself from `list_package_names` — a
    /// bare `SELECT DISTINCT name FROM local_packages`, with no visibility
    /// check, no `unlisted` filter and no identity filter — so a search returned
    /// every private package name in the registry to anyone who asked, including
    /// callers the same registry answers `403` to on the package itself (survey
    /// finding 11).
    ///
    /// It goes through [`Self::load_visible_versions`] per name instead: the same
    /// funnel every other local listing uses, so a name is included only if this
    /// caller could have listed that package directly. A name whose versions are
    /// all unlisted, all blocked, or invisible to this identity simply has
    /// nothing to report and is skipped — a search reports what the caller may
    /// see, and reports it the same way the listing endpoints do.
    ///
    /// `AccessDenied` is a skip rather than an error for the same reason
    /// `get_jetbrains_plugins` skips: a listing that refuses wholesale because
    /// one of its candidates is private tells the caller that a private package
    /// exists, which is the disclosure being closed.
    pub async fn search_local(
        &self,
        registry: &str,
        query: &str,
        limit: usize,
        identity: &Identity,
    ) -> crate::services::search::LocalSearch {
        let names = self
            .backend
            .list_package_names(registry)
            .await
            .unwrap_or_default();
        // Every published name, unfiltered — never returned to a client, only
        // used by `ProxyService::search` to recognise which of its *held*
        // packages are locally published and therefore governed by the hits
        // below rather than by the proxy's access log. See `LocalSearch`.
        let all_names: std::collections::HashSet<String> = names.iter().cloned().collect();

        let q = query.to_lowercase();
        // One fetch for the whole search rather than one per hit — search walks
        // every published name, which is the widest of the loops. See
        // `RegistryGrantRows`.
        // Degrades to the querying path rather than failing the search: this
        // function returns no `Result`, and a grants blip must cost speed, not
        // results. `filter_by_grants` still fails closed per package.
        let grant_rows = self
            .readable_packages(registry, identity)
            .await
            .map(|(_, rows)| rows)
            .unwrap_or_default();
        let mut out = Vec::new();
        for name in names {
            if !q.is_empty() && !name.to_lowercase().contains(&q) {
                continue;
            }
            let Ok(versions) = self
                .load_visible_versions_in(registry, &name, identity, Some(&grant_rows))
                .await
            else {
                continue;
            };
            // The newest visible version, so a hit names something this caller
            // can actually install rather than an empty string — which is what
            // the NuGet local branch used to emit, and what made its
            // `versions[]` unusable.
            let Some(version) = versions.last().map(|p| p.version.clone()) else {
                continue;
            };
            out.push(crate::services::search::SearchHit {
                name,
                version,
                description: None,
            });
            if out.len() >= limit {
                break;
            }
        }
        crate::services::search::LocalSearch {
            hits: out,
            all_names,
        }
    }

    /// The gate every local **document** read passes: the registry's rule chain
    /// first, then the package's own visibility.
    ///
    /// The two halves fail independently, and the survey found both halves
    /// missing in different places — the rule chain on conda, jetbrains, pypi,
    /// goproxy and the terraform listings; visibility on maven, nuget and the
    /// terraform provider binary. Pairing them in one method is what stops the
    /// next reader from adding one and believing they added both.
    ///
    /// Only the identity-keyed `rbac` rule runs, not the full chain: this
    /// authorises a *listing*, and the gate rules judge a concrete version that
    /// a listing does not name — see
    /// [`crate::services::authz::authorize_listing`]. The full chain
    /// still runs on the download that follows, in
    /// [`Self::authorize_artifact_read`].
    ///
    /// `check_visibility` stays separately public because the console's explore
    /// endpoints call it on their own authorization path, where the registry's
    /// *client-facing* RBAC is not the gate that governs.
    pub(super) async fn check_read_access(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        // RFC 0015 §4.2 — a listing's verb is `releases:list`, by definition of what a
        // listing is: *"version documents, protocol indexes and search results —
        // including the cargo sparse index, which is a version listing whatever its URL
        // suggests."*
        //
        // Requested at the **funnel** rather than at 76 handler call sites, and that is
        // not a shortcut: `authorize_listing` exists precisely because a listing names no
        // concrete version (§5.1), so every path that reaches it is a listing and no path
        // that is not reaches it. Classifying the handlers by hand would be restating the
        // funnel's own definition 76 times, with 76 chances to disagree with it.
        //
        // §10 rule 4 is what makes this safe on upgrade: *"any subject holding
        // `releases:read` **or** `source:read` gains `releases:list`"*, because both of
        // today's verbs authorise some listing document and which one varies by ecosystem
        // rather than by intent. So no translated config loses a document; the estates
        // that change are the ones that wrote a grants block distinguishing the two,
        // which is the whole point of the verb.
        crate::services::authz::authorize_listing(
            &self.hot,
            &PackageId::new(registry, name, "latest"),
            identity,
            crate::entities::Action::ReleasesList,
        )
        .await?;
        self.check_visibility(registry, name, identity).await
    }

    // ── Beta channel helpers ──────────────────────────────────────────────────

    /// Returns `true` when `version` is a pre-release.
    ///
    /// Delegates to the free [`is_prerelease`](super::is_prerelease), which is
    /// where the definition lives now that it has a consumer outside this module
    /// (RFC 0015 §6.1). Kept as an associated function because this module calls
    /// it as `Self::is_prerelease` in several places and the indirection is free.
    pub(super) fn is_prerelease(version: &str) -> bool {
        super::is_prerelease(version)
    }

    /// Filter `versions` to remove pre-release entries when `identity` is not a
    /// beta-channel member and a beta channel is configured for `registry`.
    async fn filter_for_identity(
        &self,
        registry: &str,
        versions: Vec<PublishedPackage>,
        identity: &Identity,
    ) -> Result<Vec<PublishedPackage>, CoreError> {
        let beta_port = self.hot.read().await.beta_channel.get(registry).cloned();
        let Some(beta_port) = beta_port else {
            return Ok(versions);
        };
        if beta_port.is_member(registry, identity).await? {
            return Ok(versions);
        }
        Ok(versions
            .into_iter()
            .filter(|p| !Self::is_prerelease(&p.version))
            .collect())
    }

    /// Drop versions this caller holds no `releases:read` on — RFC 0015 §4.4
    /// rule 2's second half, and [RFC 0017]'s phase 2.
    ///
    /// [RFC 0017]: https://batleforc.git.batleforc.fr/batlehub/rfc/0017-writing-grants-at-the-package-and-version-tiers
    ///
    /// # Why this is inert on every estate that has not used the editor
    ///
    /// Grants only widen (§4.3), so with **no version-tier row** a caller's read
    /// verdict is uniform across every version of a package: whatever the
    /// package tier answers is the answer for all of them, and a per-version
    /// filter would remove nothing or everything. The one query below is what
    /// establishes that, and an empty result returns the input untouched — which
    /// is RFC 0017 §9's promise that no estate's listings change until an
    /// operator writes the first version-tier grant.
    ///
    /// # Why it filters rather than refuses
    ///
    /// The funnel has already run `check_read_access`, which asks
    /// `releases:list`. A caller holding the list but not the read on some
    /// versions is exactly §4.4's opening configuration, and the answer is a
    /// shorter document rather than a `403` — the rule RFC 0006 established for
    /// blocked versions, applied to the same documents at the same point.
    ///
    /// # One query per package, never one per version
    ///
    /// [`GrantRepository::version_grants_for_package`] fetches every version row
    /// under this name at once. Asking `grants_for` per version would be the N+1
    /// §13.2 measured at 806×, on a path a package with 400 versions walks.
    ///
    /// Fails **closed** on a repository error, unlike `filter_blocked` beside it,
    /// and the asymmetry is deliberate: a blocking blip that shows one version
    /// too many is a policy miss the download gate still catches, while a grants
    /// blip that shows every version is the §2.3 disclosure this filter exists to
    /// prevent. The error propagates rather than degrading to a wider document.
    async fn filter_by_grants(
        &self,
        registry: &str,
        name: &str,
        versions: Vec<PublishedPackage>,
        identity: &Identity,
        prefetched: Option<&RegistryGrantRows>,
    ) -> Result<Vec<PublishedPackage>, CoreError> {
        use crate::entities::{Action, GrantMap, Node, Subject, Tier};
        use crate::services::authz::filter::{filter_listing, package_visibility};

        if versions.is_empty() {
            return Ok(versions);
        }
        let (repo, grants, instance) = {
            let hot = self.hot.read().await;
            (
                hot.grant_repo.clone(),
                hot.grants.get(registry).cloned(),
                hot.instance.clone(),
            )
        };
        let (Some(repo), Some(grants)) = (repo, grants) else {
            return Ok(versions);
        };

        let subject = Subject::Identity(identity.clone());

        // ── the fast path, and it costs no query ─────────────────────────────
        //
        // The config tiers alone. A caller they already grant `releases:read` to
        // holds it on every version beneath — grants only widen — so no row this
        // function could fetch would remove one, and the whole document stands.
        // That is the overwhelmingly common caller, and it must not pay a query
        // per package on a document that walks every package in the registry.
        let mut path: Vec<Node> = Vec::new();
        if let Some(instance) = instance.as_deref() {
            path.push(instance.clone());
        }
        path.extend(grants.path_for(name));
        if crate::entities::resolve(&path, &subject).holds(Action::ReleasesRead) {
            return Ok(versions);
        }

        // Prefetched when a whole-registry document is walking every package;
        // queried when this is a single-coordinate read, which pays one query
        // rather than N. Both go through the same `package@` boundary rule.
        let owned;
        let rows: Vec<&crate::ports::StoredGrant> = match prefetched {
            Some(pre) => pre.versions_of(name),
            None => {
                owned = repo.version_grants_for_package(registry, name).await?;
                owned.iter().collect()
            }
        };
        if rows.is_empty() {
            // Nothing to differ from the package-tier answer, so nothing to
            // filter — RFC 0017 §9's promise, and the state of every estate that
            // has not used the editor.
            return Ok(versions);
        }

        // Only now is the package node worth a query: it can carry the read for
        // this caller, and if it does the document again stands whole.
        if let Some(package_node) = self.package_node(registry, name, &repo, prefetched).await? {
            path.push(package_node);
            if crate::entities::resolve(&path, &subject).holds(Action::ReleasesRead) {
                return Ok(versions);
            }
        }

        // Every row for a version folded into *one* `GrantMap`, exactly as
        // `chain::stored_nodes` folds them: one node per tier, carrying every
        // subject written on it. Keeping one row per version instead would drop
        // all but one subject's grant — and which one survived would depend on
        // the order the repository happened to return, so a version granted to
        // two subjects would admit an arbitrary one of them.
        let version_prefix = format!("{name}@");
        let mut by_version: std::collections::HashMap<&str, GrantMap> =
            std::collections::HashMap::new();
        for g in rows {
            let Some(v) = g.node_key.strip_prefix(&version_prefix) else {
                continue;
            };
            let map = by_version.entry(v).or_default();
            *map = std::mem::take(map).grant(g.subject.clone(), g.actions.clone());
        }

        let outcome = filter_listing(versions, |p: &PublishedPackage| {
            let mut nodes = path.clone();
            if let Some(map) = by_version.get(p.version.as_str()) {
                nodes.push(Node::new(
                    Tier::Version,
                    format!("version:{}", p.version),
                    Some(map.clone()),
                ));
            }
            package_visibility(
                &crate::entities::resolve(&nodes, &subject),
                Action::ReleasesRead,
            )
        });

        if outcome.withheld() > 0 {
            tracing::debug!(
                registry,
                package = name,
                withheld = outcome.withheld(),
                "version listing filtered by grants"
            );
        }
        Ok(outcome.into_items())
    }

    /// The package-tier node for `name`, or `None` when nothing is written on it.
    ///
    /// Mirrors `chain::stored_nodes`' own rule: a tier with no rows contributes
    /// *inherit*, never an empty map — an empty map is a seal, and a seal here
    /// would stop the registry's grants reaching every package that has none of
    /// its own.
    async fn package_node(
        &self,
        registry: &str,
        name: &str,
        repo: &std::sync::Arc<dyn crate::ports::GrantRepository>,
        prefetched: Option<&RegistryGrantRows>,
    ) -> Result<Option<crate::entities::Node>, CoreError> {
        use crate::entities::{GrantMap, Node, Tier};

        let owned;
        let rows: Vec<&crate::ports::StoredGrant> = match prefetched {
            Some(pre) => pre.package_rows(name),
            None => {
                owned = repo
                    .grants_on_node(registry, crate::ports::NodeKind::Package, name)
                    .await?;
                owned.iter().collect()
            }
        };
        if rows.is_empty() {
            return Ok(None);
        }
        let mut map = GrantMap::new();
        for g in rows {
            map = map.grant(g.subject.clone(), g.actions.clone());
        }
        Ok(Some(Node::new(
            Tier::Package,
            format!("package:{name}"),
            Some(map),
        )))
    }

    /// Drop unlisted versions. Unlisted versions are hidden from registry-protocol
    /// listings (index, packument, version lists) but remain downloadable by exact
    /// coordinate (see [`Self::get_artifact`], which is keyed directly, not filtered).
    fn filter_unlisted(versions: Vec<PublishedPackage>) -> Vec<PublishedPackage> {
        versions.into_iter().filter(|p| !p.unlisted).collect()
    }

    /// Drop versions an administrator has blocked.
    ///
    /// Sits alongside `filter_unlisted` and applies the same rule: hidden from
    /// listings, still reachable by exact coordinate — where `get_artifact` runs
    /// the block gate and returns the operator's stated reason. Hiding governs
    /// which version a resolver *picks*; the download gate governs whether it
    /// may have the one it asked for.
    ///
    /// Fails **open** on a repository error, matching
    /// [`crate::rules::BlockListRule`]: a database blip should degrade to
    /// showing more versions than intended, not to reporting every package as
    /// missing. The download gate is the backstop — it re-checks the concrete
    /// coordinate, and denies when the store recovers.
    async fn filter_blocked(
        &self,
        registry: &str,
        name: &str,
        versions: Vec<PublishedPackage>,
    ) -> Vec<PublishedPackage> {
        let Some(repo) = self.package_repo.as_ref() else {
            return versions;
        };
        let blocked = match repo.blocked_versions(registry, name).await {
            Ok(b) if b.is_empty() => return versions,
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    registry = %registry,
                    package = %name,
                    error = %e,
                    "failed to load blocked versions for listing, failing open"
                );
                return versions;
            }
        };
        let blocked: std::collections::HashSet<&str> = blocked.iter().map(String::as_str).collect();
        versions
            .into_iter()
            .filter(|p| !blocked.contains(p.version.as_str()))
            .collect()
    }

    /// Convenience wrapper: `check_visibility` → `get_versions` →
    /// `filter_unlisted` → `filter_blocked` → `filter_for_identity`.
    ///
    /// The single chokepoint every local/hybrid version listing goes through —
    /// npm packument, Maven, NuGet, RubyGems, Go, JetBrains, Terraform and
    /// Composer all resolve their version sets here, so a filter added here
    /// applies to every ecosystem at once.
    ///
    /// Returns the filtered list (may be empty — callers decide whether that is an error).
    pub(super) async fn load_visible_versions(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
    ) -> Result<Vec<PublishedPackage>, CoreError> {
        Ok(self
            .load_visible_versions_reporting(registry, name, identity)
            .await?
            .versions)
    }

    /// [`Self::load_visible_versions`], and — when the answer is empty — whether
    /// the **grant** filter is what emptied it.
    ///
    /// The distinction only matters on the empty path, and there it is a
    /// security boundary rather than a diagnostic: see
    /// [`CoreError::NotFoundWithheld`]. Every other filter in this funnel either
    /// has its own answer for "it emptied the list" (`filter_blocked`, through
    /// `emptied_by_blocking`) or leaves a set that was already empty before it
    /// ran.
    pub(super) async fn load_visible_versions_reporting(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
    ) -> Result<VisibleVersions, CoreError> {
        self.load_visible_versions_reporting_in(registry, name, identity, None)
            .await
    }

    /// [`Self::load_visible_versions`] with the registry's grant rows already in
    /// hand.
    ///
    /// For the whole-registry documents, which walk every package: see
    /// [`RegistryGrantRows`] for why the per-package queries had to go.
    pub(super) async fn load_visible_versions_in(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
        rows: Option<&RegistryGrantRows>,
    ) -> Result<Vec<PublishedPackage>, CoreError> {
        Ok(self
            .load_visible_versions_reporting_in(registry, name, identity, rows)
            .await?
            .versions)
    }

    pub(super) async fn load_visible_versions_reporting_in(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
        rows: Option<&RegistryGrantRows>,
    ) -> Result<VisibleVersions, CoreError> {
        self.check_read_access(registry, name, identity).await?;
        let versions = self.backend.get_versions(registry, name).await?;
        let versions = Self::filter_unlisted(versions);
        let versions = self.filter_blocked(registry, name, versions).await;
        let versions = self
            .filter_for_identity(registry, versions, identity)
            .await?;
        // Measured across the grant filter alone. "Something was here and §4.4
        // rule 2 took the last of it" is the fact the caller needs; "the set was
        // already empty" is not, and conflating them would turn every genuinely
        // absent package on a Hybrid registry into one that never falls through.
        let before = versions.is_empty();
        let versions = self
            .filter_by_grants(registry, name, versions, identity, rows)
            .await?;
        Ok(VisibleVersions {
            withheld_by_grants: !before && versions.is_empty(),
            versions,
        })
    }

    /// `load_visible_versions`, turning an empty result into an error.
    ///
    /// **Which** error matters, and the distinction is a security boundary, not
    /// a cosmetic one. On a Hybrid registry the web layer treats `NotFound` as
    /// "we do not have this, ask upstream"
    /// (`handlers::proxy::common::serve_local_or_proxy_document`). So if an
    /// operator blocks every version of an internal package `acme-auth` and this
    /// returned `NotFound`, the fall-through would answer with the *public*
    /// `acme-auth` from npmjs — serving the substitution the block exists to
    /// prevent, and turning a deliberate block into a dependency-confusion
    /// vector.
    ///
    /// An all-blocked *local* package therefore reports `AccessDenied`, which
    /// the web layer surfaces as `403` and never falls through. "Never published
    /// here" keeps its `NotFound`, so genuine hybrid fall-through is unaffected.
    ///
    /// `entity_label` is used in the error message, e.g. `"crate"`, `"gem"`, `"module"`.
    pub(super) async fn load_visible_versions_or_not_found(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
        entity_label: &str,
    ) -> Result<Vec<PublishedPackage>, CoreError> {
        let outcome = self
            .load_visible_versions_reporting(registry, name, identity)
            .await?;
        if outcome.versions.is_empty() {
            // Only consulted on the empty path, so the common case pays nothing.
            if self.emptied_by_blocking(registry, name).await {
                return Err(CoreError::AccessDenied(format!(
                    "every version of {entity_label} '{name}' in registry '{registry}' \
                     is administratively blocked"
                )));
            }
            // RFC 0017 opened a second way to empty a listing, and it arrives at
            // this line with `emptied_by_blocking` false. A plain `NotFound`
            // here is the dependency-confusion fall-through the paragraph above
            // describes, reached through §4.4 rule 2 instead of through a block:
            // an internal package whose versions this caller may not read would
            // be answered with the *public* package of the same name.
            //
            // 404 to the client either way — hidden means absent — but not the
            // variant the Hybrid handlers fall through on.
            if outcome.withheld_by_grants {
                return Err(CoreError::NotFoundWithheld(format!(
                    "{entity_label} '{name}' not found in local registry '{registry}'"
                )));
            }
            return Err(CoreError::NotFound(format!(
                "{entity_label} '{name}' not found in local registry '{registry}'"
            )));
        }
        Ok(outcome.versions)
    }

    /// Whether this instance holds `name` locally *and* administrative blocks
    /// are what left its listing empty.
    ///
    /// "Some version of this name is blocked" is a different fact and must not
    /// be mistaken for this one. Blocks live in `package_statuses`, which is
    /// keyed on registry + name + version and records blocks on **proxied**
    /// versions too — so on a Hybrid registry, blocking one bad version of an
    /// upstream package that was never published here would otherwise report
    /// `AccessDenied` and refuse the whole document, where the correct answer is
    /// upstream's document with that one version stripped (which is exactly what
    /// [`crate::services::ProxyService::version_document`] does).
    ///
    /// Requiring a non-empty *local* set that blocking covers entirely keeps the
    /// `AccessDenied` for the case it was introduced for — an internal package
    /// whose every published version is blocked, which must never fall through
    /// to a public package of the same name.
    async fn emptied_by_blocking(&self, registry: &str, name: &str) -> bool {
        let Some(repo) = self.package_repo.as_ref() else {
            return false;
        };
        // Listed versions only: an unlisted version is hidden from listings by
        // policy, not by a block, so it cannot make this an "all blocked" case.
        let local = match self.backend.get_versions(registry, name).await {
            Ok(v) => Self::filter_unlisted(v),
            // Fails closed onto `NotFound`, matching `filter_blocked`'s own
            // fail-open stance: a storage blip must not manufacture a `403`.
            Err(_) => return false,
        };
        if local.is_empty() {
            return false;
        }
        let Ok(blocked) = repo.blocked_versions(registry, name).await else {
            return false;
        };
        let blocked: std::collections::HashSet<&str> = blocked.iter().map(String::as_str).collect();
        local.iter().all(|p| blocked.contains(p.version.as_str()))
    }

    /// Picks the newest non-prerelease version, falling back to the overall newest
    /// version if every entry is a pre-release. `versions` must be sorted ascending
    /// (oldest first), as returned by `load_visible_versions`.
    pub(super) fn latest_stable_or_newest(
        versions: &[PublishedPackage],
    ) -> Option<PublishedPackage> {
        versions
            .iter()
            .rev()
            .find(|v| !Self::is_prerelease(&v.version))
            .or_else(|| versions.last())
            .cloned()
    }

    /// Returns `CoreError::NotFound` if `version` is a pre-release and the caller
    /// is not a beta-channel member for `registry`.
    pub async fn check_prerelease_access(
        &self,
        registry: &str,
        version: &str,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        self.authorizer()
            .check_prerelease_access(registry, version, identity)
            .await
    }

    /// Look up the metadata for a specific published version.
    /// Returns `None` if not found (non-fatal — callers may skip signature headers).
    pub async fn get_version_meta(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Option<crate::entities::PublishedPackage> {
        self.backend
            .get_versions(registry, name)
            .await
            .ok()?
            .into_iter()
            .find(|p| p.version == version)
    }
}
