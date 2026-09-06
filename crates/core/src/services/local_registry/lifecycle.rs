//! Lifecycle mutations of an already-published version.
//!
//! # Which verb each of these is (RFC 0015 §4.2)
//!
//! The vocabulary has **one** verb for this whole family — `releases:yank`,
//! documented as *"yank and unyank"* — and §4.2 rule 3 is explicit that a new
//! action spelled differently is the shared verb rather than a new one: *"A new
//! ecosystem's 'hide this version from resolution' is `releases:yank`, not
//! `myeco:unlist:write`. The test is whether an operator reading a grant on a
//! mixed estate would expect them to mean the same thing."*
//!
//! Unlist, relist, deprecate and undeprecate all pass that test. Each is a
//! reversible mark on a version that already exists, none adds or destroys bytes,
//! and an operator granting `releases:yank` to a release engineer plainly means
//! them to be able to hide a bad build whichever of the four spellings their
//! ecosystem uses. §10 rule 5 enumerates "yank, unyank, unlist and delete" as
//! today's role-checked sites for the same reason.
//!
//! `delete_version` is the exception and takes `releases:delete`, because it is
//! the one that destroys bytes.
//!
//! `set_retention_pin` and `compact_tombstone_detail` deliberately take neither.
//! They are RFC 0016's surface, retention is a **policy** in §4.1's tier table
//! rather than a verb in §4.2's vocabulary, and inventing a verb for it here
//! would settle by implementation a question §3 hands to that document.

use super::{
    artifact_storage_key, AccessAction, AccessEvent, AccessResult, Action, CoreError, Identity,
    LocalRegistryService, PackageId, PublishRequest, ReadmeFormat, SbomFormat, SbomPublishOptions,
};

impl LocalRegistryService {
    pub async fn yank(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        self.authorize_write(registry, name, version, identity, Action::ReleasesYank)
            .await?;
        self.check_namespace_membership(registry, name, identity)
            .await?;
        self.check_ownership_lifecycle_access(registry, name, identity)
            .await?;
        self.backend.yank(registry, name, version).await?;
        self.invalidate_documents(registry).await;
        self.record_lifecycle_action(registry, name, version, AccessAction::Yank, identity)
            .await;
        Ok(())
    }

    pub async fn unyank(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        self.authorize_write(registry, name, version, identity, Action::ReleasesYank)
            .await?;
        self.check_namespace_membership(registry, name, identity)
            .await?;
        self.check_ownership_lifecycle_access(registry, name, identity)
            .await?;
        self.backend.unyank(registry, name, version).await?;
        self.invalidate_documents(registry).await;
        self.record_lifecycle_action(registry, name, version, AccessAction::Unyank, identity)
            .await;
        Ok(())
    }

    pub async fn deprecate(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        message: Option<&str>,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        self.authorize_write(registry, name, version, identity, Action::ReleasesYank)
            .await?;
        self.check_namespace_membership(registry, name, identity)
            .await?;
        self.check_ownership_lifecycle_access(registry, name, identity)
            .await?;
        self.backend
            .deprecate(registry, name, version, message)
            .await?;
        self.record_lifecycle_action(registry, name, version, AccessAction::Deprecate, identity)
            .await;
        Ok(())
    }

    pub async fn undeprecate(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        self.authorize_write(registry, name, version, identity, Action::ReleasesYank)
            .await?;
        self.check_namespace_membership(registry, name, identity)
            .await?;
        self.check_ownership_lifecycle_access(registry, name, identity)
            .await?;
        self.backend.undeprecate(registry, name, version).await?;
        self.record_lifecycle_action(registry, name, version, AccessAction::Undeprecate, identity)
            .await;
        Ok(())
    }

    pub async fn unlist(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        self.authorize_write(registry, name, version, identity, Action::ReleasesYank)
            .await?;
        self.check_namespace_membership(registry, name, identity)
            .await?;
        self.check_ownership_lifecycle_access(registry, name, identity)
            .await?;
        self.backend.unlist(registry, name, version).await?;
        self.record_lifecycle_action(registry, name, version, AccessAction::Unlist, identity)
            .await;
        Ok(())
    }

    pub async fn relist(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        self.authorize_write(registry, name, version, identity, Action::ReleasesYank)
            .await?;
        self.check_namespace_membership(registry, name, identity)
            .await?;
        self.check_ownership_lifecycle_access(registry, name, identity)
            .await?;
        self.backend.relist(registry, name, version).await?;
        self.record_lifecycle_action(registry, name, version, AccessAction::Relist, identity)
            .await;
        Ok(())
    }

    /// Delete a published version: drop the bytes, keep the coordinate.
    ///
    /// This is the whole of RFC 0016 §4.4 in one place. The version row is
    /// tombstoned rather than removed, so `1.4.0` can never mean two different
    /// things to two different lockfiles — and the artifact, its README, and the
    /// explore cache entry all go, because what the caller asked for is that the
    /// bytes stop being served.
    ///
    /// Returns `true` when a published version was tombstoned, `false` when there
    /// was nothing to delete. `false` is not an error: the coordinate is gone
    /// either way, which is what the caller asked for, and a bulk delete over a
    /// list that has already been partly processed should not fail on the
    /// second pass.
    ///
    /// **Order matters.** The row is tombstoned *first*. Between that and the
    /// storage delete the version is already invisible to every listing and
    /// already refuses a re-publish, so a crash in the middle leaves an orphaned
    /// blob — recoverable, and the coherence sweep collects it. The other order
    /// would leave a *live* version pointing at bytes that are gone, which every
    /// client resolves and then fails to download.
    pub async fn delete_version(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        identity: &Identity,
    ) -> Result<bool, CoreError> {
        self.delete_version_audited(registry, name, version, identity, AccessAction::Delete)
            .await
    }

    /// [`Self::delete_version`], recorded in the trail as `audit_as`.
    ///
    /// The one caller that passes anything but [`AccessAction::Delete`] is
    /// [`RetentionService`](crate::services::RetentionService), which passes
    /// [`AccessAction::RetentionReclaim`]. Everything else about the deletion is
    /// identical **by construction, not by resemblance** — same authorization,
    /// same tombstone, same bytes dropped — because retention reclaiming a
    /// version through a private path of its own is exactly how the tombstone
    /// invariant would come to have two implementations and one of them a bug
    /// (RFC 0016 §4.2).
    ///
    /// Only the audit action differs, and it has to: a run is triggered by an
    /// operator's own token, so without this the event a policy reclamation
    /// leaves is byte-for-byte what that operator deleting the version by hand
    /// leaves, and RFC 0016 §3's "an operator reading the audit trail must be
    /// able to tell a policy reclamation from a human deletion" is unmet.
    pub async fn delete_version_audited(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        identity: &Identity,
        audit_as: AccessAction,
    ) -> Result<bool, CoreError> {
        // `releases:delete` is the whole of the authorization decision. RFC 0016
        // §3 hands the verb to RFC 0015 and this is where it is consumed; the
        // handler in front of this is still `require_admin`, so the grant narrows
        // an already-narrow surface rather than opening one.
        self.authorize_write(registry, name, version, identity, Action::ReleasesDelete)
            .await?;
        self.check_namespace_membership(registry, name, identity)
            .await?;
        // Admins bypass the ownership check, as they already do the namespace
        // one directly above. Not a widening: this is the only delete path, the
        // handler in front of it is `require_admin`, and `can_publish` answers
        // "is this principal an owner" with no role bypass of its own — so
        // running it here would have made an administrator unable to delete any
        // package that has an owner, which is every package that was ever
        // published. Who may delete is `releases:delete` and RFC 0015 owns it
        // (RFC 0016 §3); this must not tighten it as a side effect.
        if !identity.is_admin() {
            self.check_ownership_lifecycle_access(registry, name, identity)
                .await?;
        }

        let tombstoned = self
            .backend
            .tombstone_version(registry, name, version, identity.user_id.as_deref())
            .await?;
        if !tombstoned {
            return Ok(false);
        }

        self.drop_version_bytes(registry, name, version).await;
        self.delete_readme_for_version(registry, name, version)
            .await;
        self.release_name_if_last_version(registry, name).await;
        if let Some(ref cache) = self.explore_cache {
            cache.invalidate(Some(registry)).await;
        }
        self.record_lifecycle_action(registry, name, version, audit_as, identity)
            .await;
        Ok(true)
    }

    /// The claim covering `namespace`, if any (RFC 0015 §4.2).
    ///
    /// A read, and unguarded on purpose: whether a namespace is claimed is what
    /// the OpenVSX protocol's `verified` field *is*, served to every client that
    /// asks about the namespace. There is nothing here a caller could not infer
    /// from that response.
    pub async fn namespace_claim(
        &self,
        registry: &str,
        namespace: &str,
    ) -> Result<Option<crate::entities::TeamNamespace>, CoreError> {
        let Some(ref ns_port) = self.team_namespace else {
            return Ok(None);
        };
        // `find_namespace`, so a claim on a *parent* namespace answers for its
        // children by the ecosystem's own separator — the whole point of §4.1's
        // table, and the reason this could not be built before migration 045.
        ns_port.find_namespace(registry, namespace).await
    }

    /// Claim an OpenVSX publisher namespace (RFC 0015 §4.2).
    ///
    /// `openvsx:namespace:claim` — the last of §4.2's four ecosystem verbs, and
    /// the one that was **blocked rather than declined**. `team_namespaces` was
    /// always the right store; what stopped it was that every matcher hardcoded
    /// `/` while OpenVSX namespaces are dotted, so a claim on `digital` covered
    /// `digital` and none of its extensions. Migration 045 put the separator on
    /// the claim, which is what makes this a lookup rather than a special case.
    ///
    /// # Administrative, not self-service
    ///
    /// The OpenVSX protocol assumes first-come self-service: any authenticated
    /// caller claims an unclaimed namespace. This does not, and §4.3's delegation
    /// bounds are why — a claim decides who may *see* every package beneath it
    /// once they are `team`-visible, so a self-service claim is a caller granting
    /// themselves authority over a subtree. That is the escalation the whole
    /// section is built to exclude, and the fact that a protocol expects it is not
    /// an argument for allowing it.
    ///
    /// An estate that wants self-service grants `openvsx:namespace:claim` to
    /// `role:user`, which is exactly the shape §4.5 gives `gates:exempt`: the
    /// permissive policy is expressible, and it is a decision somebody makes
    /// rather than the default.
    pub async fn claim_openvsx_namespace(
        &self,
        registry: &str,
        namespace: &str,
        group_id: &str,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        // The coordinate is the namespace itself, which is a package-shaped name
        // for resolution: a grant written on `[[registries.namespaces]] match =
        // "digital"` reaches a claim on `digital` and on nothing beside it.
        self.authorize_write(
            registry,
            namespace,
            "",
            identity,
            Action::OpenvsxNamespaceClaim,
        )
        .await?;

        let Some(ref ns_port) = self.team_namespace else {
            return Err(CoreError::Registry(
                "this deployment has no team-namespace store configured".to_owned(),
            ));
        };

        let separator = {
            let hot = self.hot.read().await;
            hot.grants
                .get(registry)
                .map(|g| crate::entities::namespace_separator(g.kind))
                .unwrap_or('/')
        };

        ns_port
            .claim_namespace(crate::entities::TeamNamespace {
                registry: registry.to_owned(),
                prefix: namespace.to_owned(),
                // Spaces stripped, as `check_team_visibility` compares them —
                // `claim_team_namespace` does the same, and a claim that does not
                // match the comparison is a claim nobody satisfies.
                group_id: group_id.replace(' ', ""),
                claimed_by: identity.user_id.clone(),
                separator,
            })
            .await?;

        self.record_registry_action(registry, AccessAction::ClaimNamespace, identity)
            .await;
        Ok(())
    }

    /// Move a published plugin build to a release channel (RFC 0015 §4.2).
    ///
    /// `jetbrains:channel:assign` — the one verb in §4.2's ecosystem table whose
    /// action this server can perform without new protocol work, because the read
    /// path already selects on `channel` (`eco_jetbrains.rs`) and only the write
    /// was missing.
    ///
    /// # Not a replacement, so `immutable` does not apply
    ///
    /// §4.5 asks whether repointing a published version counts as replacing it,
    /// and §13.6 answered the general form: *"immutability is a question about
    /// **bytes**, not about a coordinate."* A channel move changes no byte — the
    /// artifact, its checksum and its signature are untouched, and a client that
    /// already resolved the build is unaffected. What changes is which feed
    /// *offers* it, which is the same class of statement as a yank.
    ///
    /// That is also why it sits here beside yank rather than on the publish path:
    /// the hide-family verbs are the ones that mark an existing version without
    /// adding or destroying bytes, and this is one of them in everything but
    /// spelling. It gets its own verb only because §4.2 rule 3's test — *"would an
    /// operator reading a grant on a mixed estate expect them to mean the same
    /// thing"* — fails for it: a channel is a JetBrains concept, and `releases:yank`
    /// on an npm registry would not lead anyone to expect it.
    pub async fn assign_channel(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        channel: &str,
        identity: &Identity,
    ) -> Result<bool, CoreError> {
        self.authorize_write(
            registry,
            name,
            version,
            identity,
            Action::JetbrainsChannelAssign,
        )
        .await?;
        self.check_namespace_membership(registry, name, identity)
            .await?;
        self.check_ownership_lifecycle_access(registry, name, identity)
            .await?;

        let changed = self
            .backend
            .set_channel(registry, name, version, channel)
            .await?;
        if changed {
            // Every whole-registry document that selects on channel is now stale
            // — the JetBrains plugin list is one of the six §4.4 filters.
            self.invalidate_documents(registry).await;
            self.record_lifecycle_action(registry, name, version, AccessAction::Relist, identity)
                .await;
        }
        Ok(changed)
    }

    /// Pin a version against retention, or release the pin (RFC 0016 §4.1).
    ///
    /// The same gate the other lifecycle mutations use, and audited the same
    /// way, because it is one: a pin is an operator's statement about a specific
    /// release, and "who exempted this version from the policy" is exactly the
    /// question a later reader asks.
    ///
    /// Recorded as `Unlist`/`Relist`'s sibling rather than as a new action —
    /// `AccessAction::SetRetentionPin` carries whether it was set or released in
    /// the same event, because two actions for one toggle would make the trail
    /// harder to read, not easier.
    pub async fn set_retention_pin(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        keep: bool,
        identity: &Identity,
    ) -> Result<bool, CoreError> {
        // No role floor. `retention:run` is checked by the handler through the
        // engine, and a role assertion here would be a *second* decision that
        // silently overrides the first — §13.8's finding, and the reason §6.1
        // deleted the nine floors on the publish path rather than keeping them
        // as belt and braces. The ownership and namespace narrowing below stays:
        // ownership narrows, and narrowing composes with a grant (§5.1).
        self.check_namespace_membership(registry, name, identity)
            .await?;
        if !identity.is_admin() {
            self.check_ownership_lifecycle_access(registry, name, identity)
                .await?;
        }
        let changed = self
            .backend
            .set_retention_keep(registry, name, version, keep)
            .await?;
        if changed {
            self.record_lifecycle_action(
                registry,
                name,
                version,
                AccessAction::SetRetentionPin,
                identity,
            )
            .await;
        }
        Ok(changed)
    }

    /// Strip aged-out tombstone detail, keeping every coordinate claim
    /// (RFC 0016 §4.5).
    ///
    /// Audited as [`AccessAction::TombstoneCompact`] rather than as a delete: it
    /// is destructive to *history* and harmless to the invariant, which is a
    /// different fact about the system and one an operator reading the trail has
    /// to be able to separate from a version being deleted. Same reasoning
    /// `AuditPurge` already follows for the audit trail's own purge.
    ///
    /// A dry run is not audited. Nothing happened, and an audit trail that
    /// records reads is a trail nobody finishes reading.
    pub async fn compact_tombstone_detail(
        &self,
        registry: &str,
        older_than: std::time::Duration,
        dry_run: bool,
        identity: &Identity,
    ) -> Result<crate::entities::CompactionReport, CoreError> {
        // No role floor, for the reason above — and here it was not merely
        // redundant but *live*: `tombstones_read_granted_to_a_user_is_honoured`
        // fails against the assertion, so an operator writing
        // `[registries.grants]` to delegate `tombstones:read` was refused by
        // this line and nothing told them. §13.12 claims the `require_admin`
        // split made each verb delegable; for this one it did not.
        let report = self
            .backend
            .compact_tombstone_detail(registry, older_than, dry_run)
            .await?;
        if !dry_run && report.compacted > 0 {
            self.record_registry_action(registry, AccessAction::TombstoneCompact, identity)
                .await;
        }
        Ok(report)
    }

    /// Audit an action that is about a whole registry rather than one version.
    ///
    /// The `AccessEvent` shape wants a `PackageId`, so compaction — which
    /// touches many coordinates at once and is reported by count — records the
    /// registry with an empty name and version. That is what `AuditPurge`
    /// already does for an action with no coordinate at all, and it keeps the
    /// event joinable to a registry without inventing a package that was not
    /// involved.
    pub async fn record_registry_action(
        &self,
        registry: &str,
        action: AccessAction,
        identity: &Identity,
    ) {
        self.record_lifecycle_action(registry, "", "", action, identity)
            .await;
    }

    /// When a package's last live version is deleted, drop its ownership grants
    /// (RFC 0016 §4.4).
    ///
    /// A package name is a weaker claim than a version coordinate: the numbers
    /// stay spent forever, but the *name* is released and someone the grants
    /// permit may create it again. Ownership is keyed by `(registry, name)` and
    /// nothing else removes it, so leaving it behind means the previous owner
    /// holds `releases:publish` and owner-management authority over a package
    /// they have never seen — a smaller version of the 2026-08-26 survey's
    /// finding 1 arriving through the back door.
    ///
    /// It has a second effect worth naming, because it is the one an operator
    /// notices: a stale grant does not merely linger, it **blocks**. The
    /// newcomer taking the released name is refused by an owner row belonging to
    /// a package that no longer exists.
    ///
    /// Non-fatal. The version is already tombstoned and the name already
    /// released by the time this runs; failing the delete because the grant
    /// cleanup did not land would report a deletion that in fact happened.
    async fn release_name_if_last_version(&self, registry: &str, name: &str) {
        let Some(ref ownership) = self.ownership else {
            return;
        };
        // `exists` counts published rows only — a tombstone is not one — so this
        // is false exactly when the package has no live version left.
        match self.backend.exists(registry, name).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    registry, name, error = %e,
                    "delete: could not tell whether this was the last version, so the \
                     package's ownership grants were left in place"
                );
                return;
            }
        }
        if let Err(e) = ownership.remove_all_owners(registry, name).await {
            tracing::warn!(
                registry, name, error = %e,
                "delete: releasing the package name's ownership grants failed; the \
                 previous owner still holds authority over a name that is now free"
            );
        }
    }

    /// Drop every stored byte belonging to one version.
    ///
    /// Two calls rather than one because a version owns two shapes of key. The
    /// single-file ecosystems store the artifact *at* `local:{reg}/{name}/{ver}`;
    /// Maven and the Terraform providers store a directory of files *under* it
    /// (`…/{ver}/{filename}`, `…/{ver}/{os}-{arch}`). A prefix delete on the bare
    /// key would take neither reliably and both too much: `local:r/p/1.0` is a
    /// prefix of `local:r/p/1.0.1`, so it would delete a sibling version. The
    /// trailing slash on the prefix is what stops that.
    ///
    /// Non-fatal by construction. The tombstone is already written by the time
    /// this runs, so the version is unreachable regardless; a storage error here
    /// leaves an orphaned blob for the coherence sweep, and turning that into a
    /// failed delete would tell the caller the version is still live when it is
    /// not.
    async fn drop_version_bytes(&self, registry: &str, name: &str, version: &str) {
        let key = artifact_storage_key(registry, name, version);
        if let Err(e) = self.storage.delete(&key).await {
            tracing::warn!(
                registry, name, version, error = %e,
                "delete: dropping the artifact bytes failed; the version is tombstoned \
                 and unreachable, and the blob is left for the coherence sweep"
            );
        }
        // RFC 0020: the signature archives beside the artifact — the
        // registry's and a provided one — go with it.
        for sibling in [format!("{key}.sigzip"), format!("{key}.sigzip.upstream")] {
            if let Err(e) = self.storage.delete(&sibling).await {
                tracing::warn!(
                    registry, name, version, error = %e,
                    "delete: dropping a signature sibling failed (non-fatal, see above)"
                );
            }
        }
        if let Err(e) = self.storage.delete_by_prefix(&format!("{key}/")).await {
            tracing::warn!(
                registry, name, version, error = %e,
                "delete: dropping the multi-file artifacts under the version failed \
                 (non-fatal, see above)"
            );
        }
    }

    /// Record a successful lifecycle admin action (yank/unyank/deprecate/
    /// undeprecate/unlist/relist/delete) through `package_repo`, when configured. Mirrors
    /// `read.rs`'s `record_download` so these mutations aren't a silent audit
    /// gap next to the package-block/visibility/ownership admin actions that
    /// already go through `AdminService::record_admin_action`.
    pub async fn record_lifecycle_action(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        action: AccessAction,
        identity: &Identity,
    ) {
        let Some(repo) = self.package_repo.as_ref() else {
            return;
        };
        let event = AccessEvent {
            id: uuid::Uuid::new_v4(),
            user_id: identity.user_id.clone(),
            user_role: identity.role.clone(),
            package_id: Some(PackageId::new(registry, name, version)),
            action,
            result: AccessResult::Allowed,
            timestamp: chrono::Utc::now(),
            ip_address: None,
            user_agent: None,
        };
        if let Err(e) = repo.record_access(event).await {
            tracing::warn!(error = %e, "audit log write failed for local registry lifecycle action");
        }
    }

    /// If a namespace claim covers `package` in `registry`, verify `identity` is
    /// a member of the owning group. Admins and unclaimed packages bypass this.
    pub async fn check_namespace_membership(
        &self,
        registry: &str,
        package: &str,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        if identity.is_admin() {
            return Ok(());
        }
        let Some(ref ns_port) = self.team_namespace else {
            return Ok(());
        };
        if let Some(ns) = ns_port.find_namespace(registry, package).await? {
            let norm_id = ns.group_id.replace(' ', "");
            let ok = identity
                .groups
                .iter()
                .any(|g| g.replace(' ', "") == norm_id);
            if !ok {
                return Err(CoreError::AccessDenied(format!(
                    "namespace '{}' in registry '{}' is owned by group '{}'; \
                     you are not a member",
                    ns.prefix, registry, ns.group_id
                )));
            }
        }
        Ok(())
    }

    pub(super) async fn remove_pending(&self, registry: &str, name: &str, version: &str) {
        if let Err(err) = self.backend.remove_version(registry, name, version).await {
            tracing::error!("pending row cleanup failed: {err}");
        }
    }

    pub(super) async fn revoke_quota(&self, identity: &Identity, registry: &str, bytes: u64) {
        if let Some(svc) = &self.quota {
            if let Err(err) = svc.revoke_publish(identity, registry, bytes).await {
                tracing::error!("quota revoke failed: {err}");
            }
        }
    }

    /// Public revoke for path-addressed (deb/rpm) publish handlers, which record
    /// quota via [`Self::enforce_publish_policy`] and then perform their own
    /// storage writes outside the [`Self::publish`] transaction. They call this to
    /// undo the recorded quota when a write fails, so a transient storage error
    /// doesn't permanently charge the publisher for an artifact that never landed.
    pub async fn revoke_publish_quota(&self, identity: &Identity, registry: &str, bytes: u64) {
        self.revoke_quota(identity, registry, bytes).await;
    }

    /// Record a README a publish request carried in its own metadata.
    ///
    /// Called by the publish handlers that have one — npm's publish document
    /// carries `readme`, cargo's metadata carries `readme` and `readme_file` —
    /// because the field is protocol-specific and only the handler knows where
    /// to look for it. This is the shared half: the config lookup, the
    /// non-fatal contract, and the log line.
    ///
    /// Non-fatal by construction, unlike `sbom.required`: a publish that
    /// succeeded must not be reported as failed because prose could not be
    /// stored, and there is no `readme.required` for it to mean anything else.
    pub async fn record_publish_readme(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        content: String,
        format: ReadmeFormat,
    ) {
        let Some(ref readme_svc) = self.readme else {
            return;
        };
        // Absent means enabled: the builder writes an entry for every
        // configured registry, so a missing key is a hand-built test.
        let cfg = {
            let hot = self.hot.read().await;
            hot.readme.get(registry).cloned().unwrap_or_default()
        };
        if let Err(e) = readme_svc
            .record_from_publish(registry, name, version, content, format, &cfg)
            .await
        {
            tracing::warn!(
                registry, name, version, error = %e,
                "readme: recording a published README failed (non-fatal)"
            );
        }
    }

    /// Remove the stored README for a version that has just been deleted.
    ///
    /// Explicit rather than a database cascade, because `package_readmes` has no
    /// foreign key: a README outlives the bytes — the catalogue already
    /// describes versions it holds none of, and a panel that emptied itself when
    /// LRU eviction ran would be inexplicable — so a cascade from anything
    /// evictable would delete exactly the rows §5.4 says to keep. This is the
    /// other half of that decision: what nothing cascades from, something has to
    /// call.
    ///
    /// Deliberately **not** called from the admin package-delete path, which
    /// purges a *cached artifact* and its tracking row. That is eviction shaped,
    /// and the README survives it.
    pub async fn delete_readme_for_version(&self, registry: &str, name: &str, version: &str) {
        let Some(ref readme_svc) = self.readme else {
            return;
        };
        if let Err(e) = readme_svc
            .repo
            .delete_for_version(registry, name, version)
            .await
        {
            tracing::warn!(
                registry, name, version, error = %e,
                "readme: deleting a removed version's README failed (non-fatal)"
            );
        }
    }

    pub(super) async fn run_publish_sbom(
        &self,
        req: &PublishRequest,
        storage_key: &str,
        bytes: u64,
    ) -> Result<(), CoreError> {
        let Some(ref sbom_svc) = self.sbom else {
            return Ok(());
        };
        let sbom_cfg = {
            let hot = self.hot.read().await;
            hot.sbom.get(&req.registry).cloned()
        };
        let Some(cfg) = sbom_cfg.filter(|c| c.enabled) else {
            return Ok(());
        };
        let formats: Vec<SbomFormat> = cfg
            .formats
            .iter()
            .filter_map(|s| SbomFormat::parse(s))
            .collect();
        let result = sbom_svc
            .record_for_published(
                &req.registry,
                &req.name,
                &req.version,
                storage_key,
                &req.artifact,
                SbomPublishOptions {
                    registry_type: &cfg.registry_type,
                    formats: &formats,
                    required: cfg.required,
                },
            )
            .await;
        match result {
            Err(e) if cfg.required => {
                self.remove_pending(&req.registry, &req.name, &req.version)
                    .await;
                if let Err(err) = self.storage.delete(storage_key).await {
                    tracing::error!("storage cleanup after sbom failure: {err}");
                }
                self.revoke_quota(&req.publisher, &req.registry, bytes)
                    .await;
                Err(e)
            }
            Err(e) => {
                tracing::warn!(error = %e, "sbom generation failed (non-fatal)");
                Ok(())
            }
            Ok(()) => Ok(()),
        }
    }
}
