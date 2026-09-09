use super::{
    artifact_storage_key, validate_package_name, validate_path_safe, validate_version, Action,
    CoreError, Identity, LocalRegistryService, PackageId, PublishRequest, PublishedPackage,
    QuotaCheck, StorageMeta, Visibility,
};

/// Everything [`LocalRegistryService::enforce_publish_policy`] needs to know about
/// the artifact being published, grouped so the function takes one parameter for
/// the artifact plus `publisher` (the acting principal, kept separate since it
/// answers a different question — *who*, not *what*).
pub struct PublishPolicyRequest<'a> {
    pub registry: &'a str,
    pub name: &'a str,
    pub version: &'a str,
    pub artifact_len: u64,
    pub signature_bytes: Option<&'a [u8]>,
    pub signature_type: Option<&'a str>,
    /// The storage key these bytes will occupy, for publishers that write to
    /// storage themselves instead of going through the three-phase `publish()`.
    ///
    /// RFC 0015 §4.5's `immutable` asks whether **these bytes** are being
    /// replaced, and for a row-based publish the version row answers that. For a
    /// **multi-file coordinate it does not**: a Maven release is a `.pom`, a
    /// `.jar`, a `-sources.jar` and their checksums, PUT one at a time, so the
    /// row exists from the first of them onward and every later file of the
    /// *same* publish would read as a replacement. Under `immutable = "always"`
    /// that makes publishing a Maven artifact impossible rather than making it
    /// permanent.
    ///
    /// So the multi-file publishers name the key and immutability is decided on
    /// it. `None` keeps the row-based reading, which is right for every
    /// ecosystem whose coordinate is one artifact.
    pub artifact_key: Option<&'a str>,
}

impl LocalRegistryService {
    /// Publish-time half of the signing policy.
    ///
    /// The two headers are independent, so a publisher can send bytes without a
    /// type — and that state used to satisfy `required` (bytes are present) and
    /// skip `allowed_types` (there is no type to check), producing a stored
    /// "signed" artifact whose signature nothing would ever verify. The download
    /// path then read it as *absent* and served the bytes unchecked, so the
    /// perverse rule was that a **bogus** type was refused and **no** type was
    /// accepted (survey finding 13).
    ///
    /// A signature is a pair. Either half alone is incoherent, and this refuses
    /// both orders of it whenever the operator has said anything about signing at
    /// all.
    ///
    /// **Zero bytes is not a signature.** `""` is valid base64, so an empty
    /// `X-Artifact-Signature` used to arrive here as `Some(&[])` — not `None`,
    /// therefore satisfying `required`, and stored as `Some(vec![])`, which the
    /// download path reads back as "this version was published signed" and
    /// `require_signed_release` then allows. Same fail-open as the pair check
    /// above, entered through the length rather than through the type, so both
    /// halves are normalised to `None` before any of the questions are asked.
    fn check_signing_policy(
        signing: &crate::services::hot_config::SigningConfig,
        sig_bytes: Option<&[u8]>,
        sig_type: Option<&str>,
    ) -> Result<(), CoreError> {
        let sig_bytes = sig_bytes.filter(|b| !b.is_empty());
        let sig_type = sig_type.filter(|t| !t.trim().is_empty());
        if signing.required && sig_bytes.is_none() {
            return Err(CoreError::AccessDenied(
                "artifact signature required (X-Artifact-Signature header missing)".into(),
            ));
        }
        match (sig_bytes, sig_type) {
            (Some(_), None) => {
                return Err(CoreError::AccessDenied(
                    "artifact signature supplied without a type (X-Signature-Type header \
                     missing); a signature that names no algorithm can never be verified"
                        .into(),
                ));
            }
            (None, Some(ty)) => {
                return Err(CoreError::AccessDenied(format!(
                    "signature type '{ty}' supplied without a signature \
                     (X-Artifact-Signature header missing)"
                )));
            }
            _ => {}
        }
        if !signing.allowed_types.is_empty() {
            // `None` is now unreachable with bytes present, but the list is still
            // consulted through a match rather than an `if let`: an absent type
            // short-circuiting this check is the exact shape of the finding, and
            // it should not be reintroducible by someone relaxing the pair check
            // above.
            match sig_type {
                Some(st) if signing.allowed_types.iter().any(|t| t == st) => {}
                Some(st) => {
                    return Err(CoreError::AccessDenied(format!(
                        "signature type '{st}' is not in the allowed list"
                    )));
                }
                // No signature at all. Whether that is acceptable is
                // `signing.required`'s question, already answered above.
                None => {}
            }
        }
        Ok(())
    }

    /// RFC 0015 §5.1 — the write half of the decision function.
    ///
    /// Every mutation of a published coordinate goes through here: publish and
    /// overwrite from [`Self::enforce_publish_policy`], yank/unyank/unlist/relist
    /// /deprecate/undeprecate and delete from `lifecycle.rs`. It resolves the same
    /// grant hierarchy the read funnels resolve, over the same tiers, so the
    /// answer to "may this caller write here" comes from one place.
    ///
    /// # This is the whole decision, and the role checks are gone
    ///
    /// §6.1 asks that `has_role_at_least(&Role::User)` be *replaced* by the verb,
    /// and it is: the eight role assertions that used to guard publish and the
    /// lifecycle mutations are deleted, not demoted. A role still decides plenty
    /// here — it is simply decided **inside** the engine, where `role:user` is one
    /// of §4.3's five subject forms and `SubjectMatcher::Role` resolves it with
    /// the same `has_role_at_least` walk. What changed is that a handler can no
    /// longer answer the question itself.
    ///
    /// That was worth doing rather than cosmetic. A role assertion in front of
    /// the engine is indistinguishable from authorization to a reader, and it
    /// silently overrides the config: a hand-written `"*" = ["releases:publish"]`
    /// resolved to *allow* and was then refused by a role gate the operator never
    /// wrote — the "I granted it and nothing happened" failure §4.2 rule 2 exists
    /// to remove, arriving through the check that was supposed to be a backstop.
    ///
    /// The one genuine non-authorization constraint publish had — that a
    /// publisher can be *attributed* — survives as an explicit test for a
    /// `user_id` in [`Self::enforce_publish_policy`], with its own reason.
    ///
    /// **Still a narrowing for every translated config.** §10 rule 5 grants
    /// `role:user` all four write verbs on every local and hybrid registry, and
    /// `SubjectMatcher::Role(User)` matches exactly what `has_role_at_least`
    /// matched — so removing the assertion changes no estate that reached this
    /// code through `[registries.rbac]`. It changes exactly the estates that
    /// wrote a grant saying something else, which is the point.
    pub(super) async fn authorize_write(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        identity: &Identity,
        action: Action,
    ) -> Result<(), CoreError> {
        crate::services::authz::authorize_grants_public(
            &self.hot,
            &PackageId::new(registry, name, version),
            identity,
            action,
        )
        .await
    }

    /// RFC 0015 §6.1 — *"a replace additionally requires
    /// `Action::ReleasesOverwrite` **and** the resource's `immutable` setting to
    /// permit it."* This is the first half; [`Self::check_immutable`] is the
    /// second.
    ///
    /// Scoped to the publishers that can actually replace anything, which is the
    /// same scope §13.6 established for `immutable` and for the same reason:
    /// `LocalRegistryBackend::publish` refuses every republish unconditionally
    /// before any policy is consulted, so on the row-based path there is no
    /// replacement to authorize. What *can* overwrite is the path-addressed
    /// publishers — Maven's non-POM artifacts, deb, rpm — which name their storage
    /// key and write to it directly, and a re-PUT of that key replaces the bytes.
    ///
    /// Asking about the key rather than about the version row is what keeps a
    /// multi-file publish from tripping over itself: a Maven coordinate's `.jar`
    /// is a different key from the `.pom` of the same publish, so only a genuine
    /// re-PUT of the same file needs the second verb.
    async fn check_overwrite_grant(
        &self,
        req: &PublishPolicyRequest<'_>,
        publisher: &Identity,
    ) -> Result<(), CoreError> {
        let Some(key) = req.artifact_key else {
            return Ok(());
        };
        // A storage error is not evidence that the bytes are absent, but it is
        // also not this check's to report: the write that follows will fail on
        // the same backend and say so properly. Reading it as "not a replacement"
        // only ever skips a verb check on a publish that is about to fail anyway.
        if !self.storage.exists(key).await.unwrap_or(false) {
            return Ok(());
        }
        self.authorize_write(
            req.registry,
            req.name,
            req.version,
            publisher,
            Action::ReleasesOverwrite,
        )
        .await
    }

    /// Returns `true` when the package is new (no existing version).
    async fn check_ownership_publish_access(
        &self,
        registry: &str,
        name: &str,
        publisher: &Identity,
    ) -> Result<bool, CoreError> {
        let Some(ref ownership) = self.ownership else {
            return Ok(false);
        };
        let package_exists = self.backend.exists(registry, name).await?;
        if package_exists && !ownership.can_publish(registry, name, publisher).await? {
            return Err(CoreError::AccessDenied(format!(
                "you are not an owner of '{name}' in registry '{registry}'"
            )));
        }
        Ok(!package_exists)
    }

    /// Same ownership gate as [`Self::check_ownership_publish_access`], for
    /// lifecycle mutations (yank/unyank/deprecate/undeprecate/unlist/relist)
    /// on an already-published package rather than a new upload. `can_publish`
    /// already returns `true` for a package with no recorded owners, so this
    /// is a no-op both when ownership tracking is disabled and for unclaimed
    /// packages — it only blocks a `User`-role identity that isn't an owner of
    /// an already-claimed package.
    pub(super) async fn check_ownership_lifecycle_access(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        let Some(ref ownership) = self.ownership else {
            return Ok(());
        };
        if !ownership.can_publish(registry, name, identity).await? {
            return Err(CoreError::AccessDenied(format!(
                "you are not an owner of '{name}' in registry '{registry}'"
            )));
        }
        Ok(())
    }

    /// Enforce the publish-time policy that every registry shares — role,
    /// name/version validation, versioning policy, signing policy, namespace,
    /// ownership, artifact size limit, and quota — *without* committing a
    /// package-version row or storing bytes.
    ///
    /// Path-addressed registries (deb/rpm) host their packages under a custom
    /// storage layout rather than the `{registry}/{name}/{version}` key + DB
    /// version row that [`Self::publish`] manages, so they call this directly
    /// before their own storage work to avoid bypassing the configured limits.
    ///
    /// Returns the post-publish [`QuotaCheck`] (for `X-Quota-*` headers) and
    /// whether this is the first time the package name is seen.
    pub async fn enforce_publish_policy(
        &self,
        req: &PublishPolicyRequest<'_>,
        publisher: &Identity,
    ) -> Result<(QuotaCheck, bool), CoreError> {
        // Reject names/versions that could escape the storage root via path
        // traversal once interpolated into the storage key. Runs unconditionally,
        // independent of the optional versioning policy below.
        validate_package_name(req.name)?;
        validate_path_safe("version", req.version)?;

        // ── RFC 0015 §5.1: may this caller write here? ───────────────────────
        //
        // Ahead of the tombstone check on purpose. A burned coordinate is a fact
        // about the registry's history, and `burned_coordinate_message` names who
        // burned it and when — which is not something to hand to a caller who may
        // not publish here at all. Grants first, then what the coordinate is.
        self.authorize_write(
            req.registry,
            req.name,
            req.version,
            publisher,
            Action::ReleasesPublish,
        )
        .await?;
        // §4.5: "immutability is a property of the resource, the verb is a
        // property of the subject, and a replace needs both". This is the subject
        // half; `check_immutable` below is the resource half.
        self.check_overwrite_grant(req, publisher).await?;

        // ── attributability, which is not an authorization question ──────────
        //
        // `releases:publish` above is the whole of the authorization decision.
        // This is a different question, and it used to be asked as
        // `has_role_at_least(&Role::User)` — a role assertion standing in front
        // of the engine, which reads as authorization and is not.
        //
        // A publish has to be attributable to a principal because
        // `register_initial_owner` records the publisher as the package's first
        // owner, and it can only do that for an identity with an id. Without
        // one it returns early, the package is created with **no owner rows**,
        // and `OwnershipPort::can_publish` answers `true` for a package with no
        // owners — so the coordinate is left permanently publishable by anyone
        // and claimable by nobody. That is survey finding 1's exact shape,
        // created by the publish rather than found in the data.
        //
        // Stated as what it is, so an operator who writes `"*" =
        // ["releases:publish"]` is told why the grant they wrote is not enough
        // rather than being told they lack a role they did not ask about.
        if publisher.user_id.is_none() {
            return Err(CoreError::AccessDenied(
                "publishing requires an identified principal: the publisher is recorded as \
                 the package's first owner, and a package with no owner is publishable by \
                 anyone and claimable by nobody"
                    .into(),
            ));
        }

        // A spent coordinate is refused before anything else is decided
        // (RFC 0016 §4.4). Ahead of the quota reservation on purpose: a publish
        // that can never succeed should not first charge the publisher for bytes
        // and then hand them back. The backends refuse it a second time in
        // `publish()` — that one is the invariant, this one is the clean error and
        // the one the path-addressed (deb/rpm) publishers also pass through.
        if let Some(ts) = self
            .backend
            .find_tombstone(req.registry, req.name, req.version)
            .await?
        {
            return Err(CoreError::Conflict(ts.burned_coordinate_message()));
        }

        // Snapshot hot-swappable policy (versioning, signing, size limit).
        let (versioning, signing, limit) = {
            let hot = self.hot.read().await;
            let versioning = hot.versioning.get(req.registry).cloned();
            let signing = hot.signing.get(req.registry).cloned();
            let limit = hot.max_artifact_size_bytes.unwrap_or(500 * 1024 * 1024);
            (versioning, signing, limit)
        };

        // Versioning policy check.
        //
        // RFC 0015 §4.1 moves this to the tiers: a `[[registries.namespaces]]`
        // or package-tier block replaces the registry's **wholesale**, which is
        // the only way "this one namespace follows a different release
        // convention" is expressible. `hot.versioning` stays the registry-tier
        // source of truth *and* the compiled-regex fast path — a registry whose
        // policy nothing overrides pays nothing for the tier system.
        let resolved = self
            .resolve_policy(req.registry, req.name, Some(req.version))
            .await?;
        //
        // §4.7: in dry run the policy **evaluates fully**, records what it would
        // have done, and does not do it. Evaluating and discarding rather than
        // skipping is the whole point — a dry run that did not run produces no
        // record, and the record is what an operator turns the setting on for.
        let versioning_check = match Self::tier_versioning(&resolved, versioning.as_ref())? {
            Some(policy) => validate_version(req.version, &policy),
            None => match versioning {
                Some(ref policy) => validate_version(req.version, policy),
                None => Ok(()),
            },
        };
        let versioning_check = versioning_check
            // §4.5's two constraints, which the naming checks cannot express
            // because both are about the versions that already exist. Chained so
            // one dry-run branch covers the whole `versioning` policy rather
            // than three.
            .and(self.check_immutable(req, &resolved).await)
            .and(self.check_monotonic(req, &resolved).await);

        if let Err(e) = versioning_check {
            if resolved.versioning.dry_run {
                self.record_versioning_dry_run(req, &resolved, &e);
            } else {
                return Err(e);
            }
        }

        // Signing check.
        if let Some(ref signing) = signing {
            Self::check_signing_policy(signing, req.signature_bytes, req.signature_type)?;
        }

        // Namespace enforcement.
        self.check_namespace_membership(req.registry, req.name, publisher)
            .await?;

        // Ownership check.
        let is_new_package = self
            .check_ownership_publish_access(req.registry, req.name, publisher)
            .await?;

        // `limit` was extracted from hot config above.
        if req.artifact_len > limit {
            return Err(CoreError::PayloadTooLarge(format!(
                "artifact is {} bytes; limit is {limit}",
                req.artifact_len
            )));
        }

        // Check and record quota before persisting. This may return QuotaExceeded.
        //
        // RFC 0015 §4.5: the limit comes from the deepest tier that declares one.
        // Only a tier *below* the registry is passed through — the registry's own
        // quota is already in the service's config map, and handing it back would
        // be the same value by a longer route.
        let tier_quota = Self::tier_quota(&resolved);
        let quota_check = if let Some(quota_svc) = &self.quota {
            quota_svc
                .check_and_record_publish_at_tier(
                    publisher,
                    req.registry,
                    req.artifact_len,
                    tier_quota.as_ref(),
                )
                .await?
        } else {
            QuotaCheck::default()
        };

        Ok((quota_check, is_new_package))
    }

    /// The versioning policy to enforce, when a tier deeper than the registry
    /// declared one.
    ///
    /// `None` means "nothing deeper declared anything", and the caller uses the
    /// registry's pre-compiled policy — which is both correct and the fast path,
    /// since a registry whose policy nothing overrides never compiles a regex
    /// here.
    ///
    /// Returns a compiled policy when a deeper tier *did* declare one. Composition
    /// is **wholesale** (§4.1), so this is the deeper block entire, not a merge:
    /// a namespace that omits `enforce_semver` drops it, which is the point and
    /// is what `PolicyPath::narrowing_warnings` exists to make visible.
    ///
    /// Compiling per publish is deliberate. A publish is rare next to a read,
    /// the alternative is a compiled-regex cache keyed by tier that would have
    /// to be invalidated on config reload *and* on every `policy` table write,
    /// and an uncompilable pattern is already a config-load rejection (§4.9) —
    /// so reaching the error arm here means the pattern came from the `policy`
    /// table, where §4.9's check does not run.
    fn tier_versioning(
        resolved: &crate::entities::ResolvedPolicy,
        registry_policy: Option<&crate::services::hot_config::VersioningPolicy>,
    ) -> Result<Option<crate::services::hot_config::VersioningPolicy>, CoreError> {
        // Nothing deeper declared a block: `sources.versioning` names the node
        // that supplied the answer, and a registry-tier source (or none) means
        // the compiled policy the caller already holds is the right one.
        let from = resolved.sources.versioning.as_deref().unwrap_or("");
        if !from.starts_with("namespace:")
            && !from.starts_with("package:")
            && !from.starts_with("version:")
        {
            return Ok(None);
        }
        let _ = registry_policy;

        let pattern = match &resolved.versioning.version_pattern {
            None => None,
            Some(p) => Some(regex::Regex::new(p).map_err(|e| {
                CoreError::Registry(format!(
                    "the versioning policy on `{from}` has a version_pattern that is not a \
                     valid regex: {e}"
                ))
            })?),
        };
        Ok(Some(crate::services::hot_config::VersioningPolicy {
            enforce_semver: resolved.versioning.enforce_semver,
            allow_prerelease: resolved.versioning.allow_prerelease,
            version_pattern: pattern,
        }))
    }

    /// RFC 0015 §4.5 — whether these bytes may be replaced.
    ///
    /// Only fires on a **re-publish**: a coordinate with no existing version is
    /// not being replaced, whatever the setting says.
    ///
    /// The verb is not consulted here, and that is the design rather than an
    /// omission. *"Immutability is a property of the resource, the verb is a
    /// property of the subject, and a replace needs both."* `releases:overwrite`
    /// is checked where every other verb is; this is the other half, and it is
    /// what lets a namespace be append-only **for everyone, including admins** —
    /// which no role-based model can say, and which is why `immutable` is a
    /// policy rather than a verb.
    async fn check_immutable(
        &self,
        req: &PublishPolicyRequest<'_>,
        resolved: &crate::entities::ResolvedPolicy,
    ) -> Result<(), CoreError> {
        use crate::entities::Immutable;

        let immutable = resolved.versioning.immutable;
        if immutable == Immutable::Never {
            return Ok(());
        }
        // A pre-release is replaceable under `released`, so under that setting
        // there is nothing to check for one. `is_prerelease` is the single
        // definition phase 4 converged (§4.5) — the rule this replaced called
        // `1.0-SNAPSHOT` a release and would have frozen exactly the versions
        // Maven expects to churn.
        if immutable == Immutable::Released
            && crate::services::version_order::is_prerelease(req.version)
        {
            return Ok(());
        }

        // Are *these bytes* already there? For a multi-file coordinate the
        // caller names the key, because the version row cannot tell the second
        // file of one publish from a replacement of the first.
        let exists = match req.artifact_key {
            Some(key) => self.storage.exists(key).await.unwrap_or(false),
            None => self
                .backend
                .get_versions(req.registry, req.name)
                .await?
                .iter()
                .any(|p| p.version == req.version),
        };
        if !exists {
            return Ok(());
        }

        Err(CoreError::Conflict(format!(
            "{}@{} already exists and this node is immutable = \"{}\". Publish a new version; \
             no permission grants a replacement here.",
            req.name,
            req.version,
            immutable.as_str()
        )))
    }

    /// RFC 0015 §4.5 — a new version must sort strictly above the newest
    /// existing one.
    ///
    /// Catches what `immutable` cannot: republishing an *older* number after a
    /// bad release, which leaves a resolver picking a version that was never
    /// meant to come back.
    ///
    /// Three properties §4.5 states rather than leaves to be discovered, each
    /// true here by construction:
    ///
    /// - **A yanked or deleted version still counts** as the newest. This reads
    ///   `get_versions`, which includes yanked rows, and RFC 0016's soft delete
    ///   is what keeps a deleted one visible to it. Otherwise deleting `2.0.0`
    ///   would let `1.9.9` be re-taken.
    /// - **Pre-releases fall out correctly** with no special case, because
    ///   `newest_first` is semver: `1.3.0-rc1` sorts above `1.2.0` and is
    ///   accepted; `1.2.0-rc1` after `1.2.0` sorts below and is refused.
    /// - **Bulk import is incompatible with it**, by construction — a history
    ///   publishes oldest-first. Import with `monotonic = false` and turn it on
    ///   afterwards; there is deliberately no bypass verb, for the same reason
    ///   `immutable` has none.
    async fn check_monotonic(
        &self,
        req: &PublishPolicyRequest<'_>,
        resolved: &crate::entities::ResolvedPolicy,
    ) -> Result<(), CoreError> {
        use std::cmp::Ordering;

        if !resolved.versioning.monotonic {
            return Ok(());
        }
        let existing = self.backend.get_versions(req.registry, req.name).await?;

        // A coordinate that already exists is not a *new* version, so monotonic
        // has nothing to say about it — whether those bytes may be replaced is
        // `immutable`'s question, and it has already been asked. Without this the
        // two settings collide on the one workflow that needs both: a Maven
        // coordinate is several files, so the jar of a publish whose `.pom` just
        // landed would be refused for "not sorting above" the version it is part
        // of.
        if existing.iter().any(|p| p.version == req.version) {
            return Ok(());
        }
        // The newest by this server's one ordering function, not by publish
        // date: "newest" here is a statement about the version *number*, which
        // is the thing being constrained.
        let Some(newest) = existing
            .iter()
            .map(|p| p.version.as_str())
            .min_by(|a, b| crate::services::version_order::newest_first(a, b))
        else {
            return Ok(());
        };

        if crate::services::version_order::newest_first(req.version, newest) == Ordering::Less {
            return Ok(());
        }
        Err(CoreError::Conflict(format!(
            "monotonic versioning: '{}' does not sort above '{newest}', the newest version of \
             {} in this registry. A yanked or deleted version still counts — that is what stops \
             a coordinate from being re-taken. Bulk-import a history with monotonic = false and \
             enable it afterwards.",
            req.version, req.name
        )))
    }

    /// §4.7's record of a versioning refusal that did not happen.
    ///
    /// Two of the three records §4.7 asks for — the structured line and the
    /// counter. The third, the admin endpoint's buffer, is the grants shadow's:
    /// a versioning dry run refuses nothing on the *read* path, so it does not
    /// belong on a page whose subject is what authorization did. It belongs
    /// where the publish that triggered it is visible, and the log line and the
    /// counter are what an import is watched with.
    fn record_versioning_dry_run(
        &self,
        req: &PublishPolicyRequest<'_>,
        resolved: &crate::entities::ResolvedPolicy,
        error: &CoreError,
    ) {
        let node = resolved
            .sources
            .versioning
            .as_deref()
            .unwrap_or("registry")
            .to_owned();
        tracing::warn!(
            policy = "versioning",
            node = %node,
            registry = %req.registry,
            package = %req.name,
            version = %req.version,
            reason = %error,
            "dry run accepted a publish the versioning policy would have refused"
        );
        metrics::counter!(
            "batlehub_policy_dryrun_total",
            "policy" => "versioning",
            "node" => node,
        )
        .increment(1);
    }

    /// The quota a tier below the registry declared, if any (§4.5).
    ///
    /// `None` when the answer came from the registry tier or from nowhere, so
    /// the quota service uses the config map it already holds. Quota stops at
    /// the package tier — a per-version quota would limit a thing published
    /// exactly once — and the port refuses to store one, so there is no version
    /// tier to consider here.
    fn tier_quota(
        resolved: &crate::entities::ResolvedPolicy,
    ) -> Option<crate::services::quota::RegistryQuotaConfig> {
        use crate::services::quota::{QuotaEnforcement, RegistryQuotaConfig};

        let from = resolved.sources.quota.as_deref()?;
        if !from.starts_with("namespace:") && !from.starts_with("package:") {
            return None;
        }
        let q = resolved.quota.as_ref()?;
        Some(RegistryQuotaConfig {
            max_storage_bytes_per_user: q.max_bytes_per_user,
            max_packages_per_user: q.max_packages_per_user,
            warn_threshold: f64::from(q.warn_threshold_pct.unwrap_or(80)) / 100.0,
            enforcement: if q.block {
                QuotaEnforcement::Block
            } else {
                QuotaEnforcement::Warn
            },
        })
    }

    /// Validate and persist a published artifact.
    ///
    /// Returns a `QuotaCheck` describing the publisher's current quota state
    /// after the publish (useful for setting `X-Quota-*` response headers).
    /// Returns a zeroed `QuotaCheck` when no quota is configured.
    pub async fn publish(&self, req: PublishRequest) -> Result<QuotaCheck, CoreError> {
        let (quota_check, is_new_package) = self
            .enforce_publish_policy(
                &PublishPolicyRequest {
                    registry: &req.registry,
                    name: &req.name,
                    version: &req.version,
                    artifact_len: req.artifact.len() as u64,
                    signature_bytes: req.signature_bytes.as_deref(),
                    signature_type: req.signature_type.as_deref(),
                    artifact_key: None,
                },
                &req.publisher,
            )
            .await?;

        // Inherit the existing package visibility so that publishing a new version
        // doesn't silently reset a team/internal package back to public.
        // `get_visibility` returns Public when no published rows exist yet (first publish).
        // Propagate DB errors rather than defaulting to Public — silently publishing a
        // team-private package as world-readable during a DB outage is a security failure.
        let visibility = if let Some(ref ns_port) = self.team_namespace {
            // Quota was already reserved-and-recorded in `enforce_publish_policy`.
            // A failure here (before `execute_publish_transaction`, the only place
            // that revokes on rollback) would otherwise charge the publisher for
            // bytes that are never stored, so revoke the reservation before
            // propagating the error.
            match ns_port.get_visibility(&req.registry, &req.name).await {
                Ok(v) => v,
                Err(e) => {
                    self.revoke_quota(&req.publisher, &req.registry, req.artifact.len() as u64)
                        .await;
                    return Err(e);
                }
            }
        } else {
            Visibility::default()
        };

        // RFC 0015 §4.5: a namespace's `visibility` is *"the default applied to a
        // version published into the namespace, replacing 'public unless someone
        // sets it'"*, and `prerelease_visibility` is the same default for a
        // pre-release — which is what `[registries.beta_channel]` becomes (§10
        // rule 6).
        //
        // Applied only where the port answered `Public`, which is what it
        // returns both for a genuinely public package and for a first publish
        // with no rows yet. Those are the two cases where "nobody has decided"
        // is true, and they are exactly the cases a tier default is for — an
        // explicit per-package `team` must not be widened *or* re-narrowed by a
        // namespace it happens to sit under, because a per-package override
        // remains (RFC 0011-bis §4.3) and deepest wins.
        let visibility = if visibility == Visibility::Public {
            let resolved = self
                .resolve_policy(&req.registry, &req.name, Some(&req.version))
                .await
                .unwrap_or_default();
            if crate::services::version_order::is_prerelease(&req.version) {
                resolved.prerelease_visibility
            } else {
                resolved.visibility
            }
        } else {
            visibility
        };

        let pkg = PublishedPackage {
            registry: req.registry.clone(),
            name: req.name.clone(),
            version: req.version.clone(),
            checksum: req.checksum.clone(),
            yanked: false,
            deprecated: false,
            deprecation_message: None,
            unlisted: req.unlisted,
            index_metadata: req.index_metadata.clone(),
            published_at: chrono::Utc::now(),
            published_by: req.publisher.user_id.clone(),
            // Normalised the same way `check_signing_policy` judged them: an
            // empty signature is no signature, and persisting `Some(vec![])`
            // would make this row read back as "published signed" for
            // `require_signed_release` while carrying nothing to verify.
            signature_bytes: req.signature_bytes.clone().filter(|b| !b.is_empty()),
            signature_type: req.signature_type.clone().filter(|t| !t.trim().is_empty()),
            visibility,
            // Never pinned on publish. A retention pin is an operator's later
            // decision about a specific release; a publisher who could set it
            // would make every version exempt from the policy above them.
            retention_keep: false,
        };

        let storage_key = artifact_storage_key(&req.registry, &req.name, &req.version);
        let bytes = req.artifact.len() as u64;

        // Steps 1-3: reserve → store → commit, with rollback on each failure.
        self.execute_publish_transaction(pkg, &req, &storage_key, bytes)
            .await?;

        // RFC 0018 §4.2 *Local publish* (phase 2): on a `[security]` registry
        // the version is `quarantined(SCAN_PENDING)` — invisible in listings
        // — from the moment it is stored, and a `FirstSeen` job is queued, so
        // the seconds between publish and first pull are not a window in
        // which an unscanned version is listed.
        self.first_sight_after_publish(&req).await;

        // Invalidate explore cache so the new version appears without waiting for TTL expiry.
        if let Some(ref cache) = self.explore_cache {
            cache.invalidate(Some(&req.registry)).await;
        }

        // Step 4: generate SBOM. When `required` is true and generation fails,
        // roll back the publish (version row + bytes + quota) and return the
        // error. This runs *before* owner registration so a rejected publish
        // never leaves a dangling owner claim on a name with no versions.
        self.run_publish_sbom(&req, &storage_key, bytes).await?;

        // Step 5: on first publish, register the publisher as the package admin.
        // Last, and only once the publish is fully committed, so it is never
        // orphaned by a later rollback.
        self.register_initial_owner(is_new_package, &req.registry, &req.name, &req.publisher)
            .await;

        Ok(quota_check)
    }

    /// Record the just-published version's first sight on a `[security]`
    /// registry. A store error is logged, not returned: the publish is
    /// committed, and the version's first *read* creates the same hold.
    async fn first_sight_after_publish(&self, req: &PublishRequest) {
        let (policy, verdicts, queue) = {
            let hot = self.hot.read().await;
            let Some(policy) = hot.security.get(&req.registry).cloned() else {
                return;
            };
            (policy, hot.verdicts.clone(), hot.scan_queue.clone())
        };
        let (Some(verdicts), Some(queue)) = (verdicts, queue) else {
            return;
        };
        let service = crate::services::VerdictService::new(verdicts, queue);
        let package = PackageId::new(&req.registry, &req.name, &req.version);
        if let Err(e) = service
            .first_sight_local(&package, &policy, chrono::Utc::now())
            .await
        {
            tracing::warn!(
                package = %package,
                error = %e,
                "security: could not record the published version's first sight"
            );
        }
    }

    /// Steps 1-3 of publish: reserve pending row → store artifact bytes → commit.
    /// Rolls back cleanly on each failure so the caller gets a pristine error.
    async fn execute_publish_transaction(
        &self,
        pkg: PublishedPackage,
        req: &PublishRequest,
        storage_key: &str,
        bytes: u64,
    ) -> Result<(), CoreError> {
        let publisher = &req.publisher;
        let registry = req.registry.as_str();
        let name = req.name.as_str();
        let version = req.version.as_str();

        // Step 1: reserve the version (inserted as 'pending', invisible to readers).
        if let Err(e) = self.backend.publish(pkg).await {
            self.revoke_quota(publisher, registry, bytes).await;
            return Err(e);
        }

        // Step 2: persist artifact bytes. On failure, discard the pending row.
        if let Err(e) = self
            .storage
            .store(
                storage_key,
                req.artifact.clone(),
                StorageMeta {
                    content_type: Some("application/octet-stream".into()),
                    size: None,
                    checksum: Some(req.checksum.clone()),
                },
            )
            .await
        {
            self.remove_pending(registry, name, version).await;
            self.revoke_quota(publisher, registry, bytes).await;
            return Err(e);
        }

        // Step 3: promote the pending row to 'published'. On failure, undo both
        // the storage write and the pending row so the caller gets a clean error.
        self.invalidate_documents(registry).await;
        if let Err(e) = self.backend.commit_publish(registry, name, version).await {
            self.remove_pending(registry, name, version).await;
            if let Err(err) = self.storage.delete(storage_key).await {
                tracing::error!("storage cleanup after commit failure: {err}");
            }
            self.revoke_quota(publisher, registry, bytes).await;
            return Err(e);
        }

        Ok(())
    }

    /// Step 4 of publish: register the publisher as package admin on first publish (non-fatal).
    ///
    /// The package-tier grant §10 rule 9 asks for is written by
    /// [`OwnershipGrants`](crate::services::ownership_grants::OwnershipGrants),
    /// which wraps the port — not here.
    ///
    /// It used to be written here, inline, and that was the whole of the
    /// projection: the four *other* doors ownership changes through — the two
    /// admin routes, the two `cargo owner` routes — wrote `package_owners` and
    /// nothing else, so the two stores diverged from the first owner change on
    /// any estate. Adding a fifth copy of the write here would have been the
    /// convention this document exists to replace; wrapping the port instead
    /// means every door gets it because there is no other port to call.
    async fn register_initial_owner(
        &self,
        is_new_package: bool,
        registry: &str,
        name: &str,
        publisher: &Identity,
    ) {
        if !is_new_package {
            return;
        }
        let Some(ref uid) = publisher.user_id else {
            return;
        };

        if let Some(ref ownership) = self.ownership {
            if let Err(err) = ownership.initialize_owner(registry, name, uid).await {
                tracing::warn!("initialize_owner failed (non-fatal): {err}");
            }
        }
    }
}
