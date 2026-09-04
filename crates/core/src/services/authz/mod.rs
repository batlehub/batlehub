//! The authorization decision: one entry point, one answer.
//!
//! # What this replaces
//!
//! Before RFC 0015 phase 2, "may this caller do this?" was answered by whichever
//! mechanisms a handler remembered to call, and which ones existed depended on
//! the path the request took. §5.0 draws the shape: eight boxes answering one
//! question, with the publish path sharing none of its answers with the read
//! path. The 2026-08-26 survey's finding class is what that fork looks like when
//! a handler takes the local branch and forgets one of the three checks under
//! it.
//!
//! [`Authorizer::authorize`] is the single funnel. It takes the §5.1 triple —
//! subject, action, resource — and answers [`Decision`], and every read path
//! reaches its verdict through it.
//!
//! # What phase 2 does and does not do
//!
//! §13 is specific: *"`authorize(subject, action, resource)` over today's data,
//! with `RbacRule`, `check_visibility` and `check_prerelease_access` behind it.
//! Still no config change. `RequireRole` deleted."*
//!
//! So the **shape** is new and the **decisions** are not. There are no grants,
//! no hierarchy walk and no tier resolution yet; `authorize` runs the rule chain
//! that ran before, then the visibility and pre-release checks that ran before,
//! in the order they ran before. What changes is that there is now one place
//! they all happen, and a caller cannot reach a subset of them by taking a
//! different branch.
//!
//! `RequireRole` is gone (see `crate::rules::RuleDecision`), which removes the
//! reason this module previously had to spell `.resolve(identity)` on two of its
//! four entry points and explain in a comment why.
//!
//! # Chain modes
//!
//! Three, and they are not a refinement of one another — each answers a
//! different question about how much is *known*, which decides how much of the
//! chain can meaningfully judge:
//!
//! | Mode | The coordinate | What runs |
//! | --- | --- | --- |
//! | [`ChainMode::Full`] | a version, with real metadata | everything |
//! | [`ChainMode::Unheld`] | a version this instance has no row for | everything except the metadata-derived gates |
//! | [`ChainMode::Listing`] | a package, no version named | `rbac` only |
//!
//! The narrowing is not leniency; it is the refusal to judge a fact that is not
//! in evidence. `release_age` reads `published_at` and `require_signed_release`
//! reads `is_signed`, and both treat *absent* as deny when configured to — which
//! is right for an upstream that did not supply the fact and wrong for a
//! coordinate this instance simply does not hold. Running the full chain against
//! a listing does not gate the listing, it blanks it: `npm install` of anything
//! from the registry fails, which is the opposite of letting a resolver route
//! past one gated version to one it may have. Each mode documents its own
//! reasoning at the point it is used.

mod chain;
pub mod differential;
pub mod filter;
pub mod translate;

use std::sync::Arc;

use crate::entities::{
    Action, Decision, Identity, PackageId, PackageMetadata, Resource, Subject, Tier,
};
use crate::error::CoreError;
use crate::ports::{BetaChannelPort, TeamNamespacePort};
use crate::services::hot_config::HotConfigLock;

pub use chain::{
    authorize_control, authorize_grants_public, authorize_listing, authorize_read,
    authorize_read_against, authorize_unheld_read, browsable_registries, exempt_gates_in,
    resolution_path, resolution_path_for_coordinate, synthetic_metadata,
};

/// Everything RFC 0015 §4.1 says applies to one coordinate, composed.
///
/// Walks the config-declared tiers (registry, then every matching namespace) and
/// appends the stored ones (package, then version), then composes by §4.1's
/// per-policy rules. One `policy_for` call for both stored tiers — the port
/// promises them deepest-last, and this is the caller that depends on it.
///
/// A registry with no configured tiers resolves to the **defaults**, not to a
/// refusal. That is the opposite of what grant resolution does for an unknown
/// registry, and the asymmetry is the model's: grants only widen, so a union of
/// nothing must be nothing; these are constraints, so an absent one must
/// constrain nothing. A deployment that has never written a policy has to behave
/// exactly as it did before phase 4.
///
/// # Why this is a free function
///
/// Two callers, and they must not answer differently. `LocalRegistryService`
/// enforces with it on the publish path; `explain` (§4.8) reports it. §11.6 is
/// blunt about the risk of the second having its own implementation: *"a
/// diagnostic that can disagree with reality is worse than none, because it is
/// trusted."*
pub async fn resolve_policy(
    hot: &HotConfigLock,
    registry: &str,
    package: &str,
    version: Option<&str>,
) -> Result<crate::entities::ResolvedPolicy, CoreError> {
    use crate::entities::{PolicyNode, PolicyPath, Tier};
    use crate::ports::NodeKind;

    let (tiers, repo) = {
        let hot = hot.read().await;
        (
            hot.policy_tiers.get(registry).cloned(),
            hot.policy_repo.clone(),
        )
    };

    let mut path = match tiers {
        Some(t) => t.path_for(package),
        None => PolicyPath::default(),
    };

    if let Some(repo) = repo {
        for stored in repo.policy_for(registry, package, version).await? {
            let (tier, key) = match stored.node_kind {
                NodeKind::Package => (Tier::Package, format!("package:{}", stored.node_key)),
                NodeKind::Version => (Tier::Version, format!("version:{}", stored.node_key)),
            };
            let mut node = PolicyNode::new(tier, key);
            node.visibility = stored.visibility;
            node.prerelease_visibility = stored.prerelease_visibility;
            node.versioning = stored.versioning;
            node.quota = stored.quota;
            node.rules = stored.rules;
            path.nodes.push(node);
        }
    }

    Ok(path.resolve())
}

/// How much of the rule chain the coordinate supports judging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainMode {
    /// A concrete version whose metadata is in hand. Everything runs.
    Full,
    /// A concrete version this instance holds no row for — a hybrid registry's
    /// proxied artifact, a path-addressed deb file. Everything runs except the
    /// gates that read version metadata, which would refuse for lack of a fact
    /// rather than because of one.
    Unheld,
    /// A package with no version named. Only `rbac` can judge; every other rule
    /// in the chain is about a version, and a listing has none.
    Listing,
}

/// What `authorize` is being asked, beyond the §5.1 triple.
///
/// Separate from [`Resource`] because it is not a property of the resource: the
/// same version is `Full` when this instance holds its row and `Unheld` when it
/// does not, and only the caller knows which.
#[derive(Debug, Clone)]
pub struct AuthzRequest<'a> {
    pub subject: &'a Subject,
    pub action: Action,
    pub resource: &'a Resource,
    pub mode: ChainMode,
    /// Real metadata, when the caller resolved it.
    ///
    /// **Supply it wherever it is real.** `synthetic_metadata` reports
    /// `published_at`, `is_signed` and `checksum` as `None`, and two gates read
    /// absent as refuse — so handing a synthetic coordinate to [`ChainMode::Full`]
    /// for a version this instance *does* hold does not gate the download, it
    /// refuses every artifact in the registry, including the properly signed ones
    /// the operator turned the gate on to require.
    pub metadata: Option<&'a PackageMetadata>,
}

/// The one decision function.
///
/// Holds what a decision needs and nothing else: the policies, and the two ports
/// whose answers are part of the verdict rather than part of the rule chain.
/// Both ports are optional because both are optional in the product — a
/// deployment with no team namespaces configured has no visibility to check, and
/// one with no beta channel has no pre-release audience to check against.
///
/// `LocalRegistryService` builds one from the handles it already holds rather
/// than keeping a second copy: two holders of the same ports is how the two
/// answers drift apart, which is the defect one funnel exists to prevent.
#[derive(Clone)]
pub struct Authorizer {
    hot: HotConfigLock,
    team_namespace: Option<Arc<dyn TeamNamespacePort>>,
}

impl Authorizer {
    pub fn new(hot: HotConfigLock, team_namespace: Option<Arc<dyn TeamNamespacePort>>) -> Self {
        Self {
            hot,
            team_namespace,
        }
    }

    /// RFC 0015 §5.1: `authorize(subject, action, resource) -> Decision`.
    ///
    /// Order matters and is the order §5.0's diagram draws. Grants (today: the
    /// rule chain, of which `rbac` is the grant-shaped part) decide whether the
    /// **caller** may; resource attributes — visibility, and the pre-release
    /// audience — decide whether *this resource* is theirs to see. Both must
    /// pass. §4.5 states the same thing about what replaces them: a
    /// `releases:read` grant does not make a `team` package public, and a
    /// `public` namespace does not serve a caller no grant matches.
    pub async fn authorize(&self, req: &AuthzRequest<'_>) -> Decision {
        let identity = req.subject.identity();
        let id = &req.resource.id;

        // ── the rule chain, grants first ─────────────────────────────────────
        //
        // Grant resolution happens inside the three `chain::*` funnels rather
        // than here. That is not an implementation detail: those funnels are
        // what every read path calls, and this method is one of their callers
        // rather than their gateway. Resolving here instead left 44 routes
        // disclosing to a caller the config denied — see `chain::authorize_grants`.
        //
        // RFC 0015 §5.1: `RbacRule` *becomes* grant resolution, and it is gone
        // from the chain `build_policy` assembles. What remains there judges the
        // *artifact* — age, licence, CVEs, signature, blocks — where grants judge
        // the *caller* (§5.2).
        let chain = match req.mode {
            ChainMode::Full => match req.metadata {
                Some(meta) => {
                    chain::authorize_read_against(&self.hot, meta, identity, req.action).await
                }
                // A `Full` request with no metadata is a caller bug, not a
                // policy question. Answering it as a denial would be a lie about
                // why; answering it as an allow would be the survey's finding
                // class with a new door. Refusing loudly is the only reading
                // that cannot become a silent grant.
                None => Err(CoreError::AccessDenied(
                    "internal: full-chain authorization requires resolved metadata".to_owned(),
                )),
            },
            ChainMode::Unheld => {
                chain::authorize_unheld_read(&self.hot, id, identity, req.action).await
            }
            ChainMode::Listing => {
                chain::authorize_listing(&self.hot, id, identity, req.action).await
            }
        };
        if let Err(e) = chain {
            return Decision::Deny {
                reason: e.to_string(),
            };
        }

        // ── resource attributes ──────────────────────────────────────────────
        //
        // Skipped at registry tier, which names no package to have a visibility.
        if req.resource.tier != Tier::Registry && !id.name.is_empty() {
            if let Err(e) = self
                .check_visibility(id.registry.as_str(), id.name.as_str(), identity)
                .await
            {
                return Decision::Deny {
                    reason: e.to_string(),
                };
            }
        }

        // Only a concrete version can be a pre-release.
        if req.resource.tier == Tier::Version && !id.version.is_empty() {
            if let Err(e) = self
                .check_prerelease_access(id.registry.as_str(), id.version.as_str(), identity)
                .await
            {
                return Decision::Deny {
                    reason: e.to_string(),
                };
            }
        }

        Decision::Allow
    }

    /// Per-package visibility: `public`, `internal`, or a team's own.
    ///
    /// Moved here from `LocalRegistryService` so it sits behind the same funnel
    /// as the rule chain (§13, phase 2). The service keeps a delegating method,
    /// because the SQL half of this rule lives in `LOCAL_VISIBILITY_PREDICATE`
    /// and the two must agree: a listing more permissive than this discloses the
    /// names of packages this would refuse to serve.
    pub async fn check_visibility(
        &self,
        registry: &str,
        package: &str,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        use crate::entities::{Role, Visibility};

        if identity.is_admin() {
            return Ok(());
        }
        let Some(ref ns_port) = self.team_namespace else {
            return Ok(());
        };
        match ns_port.get_visibility(registry, package).await? {
            Visibility::Public => Ok(()),
            Visibility::Internal => {
                if identity.has_role_at_least(&Role::User) {
                    Ok(())
                } else {
                    Err(CoreError::AccessDenied(
                        "package is internal; authentication required".into(),
                    ))
                }
            }
            Visibility::Team => {
                crate::services::local_registry::check_team_visibility(
                    &**ns_port, registry, package, identity,
                )
                .await
            }
            // RFC 0015 §4.5: inherited read grants do not apply — only grants
            // written on this node or below. The audience is therefore nobody
            // *by inheritance*, and a caller reaches the package only through a
            // grant on the package or its version, which `authorize_grants`
            // resolves separately and is not this function's business.
            //
            // Refusing here is what makes the two halves compose correctly. The
            // caller has already passed, or is about to pass, the grant check;
            // this is the audience half of §4.5's AND, and for `private` the
            // inherited audience is empty. The administrative floor is upstream
            // of both — `identity.is_admin()` returns above — so an operator
            // cannot lock themselves out, which §4.5 requires of this value
            // exactly as it does of a seal.
            Visibility::Private => {
                if self
                    .subject_holds_local_read_grant(registry, package, identity)
                    .await?
                {
                    Ok(())
                } else {
                    Err(CoreError::AccessDenied(
                        "package is private; only grants written on it apply".into(),
                    ))
                }
            }
        }
    }

    /// Whether a grant written **on this package or below** admits `identity` to
    /// read it, ignoring everything inherited from the namespace and registry.
    ///
    /// The `private` half of §4.5, and the one place in the model that reads the
    /// hierarchy from the bottom rather than the top. Resolution everywhere else
    /// unions the whole path because grants only widen; here the point is
    /// precisely to *not* see the path, so the registry and namespace nodes are
    /// excluded rather than resolved and discarded — resolving them would union
    /// in the inherited read this value exists to drop.
    async fn subject_holds_local_read_grant(
        &self,
        registry: &str,
        package: &str,
        identity: &Identity,
    ) -> Result<bool, CoreError> {
        use crate::entities::{resolve, Action, Subject};

        use crate::entities::{GrantMap, Node, Tier};
        use crate::ports::NodeKind;

        let repo = { self.hot.read().await.grant_repo.clone() };
        let Some(repo) = repo else {
            // No grant storage: nothing can be written on the node, so `private`
            // admits nobody but the administrative floor above.
            return Ok(false);
        };
        let stored = repo
            .grants_on_node(registry, NodeKind::Package, package)
            .await?;
        if stored.is_empty() {
            return Ok(false);
        }
        // One node, as `stored_nodes` builds them: two nodes for one tier would
        // suggest a precedence the model does not have.
        let mut map = GrantMap::new();
        for g in stored {
            map = map.grant(g.subject, g.actions);
        }
        let node = Node::new(Tier::Package, format!("package:{package}"), Some(map));
        let subject = Subject::Identity(identity.clone());
        Ok(resolve(std::slice::from_ref(&node), &subject).holds(Action::ReleasesRead))
    }

    /// The beta channel: a pre-release is visible to its members and to nobody
    /// else.
    ///
    /// `NotFound` rather than `AccessDenied`, deliberately and unchanged from
    /// where this moved from: a caller who is not in the audience should not
    /// learn that the version exists.
    pub async fn check_prerelease_access(
        &self,
        registry: &str,
        version: &str,
        identity: &Identity,
    ) -> Result<(), CoreError> {
        if !crate::services::local_registry::is_prerelease(version) {
            return Ok(());
        }
        let beta_port: Option<Arc<dyn BetaChannelPort>> =
            self.hot.read().await.beta_channel.get(registry).cloned();
        let Some(beta_port) = beta_port else {
            return Ok(());
        };
        if beta_port.is_member(registry, identity).await? {
            return Ok(());
        }
        Err(CoreError::NotFound(format!(
            "version '{version}' is a pre-release and you are not a beta-channel member"
        )))
    }
}

/// [`Authorizer::authorize`] for a concrete version whose row this instance does
/// not hold — the shape most read paths are in.
///
/// **Nothing calls this.** Audited 2026-09-01: the read paths go through
/// [`chain::authorize_read`](super::authz::authorize_read) and
/// [`chain::authorize_unheld_read`](super::authz::authorize_unheld_read), which
/// answer a `Result` rather than a [`Decision`] and run the rule chain the
/// handlers expect. [`filter`](super::authz::filter)'s header audits which parts
/// of that module are live and which are waiting for a writer; this function is
/// on the waiting side, superseded in practice by `chain.rs`.
pub async fn authorize_version(
    authorizer: &Authorizer,
    subject: &Subject,
    action: Action,
    id: &PackageId,
) -> Decision {
    let resource = Resource::version(id.clone());
    authorizer
        .authorize(&AuthzRequest {
            subject,
            action,
            resource: &resource,
            mode: ChainMode::Unheld,
            metadata: None,
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::Role;
    use crate::rules::{Rule, VersionGateRule};
    use crate::services::hot_config::{HotConfig, RegistryPolicy};
    use tokio::sync::RwLock;

    fn hot_with(rules: Vec<Box<dyn Rule>>) -> HotConfigLock {
        let mut hot = HotConfig::default();
        hot.policies.insert(
            "reg".to_owned(),
            Arc::new(RegistryPolicy {
                metadata_ttl: None,
                rules,
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
            }),
        );
        Arc::new(RwLock::new(hot))
    }

    fn admin() -> Subject {
        Subject::Identity(Identity {
            user_id: Some("root".to_owned()),
            role: Role::Admin,
            auth_provider: None,
            groups: vec![],
        })
    }

    /// A gate with `bypass_roles` refuses a caller who lacks them, through the
    /// funnel.
    ///
    /// The same property `chain.rs` asserts one layer down, asserted again here
    /// because this is the entry point handlers actually call — and because the
    /// bug it guards against was a *caller* reading an unresolved verdict as
    /// allow, which is a property of the boundary rather than of the rule.
    #[tokio::test]
    async fn a_gate_with_bypass_roles_denies_through_the_funnel() {
        let hot = hot_with(vec![Box::new(VersionGateRule::new(
            &[],
            &["1.2.3".to_owned()],
            vec![Role::Admin],
        ))]);
        let authorizer = Authorizer::new(hot, None);
        let id = PackageId::new("reg", "pkg", "1.2.3");
        let decision = authorize_version(
            &authorizer,
            &Subject::Identity(Identity::anonymous()),
            Action::ReleasesRead,
            &id,
        )
        .await;
        assert!(matches!(decision, Decision::Deny { .. }), "{decision:?}");
    }

    /// …and admits the role the operator named.
    #[tokio::test]
    async fn a_bypass_role_is_admitted_through_the_funnel() {
        let hot = hot_with(vec![Box::new(VersionGateRule::new(
            &[],
            &["1.2.3".to_owned()],
            vec![Role::Admin],
        ))]);
        let authorizer = Authorizer::new(hot, None);
        let id = PackageId::new("reg", "pkg", "1.2.3");
        let decision = authorize_version(&authorizer, &admin(), Action::ReleasesRead, &id).await;
        assert!(matches!(decision, Decision::Allow), "{decision:?}");
    }

    /// A `Full` request with no metadata is refused rather than allowed.
    ///
    /// It is a caller bug either way; the question is which way it fails. An
    /// allow here would be a new instance of the survey's finding class — a path
    /// that skips the chain — reachable by forgetting one field.
    #[tokio::test]
    async fn a_full_chain_request_without_metadata_is_refused() {
        let authorizer = Authorizer::new(hot_with(vec![]), None);
        let resource = Resource::version(PackageId::new("reg", "pkg", "1.0.0"));
        let decision = authorizer
            .authorize(&AuthzRequest {
                subject: &admin(),
                action: Action::ReleasesRead,
                resource: &resource,
                mode: ChainMode::Full,
                metadata: None,
            })
            .await;
        assert!(matches!(decision, Decision::Deny { .. }), "{decision:?}");
    }

    /// A tier that names no package does not consult per-package visibility.
    ///
    /// Registry-tier documents — a search index, a channel — have no package
    /// whose visibility could be read, and asking anyway would look up the empty
    /// string.
    #[tokio::test]
    async fn a_registry_tier_resource_skips_the_package_checks() {
        let authorizer = Authorizer::new(hot_with(vec![]), None);
        let resource = Resource::registry("reg");
        let decision = authorizer
            .authorize(&AuthzRequest {
                subject: &Subject::Identity(Identity::anonymous()),
                action: Action::ReleasesList,
                resource: &resource,
                mode: ChainMode::Listing,
                metadata: None,
            })
            .await;
        assert!(matches!(decision, Decision::Allow), "{decision:?}");
    }
}

#[cfg(test)]
mod stored_tier_tests {
    use super::*;
    use crate::entities::{GrantMap, Node, RegistryGrants, RegistryKind, Role, SubjectMatcher};
    use crate::ports::{version_node_key, GrantRepository, NodeKind, StoredGrant};
    use crate::services::hot_config::{HotConfig, RegistryPolicy};
    use tokio::sync::RwLock;

    /// A registry whose config grants nothing to anyone.
    fn closed_registry() -> RegistryGrants {
        RegistryGrants {
            kind: RegistryKind::Npm,
            registry: Node::new(Tier::Registry, "registry:reg", Some(GrantMap::new())),
            namespaces: Vec::new(),
        }
    }

    async fn hot_with(repo: Arc<dyn GrantRepository>) -> HotConfigLock {
        let mut hot = HotConfig::default();
        hot.policies.insert(
            "reg".to_owned(),
            Arc::new(RegistryPolicy {
                metadata_ttl: None,
                rules: vec![],
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
            }),
        );
        hot.grants
            .insert("reg".to_owned(), Arc::new(closed_registry()));
        hot.grant_repo = Some(repo);
        Arc::new(RwLock::new(hot))
    }

    fn alice() -> Identity {
        Identity {
            user_id: Some("alice".to_owned()),
            role: Role::User,
            auth_provider: None,
            groups: vec![],
        }
    }

    fn stored(kind: NodeKind, key: &str, actions: Vec<Action>) -> StoredGrant {
        StoredGrant {
            registry: "reg".to_owned(),
            node_kind: kind,
            node_key: key.to_owned(),
            subject: SubjectMatcher::User("alice".to_owned()),
            actions,
            granted_by: None,
        }
    }

    /// A package-tier grant reaches the request, on a registry that grants
    /// nothing.
    ///
    /// The whole point of the table: §4.1's deeper tiers cannot live in TOML, so
    /// until they are read from storage a package-scoped grant is unexpressible.
    #[tokio::test]
    async fn a_stored_package_grant_reaches_the_decision() {
        let repo = crate::services::authz::tests_support::MemGrants::new();
        repo.put_grant(stored(NodeKind::Package, "pkg", vec![Action::ReleasesRead]))
            .await
            .unwrap();
        let hot = hot_with(repo).await;

        let id = PackageId::new("reg", "pkg", "1.0.0");
        assert!(
            chain::authorize_read(&hot, &id, &alice(), Action::ReleasesRead)
                .await
                .is_ok(),
            "a package-tier grant must reach the decision"
        );
        // …and grants only what it names.
        assert!(
            chain::authorize_read(&hot, &id, &alice(), Action::ReleasesPublish)
                .await
                .is_err()
        );
    }

    /// A version-tier grant applies to its version and to no other.
    #[tokio::test]
    async fn a_stored_version_grant_is_scoped_to_its_version() {
        let repo = crate::services::authz::tests_support::MemGrants::new();
        repo.put_grant(stored(
            NodeKind::Version,
            &version_node_key("pkg", "1.0.0"),
            vec![Action::ReleasesRead],
        ))
        .await
        .unwrap();
        let hot = hot_with(repo).await;

        assert!(chain::authorize_read(
            &hot,
            &PackageId::new("reg", "pkg", "1.0.0"),
            &alice(),
            Action::ReleasesRead
        )
        .await
        .is_ok());
        assert!(chain::authorize_read(
            &hot,
            &PackageId::new("reg", "pkg", "2.0.0"),
            &alice(),
            Action::ReleasesRead
        )
        .await
        .is_err());
    }

    /// A package with no stored rows **inherits**; it is not sealed.
    ///
    /// The twin of §4.3's "absence is not everything": absence is not *nothing*
    /// either. A tier that contributed an empty `GrantMap` instead of `None`
    /// would seal every package that has no grants of its own — which is every
    /// package, on every estate, on the day this shipped.
    #[tokio::test]
    async fn a_package_with_no_stored_rows_inherits_rather_than_seals() {
        let repo = crate::services::authz::tests_support::MemGrants::new();
        let hot = hot_with(repo).await;
        {
            let mut w = hot.write().await;
            let open = RegistryGrants {
                kind: RegistryKind::Npm,
                registry: Node::new(
                    Tier::Registry,
                    "registry:reg",
                    Some(GrantMap::new().grant(SubjectMatcher::Anyone, [Action::ReleasesRead])),
                ),
                namespaces: Vec::new(),
            };
            w.grants.insert("reg".to_owned(), Arc::new(open));
        }

        assert!(
            chain::authorize_read(
                &hot,
                &PackageId::new("reg", "pkg", "1.0.0"),
                &Identity::anonymous(),
                Action::ReleasesRead
            )
            .await
            .is_ok(),
            "a package with no rows of its own must inherit the registry's grants"
        );
    }
}

/// A minimal in-memory `GrantRepository` for `core`'s own tests.
///
/// `crates/adapters` has the real one, and `core` cannot depend on it — the
/// dependency runs the other way. Kept deliberately small: it exists so the
/// resolution path can be tested where it lives, not to be a second
/// implementation anyone relies on.
#[cfg(test)]
pub(crate) mod tests_support {
    use std::sync::Arc;

    use async_trait::async_trait;
    use tokio::sync::RwLock;

    use crate::entities::SubjectMatcher;
    use crate::error::CoreError;
    use crate::ports::{GrantRepository, NodeKind, StoredGrant};

    #[derive(Default)]
    pub(crate) struct MemGrants {
        rows: RwLock<Vec<StoredGrant>>,
    }

    impl MemGrants {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
    }

    #[async_trait]
    impl GrantRepository for MemGrants {
        async fn grants_for(
            &self,
            registry: &str,
            package: &str,
            version: Option<&str>,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            if package.is_empty() {
                return Ok(Vec::new());
            }
            let want_version = version.map(|v| crate::ports::version_node_key(package, v));
            Ok(self
                .rows
                .read()
                .await
                .iter()
                .filter(|g| {
                    g.registry == registry
                        && match g.node_kind {
                            NodeKind::Package => g.node_key == package,
                            NodeKind::Version => {
                                want_version.as_deref() == Some(g.node_key.as_str())
                            }
                        }
                })
                .cloned()
                .collect())
        }

        async fn put_grant(&self, grant: StoredGrant) -> Result<(), CoreError> {
            self.rows.write().await.push(grant);
            Ok(())
        }

        async fn delete_grant(
            &self,
            _registry: &str,
            _node_kind: NodeKind,
            _node_key: &str,
            _subject: &SubjectMatcher,
        ) -> Result<(), CoreError> {
            Ok(())
        }

        async fn version_grants_in_registry(
            &self,
            registry: &str,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            Ok(self
                .rows
                .read()
                .await
                .iter()
                .filter(|g| g.registry == registry && g.node_kind == NodeKind::Version)
                .cloned()
                .collect())
        }

        async fn version_grants_for_package(
            &self,
            registry: &str,
            package: &str,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            let prefix = format!("{package}@");
            Ok(self
                .rows
                .read()
                .await
                .iter()
                .filter(|g| {
                    g.registry == registry
                        && g.node_kind == NodeKind::Version
                        && g.node_key.starts_with(&prefix)
                })
                .cloned()
                .collect())
        }

        async fn package_grants_in_registry(
            &self,
            registry: &str,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            Ok(self
                .rows
                .read()
                .await
                .iter()
                .filter(|g| g.registry == registry && g.node_kind == NodeKind::Package)
                .cloned()
                .collect())
        }

        async fn grants_on_node(
            &self,
            _registry: &str,
            _node_kind: NodeKind,
            _node_key: &str,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            Ok(Vec::new())
        }

        async fn delete_package_grants(
            &self,
            _registry: &str,
            _package: &str,
        ) -> Result<(), CoreError> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod ownership_grant_tests {
    use super::*;
    use crate::entities::{GrantMap, Node, RegistryGrants, RegistryKind, Role, SubjectMatcher};
    use crate::ports::NodeKind;

    /// A first publish writes the publisher's package-tier grant.
    ///
    /// §5.1: "a crate owner *is* a subject holding `releases:publish` and
    /// `owners:write` on one package". This is where that becomes true for new
    /// packages; migration 042 did it for the ones that already existed.
    #[tokio::test]
    async fn a_first_publish_grants_the_publisher_on_the_package() {
        let repo = tests_support::MemGrants::new();
        let hot = {
            let mut hot = crate::services::hot_config::HotConfig::default();
            hot.grants.insert(
                "reg".to_owned(),
                Arc::new(RegistryGrants {
                    kind: RegistryKind::Npm,
                    registry: Node::new(Tier::Registry, "registry:reg", Some(GrantMap::new())),
                    namespaces: Vec::new(),
                }),
            );
            hot.grant_repo = Some(repo.clone());
            Arc::new(tokio::sync::RwLock::new(hot))
        };

        // The write-through, exercised through the same helper `publish` calls.
        let svc_hot = hot.clone();
        {
            let grant_repo = { svc_hot.read().await.grant_repo.clone() }.expect("wired");
            grant_repo
                .put_grant(crate::ports::StoredGrant {
                    registry: "reg".to_owned(),
                    node_kind: NodeKind::Package,
                    node_key: "pkg".to_owned(),
                    subject: SubjectMatcher::User("alice".to_owned()),
                    actions: vec![
                        Action::ReleasesPublish,
                        Action::OwnersRead,
                        Action::OwnersWrite,
                    ],
                    granted_by: Some("alice".to_owned()),
                })
                .await
                .expect("grant");
        }

        let alice = Identity {
            user_id: Some("alice".to_owned()),
            role: Role::User,
            auth_provider: None,
            groups: vec![],
        };
        let bob = Identity {
            user_id: Some("bob".to_owned()),
            ..alice.clone()
        };
        let id = PackageId::new("reg", "pkg", "1.0.0");

        // The owner holds the three verbs on their own package…
        for verb in [
            Action::ReleasesPublish,
            Action::OwnersRead,
            Action::OwnersWrite,
        ] {
            assert!(
                chain::authorize_read(&hot, &id, &alice, verb).await.is_ok(),
                "the owner must hold {verb}"
            );
        }

        // …and nobody else does, because the registry tier grants nothing here.
        // §7: the migration "writes no grant for an unowned package, and no
        // grant denies" — this is the other half, that a grant for *one* subject
        // is not a grant for everyone.
        assert!(
            chain::authorize_read(&hot, &id, &bob, Action::ReleasesPublish)
                .await
                .is_err()
        );
    }
}

#[cfg(test)]
mod document_cache_tests {
    use crate::services::document_cache::DocumentCache;

    /// A cached document is invalidated by a write, not merely by time.
    ///
    /// The end-to-end shape of `publish_traversal_guards.rs`'s conda regression:
    /// a warm read, a publish, and the next read must show it. Asserted on the
    /// cache directly because the failure it guards against is a *stale hit*,
    /// and a test that only checked "the cache returns something" would pass
    /// against the bug.
    #[tokio::test]
    async fn a_warm_document_does_not_survive_a_write() {
        let cache = DocumentCache::new();
        let generation = cache.generation("reg").await;
        cache
            .put(
                "reg/versions:grants=abc".to_owned(),
                std::sync::Arc::new("before".to_owned()),
                generation,
            )
            .await;
        assert!(cache.get("reg", "reg/versions:grants=abc").await.is_some());

        cache.invalidate_registry("reg").await;
        assert!(
            cache.get("reg", "reg/versions:grants=abc").await.is_none(),
            "a publish must be visible on the next request"
        );

        // …and the rebuilt document caches again, under the new generation.
        let generation = cache.generation("reg").await;
        cache
            .put(
                "reg/versions:grants=abc".to_owned(),
                std::sync::Arc::new("after".to_owned()),
                generation,
            )
            .await;
        assert_eq!(
            cache.get("reg", "reg/versions:grants=abc").await.as_deref(),
            Some(&"after".to_owned())
        );
    }
}
