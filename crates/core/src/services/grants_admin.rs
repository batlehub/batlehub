//! The writer RFC 0015 built two tiers for and never supplied.
//!
//! [RFC 0017]. Migration 041 gave the `grants` table a `node_kind` of `package`
//! or `version`, [`GrantRepository`] can write either, and `chain::stored_nodes`
//! reads both on every `authorize` — but the only `put_grant` caller in the tree
//! was the ownership projection, which writes package rows carrying exactly
//! three verbs and has never written a version row. So `releases:read` on one
//! package for one group — RFC 0015 §4.4's own opening example — was
//! unexpressible, and the version tier was walked on the hot path for a
//! capability nothing could populate.
//!
//! [RFC 0017]: https://batleforc.git.batleforc.fr/batlehub/rfc/0017-writing-grants-at-the-package-and-version-tiers
//!
//! # Why this is a service and not handler code
//!
//! Two callers — the admin API and the CLI — and one definition of a legal
//! grant. Validation in a handler is validation the CLI can route around, and
//! §4.4's table is the kind of rule that gets re-implemented slightly
//! differently the second time. The handler is a thin translation of HTTP to
//! this; `cli` goes through the same HTTP, so there is exactly one implementation
//! and one place to read it.
//!
//! # What it deliberately cannot do
//!
//! **Write a seal.** [`crate::ports::StoredGrant`] has no representation for
//! one and this adds none: §4.3 confines sealing to the config file, because a
//! delegate holding this verb could otherwise lock the registry owner out of a
//! package. An empty action set is not an empty grant here, it is a *removal*,
//! and [`GrantAdminService::set`] refuses it rather than writing a row the
//! table's own `ck_grants_actions_non_empty` would refuse anyway.
//!
//! **Take access away.** Grants only widen (§4.3). Nothing written here can
//! narrow what a broader tier granted, which is why the worst a wrong write does
//! is grant access — a direction an audit trail catches after the fact, and the
//! reason every mutation records one.

use std::sync::Arc;

use crate::entities::PublishedPackage;
use crate::entities::{
    expand_patterns_for, Action, ActionParseError, GrantSet, Identity, RegistryKind, Subject,
    SubjectMatcher, SubjectParseError, WildcardScope,
};
use crate::error::CoreError;
use crate::ports::{
    version_node_key, GrantRepository, LocalRegistryBackend, NodeKind, OwnershipPort, StoredGrant,
};
use crate::services::hot_config::HotConfigLock;
use crate::services::ownership_grants::{subject_for_owner, OWNERSHIP_ACTIONS};

/// The one question this service asks of the local registry.
///
/// A narrow trait rather than the 22-method [`LocalRegistryBackend`], because
/// that is the whole of the dependency: §4.4 needs to know whether a version
/// exists and whether it is yanked, and nothing else. Naming the dependency at
/// its real width is also what makes the validation table testable — a double
/// for this is four lines, a double for the backend is not, and a rule nobody
/// can test cheaply is a rule that ends up asserted through the HTTP layer or
/// not at all.
#[async_trait::async_trait]
pub trait VersionLookup: Send + Sync {
    async fn versions_of(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<PublishedPackage>, CoreError>;
}

/// The production implementation: the local registry backend.
pub struct BackendVersions(pub Arc<dyn LocalRegistryBackend>);

#[async_trait::async_trait]
impl VersionLookup for BackendVersions {
    async fn versions_of(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<PublishedPackage>, CoreError> {
        self.0.get_versions(registry, package).await
    }
}

/// Which node a request addresses: a package, or one version of it.
///
/// Carried as one type rather than an `Option<String>` threaded through six
/// signatures, because every operation here has to answer "which tier" and the
/// answer decides the node key, the audit coordinate and whether a version has
/// to exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantTarget {
    pub registry: String,
    pub package: String,
    /// `None` addresses the package node; `Some` the version node.
    pub version: Option<String>,
}

impl GrantTarget {
    pub fn package(registry: impl Into<String>, package: impl Into<String>) -> Self {
        Self {
            registry: registry.into(),
            package: package.into(),
            version: None,
        }
    }

    pub fn version(
        registry: impl Into<String>,
        package: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            registry: registry.into(),
            package: package.into(),
            version: Some(version.into()),
        }
    }

    pub fn node_kind(&self) -> NodeKind {
        match self.version {
            Some(_) => NodeKind::Version,
            None => NodeKind::Package,
        }
    }

    /// The key this node is stored under — `name` or `name@version`, exactly as
    /// [`version_node_key`] spells it, so the CLI's `name@version` positional
    /// argument and the storage key read the same.
    pub fn node_key(&self) -> String {
        match &self.version {
            Some(v) => version_node_key(&self.package, v),
            None => self.package.clone(),
        }
    }
}

/// Something legal but probably not what the operator meant.
///
/// Reported rather than refused, because every one of these is a *valid* grant
/// (§4.4's second table): refusing would make the editor disagree with the model
/// about what a legal grant is. Returned on the response and surfaced by the CLI
/// so that "I wrote it and nothing happened" is answered at the moment of
/// writing rather than discovered later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantWarning {
    /// The subject already holds every one of these verbs from a broader tier.
    /// Legal and inert — grants union — but an operator who wrote it believed it
    /// did something.
    Redundant { actions: Vec<Action> },
    /// A version-tier grant on a version that is yanked or deleted. The
    /// coordinate is spent; the row will resolve for nothing.
    SpentCoordinate { version: String },
}

impl std::fmt::Display for GrantWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrantWarning::Redundant { actions } => write!(
                f,
                "subject already holds {} from a broader tier; the grant is legal and inert",
                actions
                    .iter()
                    .map(|a| a.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            GrantWarning::SpentCoordinate { version } => write!(
                f,
                "version '{version}' is yanked or deleted; this grant will resolve for nothing"
            ),
        }
    }
}

/// The write funnel for the two stored tiers.
pub struct GrantAdminService {
    hot: HotConfigLock,
    /// The one question validation cannot answer without the local registry:
    /// does this version exist, and is it spent? `None` on a pure-proxy
    /// deployment, where a version-tier grant has no local coordinate to hang
    /// from and the check degrades to "not checkable" rather than to "refused".
    versions: Option<Arc<dyn VersionLookup>>,
    /// The ownership list, for §4.3's `409`. `None` disables that refusal, which
    /// is correct: with no ownership port there is no projection to fight with.
    ownership: Option<Arc<dyn OwnershipPort>>,
}

impl GrantAdminService {
    pub fn new(
        hot: HotConfigLock,
        versions: Option<Arc<dyn VersionLookup>>,
        ownership: Option<Arc<dyn OwnershipPort>>,
    ) -> Self {
        Self {
            hot,
            versions,
            ownership,
        }
    }

    /// Whether grant storage is wired at all.
    ///
    /// The web layer asks this first so an unconfigured deployment answers
    /// `503` rather than the `500` a bare `CoreError::Config` would become —
    /// the same shape `require_ownership` uses for the ownership port.
    pub async fn is_configured(&self) -> bool {
        self.hot.read().await.grant_repo.is_some()
    }

    async fn repo(&self) -> Result<Arc<dyn GrantRepository>, CoreError> {
        self.hot
            .read()
            .await
            .grant_repo
            .clone()
            .ok_or_else(|| CoreError::Config("grant storage is not configured".to_owned()))
    }

    /// The ecosystem of `registry`, which decides whether an ecosystem-scoped
    /// verb is legal here (§4.2 rule 2).
    ///
    /// Unknown registry is an error rather than a permissive default: a grant
    /// written against a registry this server does not serve is a typo whose
    /// only symptom would be a row nothing ever reads.
    async fn registry_kind(&self, registry: &str) -> Result<RegistryKind, CoreError> {
        self.hot
            .read()
            .await
            .grants
            .get(registry)
            .map(|g| g.kind)
            .ok_or_else(|| CoreError::NotFound(format!("registry '{registry}' is not configured")))
    }

    /// Every grant on one node.
    pub async fn list(&self, target: &GrantTarget) -> Result<Vec<StoredGrant>, CoreError> {
        let repo = self.repo().await?;
        repo.grants_on_node(&target.registry, target.node_kind(), &target.node_key())
            .await
    }

    /// Both tiers for one package, which is what an operator actually wants to
    /// see: the package rows and the rows on each of its versions.
    ///
    /// # Two queries, not one per version
    ///
    /// This enumerated versions through [`VersionLookup`] and asked
    /// `grants_on_node` for each, under a comment saying the port offered no
    /// "every version row of this package" query. That was true when the comment
    /// was written and false by the time it shipped: this RFC added
    /// [`GrantRepository::version_grants_for_package`] for the listing filter,
    /// and it answers exactly this question in one indexed query.
    ///
    /// The cost was the smaller half of the problem. Driving the listing off the
    /// *version list* meant a row could exist and not be listed:
    ///
    /// - **A grant on a version that was later deleted became invisible.** It
    ///   kept resolving — `version_grants_in_registry` still feeds it to
    ///   `Readable::with_version_grants`, so it went on admitting the package
    ///   name into listings — while `GET /grants`, `batlehub admin grants list`
    ///   and the console panel all showed nothing. An operator could neither
    ///   discover it nor remove it through the surface built to remove it.
    /// - **With no local backend wired, no version row was ever listed**, though
    ///   `set` writes them: `check_version` degrades to *not checkable* rather
    ///   than to *refused*, so the write succeeds and the read showed nothing.
    ///
    /// Reading the rows themselves has neither failure, and does not depend on
    /// the backend at all.
    ///
    /// Sorted here rather than by the store: this is a listing an operator
    /// reads, the CLI renders as a table and the console paginates, and neither
    /// adapter promises an order.
    pub async fn list_for_package(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<StoredGrant>, CoreError> {
        let repo = self.repo().await?;
        let mut out = repo
            .grants_on_node(registry, NodeKind::Package, package)
            .await?;
        let mut versions = repo.version_grants_for_package(registry, package).await?;
        versions.sort_by(|a, b| {
            (&a.node_key, a.subject.as_string()).cmp(&(&b.node_key, b.subject.as_string()))
        });
        out.extend(versions);
        Ok(out)
    }

    /// Write one subject's grant on `target`, replacing that subject's row.
    ///
    /// The order of the checks is the order of §4.4's table, and it is chosen so
    /// that the most specific complaint wins: an operator who misspells both a
    /// subject and a verb is told about the subject first, because the verb list
    /// they meant depends on which subject they meant.
    pub async fn set(
        &self,
        target: &GrantTarget,
        subject: &str,
        action_patterns: &[String],
        granted_by: &Identity,
    ) -> Result<Vec<GrantWarning>, CoreError> {
        let repo = self.repo().await?;
        let kind = self.registry_kind(&target.registry).await?;

        let matcher = SubjectMatcher::parse(subject)
            .map_err(|e: SubjectParseError| CoreError::InvalidInput(e.to_string()))?;

        if action_patterns.is_empty() {
            return Err(CoreError::InvalidInput(
                "a grant needs at least one action; to remove a subject's grant, delete it"
                    .to_owned(),
            ));
        }

        // §4.2: expansion happens at write, never at evaluation. `expand_patterns_for`
        // is the same function config load uses, so `releases:*` means the same
        // set in both places and an ecosystem verb is refused on the wrong
        // registry here exactly as it is there.
        let actions = expand_patterns_for(action_patterns, WildcardScope::Everything, Some(kind))
            .map_err(|e: ActionParseError| CoreError::InvalidInput(e.to_string()))?;
        if actions.is_empty() {
            return Err(CoreError::InvalidInput(
                "the action patterns expanded to nothing".to_owned(),
            ));
        }

        let mut warnings = Vec::new();
        if let Some(version) = &target.version {
            warnings.extend(
                self.check_version(&target.registry, &target.package, version)
                    .await?,
            );
        }

        self.refuse_ownership_verbs(target, &matcher, &actions)
            .await?;

        if let Some(w) = self.redundancy(target, &matcher, &actions).await? {
            warnings.push(w);
        }

        repo.put_grant(StoredGrant {
            registry: target.registry.clone(),
            node_kind: target.node_kind(),
            node_key: target.node_key(),
            subject: matcher,
            actions,
            granted_by: granted_by.user_id.clone(),
        })
        .await?;

        Ok(warnings)
    }

    /// Remove one subject's grant. Absent is not an error, matching the port.
    ///
    /// Returns whether a row was actually there, so a caller can tell "removed"
    /// from "nothing to remove" — the audit event is worth writing either way,
    /// but the operator deserves to know which happened.
    pub async fn remove(&self, target: &GrantTarget, subject: &str) -> Result<bool, CoreError> {
        let repo = self.repo().await?;
        let matcher = SubjectMatcher::parse(subject)
            .map_err(|e: SubjectParseError| CoreError::InvalidInput(e.to_string()))?;

        // Ownership rows are the projection's to remove. Letting this delete one
        // would make `owners remove` the only way to restore it, and the next
        // owner change would silently put it back — the same race §4.3 refuses
        // on the write side, arriving through the delete.
        self.refuse_ownership_removal(target, &matcher).await?;

        let existed = repo
            .grants_on_node(&target.registry, target.node_kind(), &target.node_key())
            .await?
            .into_iter()
            .any(|g| g.subject == matcher);

        repo.delete_grant(
            &target.registry,
            target.node_kind(),
            &target.node_key(),
            &matcher,
        )
        .await?;
        Ok(existed)
    }

    /// §4.4 — a version-tier grant needs a version to hang from.
    ///
    /// A grant on a coordinate that does not exist is a typo more often than a
    /// plan, and the row would resolve for nobody. A *yanked* version is a
    /// different case and only warns: the coordinate is real, still resolvable
    /// by exact pin, and an operator granting read on it may well mean it.
    async fn check_version(
        &self,
        registry: &str,
        package: &str,
        version: &str,
    ) -> Result<Vec<GrantWarning>, CoreError> {
        let Some(lookup) = &self.versions else {
            // Nothing local to check against. Not an error: §11 open question 1
            // settles that a version-tier grant names a *local* coordinate, and
            // a deployment with no local backend has none to validate against.
            return Ok(Vec::new());
        };
        let versions = lookup.versions_of(registry, package).await?;
        let Some(found) = versions.iter().find(|p| p.version == version) else {
            return Err(CoreError::InvalidInput(format!(
                "package '{package}' in registry '{registry}' has no version '{version}'"
            )));
        };
        if found.yanked {
            return Ok(vec![GrantWarning::SpentCoordinate {
                version: version.to_owned(),
            }]);
        }
        Ok(Vec::new())
    }

    /// §4.3 — the ownership verbs are not editable through this surface.
    ///
    /// Two writers on one table is the design; two writers on one *verb* is a
    /// race with no winner. The projection rewrites its three verbs on every
    /// owner change, so an editor that could drop one would have its edit
    /// silently undone the next time anybody touched the owner list — the worst
    /// shape of all, because the grant would look written.
    ///
    /// Only a write that would *remove* one is refused. Granting a verb the
    /// subject already holds by ownership is redundant, not conflicting, and
    /// falls out as the §4.4 warning instead.
    async fn refuse_ownership_verbs(
        &self,
        target: &GrantTarget,
        matcher: &SubjectMatcher,
        actions: &[Action],
    ) -> Result<(), CoreError> {
        let held = self.ownership_verbs(target, matcher).await?;
        let dropped: Vec<Action> = held.into_iter().filter(|a| !actions.contains(a)).collect();
        if dropped.is_empty() {
            return Ok(());
        }
        Err(CoreError::Conflict(format!(
            "'{}' holds {} on '{}' through ownership; this write would drop {}. \
             Use `batlehub-cli owners remove` to change ownership.",
            matcher,
            OWNERSHIP_ACTIONS
                .iter()
                .map(|a| a.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            target.package,
            dropped
                .iter()
                .map(|a| a.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }

    async fn refuse_ownership_removal(
        &self,
        target: &GrantTarget,
        matcher: &SubjectMatcher,
    ) -> Result<(), CoreError> {
        if self.ownership_verbs(target, matcher).await?.is_empty() {
            return Ok(());
        }
        Err(CoreError::Conflict(format!(
            "'{}' holds this grant through ownership of '{}'; remove the owner with \
             `batlehub-cli owners remove` instead",
            matcher, target.package
        )))
    }

    /// The verbs `matcher` holds on this package by being an owner.
    ///
    /// Package tier only. Ownership is a property of a package name — the
    /// projection writes `NodeKind::Package` and nothing else — so a version-tier
    /// row can never collide with it, and a version-tier write is never refused
    /// by this check.
    async fn ownership_verbs(
        &self,
        target: &GrantTarget,
        matcher: &SubjectMatcher,
    ) -> Result<Vec<Action>, CoreError> {
        if target.version.is_some() {
            return Ok(Vec::new());
        }
        let Some(ownership) = &self.ownership else {
            return Ok(Vec::new());
        };
        let owners = ownership
            .list_owners(&target.registry, &target.package)
            .await?;
        let owns = owners.iter().any(|o| {
            subject_for_owner(&o.principal_type, &o.principal_id).as_ref() == Some(matcher)
        });
        Ok(if owns {
            OWNERSHIP_ACTIONS.to_vec()
        } else {
            Vec::new()
        })
    }

    /// §4.4 — does a broader tier already grant all of this?
    ///
    /// Resolved over the instance, registry and namespace nodes only: the point
    /// is what the subject holds *without* the row being written, so including
    /// the package node would make a re-write of an existing row report itself
    /// as redundant.
    ///
    /// A `SubjectMatcher` is not a caller, so this asks the question of a
    /// synthetic identity the matcher describes — see [`synthetic_subject`] for
    /// which forms that is exact for and which it approximates. Approximate in
    /// the safe direction: a missed warning is a message not printed, never a
    /// grant not written, which is why this went unnoticed for `group:*:`.
    async fn redundancy(
        &self,
        target: &GrantTarget,
        matcher: &SubjectMatcher,
        actions: &[Action],
    ) -> Result<Option<GrantWarning>, CoreError> {
        let (grants, instance) = {
            let hot = self.hot.read().await;
            (
                hot.grants.get(&target.registry).cloned(),
                hot.instance.clone(),
            )
        };
        let Some(grants) = grants else {
            return Ok(None);
        };
        let Some(subject) = synthetic_subject(matcher) else {
            return Ok(None);
        };

        let mut nodes: Vec<crate::entities::Node> = Vec::new();
        if let Some(instance) = instance.as_deref() {
            nodes.push(instance.clone());
        }
        nodes.push(grants.registry.clone());
        for (prefix, node) in &grants.namespaces {
            if crate::entities::namespace_matches(grants.kind, prefix, &target.package) {
                nodes.push(node.clone());
            }
        }

        let resolved: GrantSet = crate::entities::resolve(&nodes, &subject);
        let already: Vec<Action> = actions
            .iter()
            .copied()
            .filter(|a| resolved.holds(*a))
            .collect();
        if already.len() == actions.len() {
            Ok(Some(GrantWarning::Redundant { actions: already }))
        } else {
            Ok(None)
        }
    }
}

/// The caller a subject matcher describes, for the redundancy question.
///
/// `None` for the forms that describe no single caller: `*` matches everyone, so
/// "does this subject already hold it" has no one answer, and `token:` names a
/// principal whose groups this service cannot know.
///
/// Exact for `user:` and for both spellings of `group:`; approximate for
/// `role:`, where it uses the role itself.
///
/// # The group string has to carry a provider
///
/// `group:*:eng` matches an identity whose group string is `<provider>:eng` for
/// **any non-empty provider** — `group_matches` requires the prefix to be there
/// and non-empty, and answers `false` for a bare string. Synthesising a bare
/// `eng` therefore built an identity its own matcher did not match, so the
/// redundancy warning could never fire for a wildcard-provider subject: an
/// operator re-granting at the package tier a verb the registry tier already
/// gave `group:*:eng` was told nothing.
///
/// `authz_explain::identity_for` has the same problem and solved it the same
/// way, for the same reason. The two are deliberately *not* one function — they
/// disagree about `*` and `token:`, which each answers for its own endpoint —
/// but they agree here, and a reader changing one should look at the other.
fn synthetic_subject(matcher: &SubjectMatcher) -> Option<Subject> {
    use crate::entities::{GroupProvider, Role};

    /// Stands in for "some provider" when the subject names none. Never
    /// compared against a configured provider — a `group:*:` subject matches on
    /// the name and on the prefix merely being present.
    const SYNTHETIC_PROVIDER: &str = "grants-editor-any-provider";

    let identity = match matcher {
        SubjectMatcher::User(id) => Identity {
            user_id: Some(id.clone()),
            role: Role::User,
            auth_provider: None,
            groups: Vec::new(),
        },
        SubjectMatcher::Group { provider, name } => Identity {
            user_id: None,
            role: Role::User,
            auth_provider: match provider {
                GroupProvider::Named(p) => Some(p.clone()),
                _ => None,
            },
            groups: vec![match provider {
                GroupProvider::Named(p) => format!("{p}:{name}"),
                // Any provider will do, so name one that cannot collide with a
                // real one: the answer has to be about the wildcard, not about
                // whichever identity provider this deployment happens to run.
                GroupProvider::Any => format!("{SYNTHETIC_PROVIDER}:{name}"),
                // Unprefixed matches a bare group string, and only a bare one.
                // Unreachable from this service — `SubjectMatcher::parse`
                // requires `group:<provider>:<name>`, so the editor cannot write
                // one — but the arm is here because the enum has it and a
                // `_ =>` is how the wildcard case got the wrong string.
                GroupProvider::Unprefixed => name.clone(),
            }],
        },
        SubjectMatcher::Role(role) => Identity {
            user_id: None,
            role: role.clone(),
            auth_provider: None,
            groups: Vec::new(),
        },
        _ => return None,
    };
    Some(Subject::Identity(identity))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{GrantMap, Node, Role, Tier};
    use crate::ports::OwnerEntry;
    use crate::services::hot_config::{new_hot_lock, HotConfig};
    use std::collections::HashMap;
    use std::sync::Mutex;

    // ── doubles ──────────────────────────────────────────────────────────────

    #[derive(Default)]
    struct Rows(Mutex<Vec<StoredGrant>>);

    #[async_trait::async_trait]
    impl GrantRepository for Rows {
        async fn grants_for(
            &self,
            registry: &str,
            package: &str,
            version: Option<&str>,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            let want_version = version.map(|v| version_node_key(package, v));
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|g| {
                    g.registry == registry
                        && match g.node_kind {
                            NodeKind::Package => g.node_key == package,
                            NodeKind::Version => Some(&g.node_key) == want_version.as_ref(),
                        }
                })
                .cloned()
                .collect())
        }

        async fn put_grant(&self, grant: StoredGrant) -> Result<(), CoreError> {
            let mut rows = self.0.lock().unwrap();
            rows.retain(|g| {
                !(g.registry == grant.registry
                    && g.node_kind == grant.node_kind
                    && g.node_key == grant.node_key
                    && g.subject == grant.subject)
            });
            rows.push(grant);
            Ok(())
        }

        async fn delete_grant(
            &self,
            registry: &str,
            node_kind: NodeKind,
            node_key: &str,
            subject: &SubjectMatcher,
        ) -> Result<(), CoreError> {
            self.0.lock().unwrap().retain(|g| {
                !(g.registry == registry
                    && g.node_kind == node_kind
                    && g.node_key == node_key
                    && &g.subject == subject)
            });
            Ok(())
        }

        async fn package_grants_in_registry(
            &self,
            registry: &str,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|g| g.registry == registry && g.node_kind == NodeKind::Package)
                .cloned()
                .collect())
        }

        async fn grants_on_node(
            &self,
            registry: &str,
            node_kind: NodeKind,
            node_key: &str,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|g| {
                    g.registry == registry && g.node_kind == node_kind && g.node_key == node_key
                })
                .cloned()
                .collect())
        }

        async fn version_grants_in_registry(
            &self,
            registry: &str,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            Ok(self
                .0
                .lock()
                .unwrap()
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
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|g| {
                    g.registry == registry
                        && g.node_kind == NodeKind::Version
                        && g.node_key.starts_with(&prefix)
                })
                .cloned()
                .collect())
        }

        async fn delete_package_grants(
            &self,
            _registry: &str,
            _package: &str,
        ) -> Result<(), CoreError> {
            Ok(())
        }
    }

    struct Versions(Vec<(&'static str, bool)>);

    #[async_trait::async_trait]
    impl VersionLookup for Versions {
        async fn versions_of(
            &self,
            registry: &str,
            package: &str,
        ) -> Result<Vec<PublishedPackage>, CoreError> {
            Ok(self
                .0
                .iter()
                .map(|(v, yanked)| PublishedPackage {
                    registry: registry.to_owned(),
                    name: package.to_owned(),
                    version: (*v).to_owned(),
                    checksum: String::new(),
                    yanked: *yanked,
                    deprecated: false,
                    deprecation_message: None,
                    unlisted: false,
                    index_metadata: serde_json::Value::Null,
                    published_at: chrono::Utc::now(),
                    published_by: None,
                    signature_bytes: None,
                    signature_type: None,
                    visibility: crate::entities::Visibility::Public,
                    retention_keep: false,
                })
                .collect())
        }
    }

    struct Owners(Vec<OwnerEntry>);

    #[async_trait::async_trait]
    impl OwnershipPort for Owners {
        async fn initialize_owner(&self, _: &str, _: &str, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        async fn can_publish(&self, _: &str, _: &str, _: &Identity) -> Result<bool, CoreError> {
            Ok(true)
        }
        async fn add_owner(&self, _: &str, _: &str, _: OwnerEntry) -> Result<(), CoreError> {
            Ok(())
        }
        async fn remove_owner(&self, _: &str, _: &str, _: &str, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        async fn list_owners(&self, _: &str, _: &str) -> Result<Vec<OwnerEntry>, CoreError> {
            Ok(self
                .0
                .iter()
                .map(|o| OwnerEntry {
                    principal_type: o.principal_type.clone(),
                    principal_id: o.principal_id.clone(),
                    role: o.role.clone(),
                    granted_by: o.granted_by.clone(),
                })
                .collect())
        }
    }

    // ── fixtures ─────────────────────────────────────────────────────────────

    /// A registry whose own node grants `registry_grants` to `group:oidc1:eng`,
    /// which is what the redundancy warning is measured against.
    fn service(
        rows: Arc<Rows>,
        registry_grants: &[Action],
        versions: Option<Arc<dyn VersionLookup>>,
        ownership: Option<Arc<dyn OwnershipPort>>,
    ) -> GrantAdminService {
        let mut map = GrantMap::new();
        if !registry_grants.is_empty() {
            map = map.grant(
                SubjectMatcher::Group {
                    provider: crate::entities::GroupProvider::Named("oidc1".to_owned()),
                    name: "eng".to_owned(),
                },
                registry_grants.to_vec(),
            );
        }
        let grants = crate::entities::RegistryGrants {
            kind: RegistryKind::Npm,
            registry: Node::new(Tier::Registry, "registry:npm1", Some(map)),
            namespaces: Vec::new(),
        };
        let hot = new_hot_lock(HotConfig {
            grants: HashMap::from([("npm1".to_owned(), Arc::new(grants))]),
            grant_repo: Some(rows as Arc<dyn GrantRepository>),
            ..Default::default()
        });
        GrantAdminService::new(hot, versions, ownership)
    }

    fn admin() -> Identity {
        Identity {
            user_id: Some("root".to_owned()),
            role: Role::Admin,
            auth_provider: None,
            groups: Vec::new(),
        }
    }

    fn pkg() -> GrantTarget {
        GrantTarget::package("npm1", "@acme/billing")
    }

    // ── the node key is the storage key ──────────────────────────────────────

    #[test]
    fn a_version_target_keys_the_way_version_node_key_spells_it() {
        let t = GrantTarget::version("npm1", "@acme/billing", "2.4.0-rc.1");
        assert_eq!(t.node_kind(), NodeKind::Version);
        assert_eq!(t.node_key(), "@acme/billing@2.4.0-rc.1");
        assert_eq!(pkg().node_kind(), NodeKind::Package);
        assert_eq!(pkg().node_key(), "@acme/billing");
    }

    // ── §4.4 validation ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_grant_is_written_and_read_back() {
        let rows = Arc::new(Rows::default());
        let svc = service(rows.clone(), &[], None, None);
        let warnings = svc
            .set(
                &pkg(),
                "group:oidc1:eng",
                &["releases:read".to_owned()],
                &admin(),
            )
            .await
            .expect("written");
        assert!(warnings.is_empty());

        let listed = svc.list(&pkg()).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].actions, vec![Action::ReleasesRead]);
        assert_eq!(listed[0].granted_by.as_deref(), Some("root"));
    }

    /// §4.2 — expansion happens at write, so the stored row carries the set and
    /// no decision is deferred to evaluation.
    #[tokio::test]
    async fn a_wildcard_is_expanded_at_write() {
        let rows = Arc::new(Rows::default());
        let svc = service(rows.clone(), &[], None, None);
        svc.set(&pkg(), "user:alice", &["releases:*".to_owned()], &admin())
            .await
            .unwrap();

        let stored = &svc.list(&pkg()).await.unwrap()[0];
        assert!(stored.actions.contains(&Action::ReleasesRead));
        assert!(stored.actions.contains(&Action::ReleasesPublish));
        assert!(
            !stored.actions.contains(&Action::GatesExempt),
            "`releases:*` must not reach `gates:exempt` — that is why it is spelled \
             under its own prefix"
        );
    }

    #[tokio::test]
    async fn an_unknown_action_is_refused() {
        let svc = service(Arc::new(Rows::default()), &[], None, None);
        let err = svc
            .set(
                &pkg(),
                "user:alice",
                &["releases:teleport".to_owned()],
                &admin(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidInput(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn an_unparseable_subject_is_refused() {
        let svc = service(Arc::new(Rows::default()), &[], None, None);
        let err = svc
            .set(
                &pkg(),
                "not a subject",
                &["releases:read".to_owned()],
                &admin(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidInput(_)), "got {err:?}");
    }

    /// A seal is a config-file statement (§4.3) and has no representation here.
    #[tokio::test]
    async fn an_empty_action_set_is_refused_rather_than_written_as_a_seal() {
        let svc = service(Arc::new(Rows::default()), &[], None, None);
        let err = svc
            .set(&pkg(), "user:alice", &[], &admin())
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidInput(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn an_ecosystem_verb_from_another_ecosystem_is_refused() {
        let svc = service(Arc::new(Rows::default()), &[], None, None);
        // npm1 is an npm registry; the Terraform verb is not defined here.
        let err = svc
            .set(
                &pkg(),
                "user:alice",
                &["terraform:signing-keys:write".to_owned()],
                &admin(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidInput(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn an_unconfigured_registry_is_refused() {
        let svc = service(Arc::new(Rows::default()), &[], None, None);
        let err = svc
            .set(
                &GrantTarget::package("nope", "x"),
                "user:alice",
                &["releases:read".to_owned()],
                &admin(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::NotFound(_)), "got {err:?}");
    }

    // ── the version tier ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_version_tier_grant_needs_the_version_to_exist() {
        let versions = Arc::new(Versions(vec![("1.0.0", false)])) as Arc<dyn VersionLookup>;
        let svc = service(Arc::new(Rows::default()), &[], Some(versions), None);
        let err = svc
            .set(
                &GrantTarget::version("npm1", "@acme/billing", "9.9.9"),
                "user:alice",
                &["releases:read".to_owned()],
                &admin(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::InvalidInput(_)), "got {err:?}");
    }

    /// A yanked coordinate is real, still resolvable by exact pin, and an
    /// operator may well mean it — so it warns rather than refusing.
    #[tokio::test]
    async fn a_grant_on_a_yanked_version_warns_and_is_written() {
        let versions = Arc::new(Versions(vec![("1.0.0", true)])) as Arc<dyn VersionLookup>;
        let rows = Arc::new(Rows::default());
        let svc = service(rows, &[], Some(versions), None);
        let target = GrantTarget::version("npm1", "@acme/billing", "1.0.0");
        let warnings = svc
            .set(
                &target,
                "user:alice",
                &["releases:read".to_owned()],
                &admin(),
            )
            .await
            .unwrap();
        assert!(matches!(
            warnings.as_slice(),
            [GrantWarning::SpentCoordinate { .. }]
        ));
        assert_eq!(svc.list(&target).await.unwrap().len(), 1);
    }

    /// A deployment with no local backend cannot check, and "not checkable" is
    /// not "refused" — §11 open question 1's local-only scope stated as code.
    #[tokio::test]
    async fn without_a_local_backend_a_version_grant_is_written_unchecked() {
        let svc = service(Arc::new(Rows::default()), &[], None, None);
        svc.set(
            &GrantTarget::version("npm1", "@acme/billing", "9.9.9"),
            "user:alice",
            &["releases:read".to_owned()],
            &admin(),
        )
        .await
        .expect("written");
    }

    // ── §4.3 the ownership refusal ───────────────────────────────────────────

    fn owner_of_billing() -> Arc<dyn OwnershipPort> {
        Arc::new(Owners(vec![OwnerEntry {
            principal_type: "user".to_owned(),
            principal_id: "alice".to_owned(),
            role: "admin".to_owned(),
            granted_by: None,
        }]))
    }

    #[tokio::test]
    async fn a_write_that_would_drop_an_ownership_verb_is_refused() {
        let svc = service(
            Arc::new(Rows::default()),
            &[],
            None,
            Some(owner_of_billing()),
        );
        let err = svc
            .set(
                &pkg(),
                "user:alice",
                &["releases:read".to_owned()],
                &admin(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::Conflict(_)), "got {err:?}");
        let CoreError::Conflict(msg) = err else {
            unreachable!()
        };
        assert!(
            msg.contains("owners remove"),
            "the refusal has to name the way to do it: {msg}"
        );
    }

    /// Keeping the ownership verbs and adding one is not a conflict — it is
    /// exactly how an owner acquires an extra verb.
    #[tokio::test]
    async fn a_write_that_keeps_the_ownership_verbs_is_allowed() {
        let svc = service(
            Arc::new(Rows::default()),
            &[],
            None,
            Some(owner_of_billing()),
        );
        svc.set(
            &pkg(),
            "user:alice",
            &[
                "releases:publish".to_owned(),
                "owners:read".to_owned(),
                "owners:write".to_owned(),
                "releases:read".to_owned(),
            ],
            &admin(),
        )
        .await
        .expect("keeps all three, adds one");
    }

    #[tokio::test]
    async fn a_non_owner_is_unaffected_by_the_ownership_refusal() {
        let svc = service(
            Arc::new(Rows::default()),
            &[],
            None,
            Some(owner_of_billing()),
        );
        svc.set(&pkg(), "user:bob", &["releases:read".to_owned()], &admin())
            .await
            .expect("bob owns nothing");
    }

    /// Ownership is a property of a package name, so the projection can never
    /// have written a version row and a version-tier write can never collide.
    #[tokio::test]
    async fn the_ownership_refusal_does_not_reach_the_version_tier() {
        let versions = Arc::new(Versions(vec![("1.0.0", false)])) as Arc<dyn VersionLookup>;
        let svc = service(
            Arc::new(Rows::default()),
            &[],
            Some(versions),
            Some(owner_of_billing()),
        );
        svc.set(
            &GrantTarget::version("npm1", "@acme/billing", "1.0.0"),
            "user:alice",
            &["releases:read".to_owned()],
            &admin(),
        )
        .await
        .expect("no package-tier ownership row can be at stake");
    }

    #[tokio::test]
    async fn removing_an_ownership_projected_grant_is_refused() {
        let svc = service(
            Arc::new(Rows::default()),
            &[],
            None,
            Some(owner_of_billing()),
        );
        let err = svc.remove(&pkg(), "user:alice").await.unwrap_err();
        assert!(matches!(err, CoreError::Conflict(_)), "got {err:?}");
    }

    // ── §4.4 the redundancy warning ──────────────────────────────────────────

    /// A registry tier granting `group:*:eng`, so the wildcard spelling is the
    /// one under test on both sides.
    fn service_granting_any_provider(actions: &[Action]) -> GrantAdminService {
        let map = GrantMap::new().grant(
            SubjectMatcher::Group {
                provider: crate::entities::GroupProvider::Any,
                name: "eng".to_owned(),
            },
            actions.to_vec(),
        );
        let grants = crate::entities::RegistryGrants {
            kind: RegistryKind::Npm,
            registry: Node::new(Tier::Registry, "registry:npm1", Some(map)),
            namespaces: Vec::new(),
        };
        let hot = new_hot_lock(HotConfig {
            grants: HashMap::from([("npm1".to_owned(), Arc::new(grants))]),
            grant_repo: Some(Arc::new(Rows::default()) as Arc<dyn GrantRepository>),
            ..Default::default()
        });
        GrantAdminService::new(hot, None, None)
    }

    /// The wildcard-provider spelling gets its warning too.
    ///
    /// `group:*:eng` matches a group string `<provider>:eng` for any non-empty
    /// provider, and `synthetic_subject` was building a **bare** `eng` — an
    /// identity the subject's own matcher rejects. So this warning could never
    /// fire for the wildcard, and an operator re-granting a verb the registry
    /// tier already gave was told nothing. Fail-safe, and therefore silent,
    /// which is why it needed a test rather than a bug report.
    #[tokio::test]
    async fn a_wildcard_provider_group_is_reported_redundant() {
        let svc = service_granting_any_provider(&[Action::ReleasesRead]);
        let warnings = svc
            .set(
                &pkg(),
                "group:*:eng",
                &["releases:read".to_owned()],
                &admin(),
            )
            .await
            .unwrap();
        assert!(
            matches!(warnings.as_slice(), [GrantWarning::Redundant { .. }]),
            "got {warnings:?}"
        );
    }

    /// …and it is still the *name* that decides: a wildcard on another name is
    /// not covered by this one, or the warning would fire for everything.
    #[tokio::test]
    async fn a_wildcard_provider_group_on_another_name_is_not_redundant() {
        let svc = service_granting_any_provider(&[Action::ReleasesRead]);
        let warnings = svc
            .set(
                &pkg(),
                "group:*:security",
                &["releases:read".to_owned()],
                &admin(),
            )
            .await
            .unwrap();
        assert!(warnings.is_empty(), "got {warnings:?}");
    }

    #[tokio::test]
    async fn a_verb_the_registry_tier_already_grants_is_reported_redundant() {
        let svc = service(
            Arc::new(Rows::default()),
            &[Action::ReleasesRead],
            None,
            None,
        );
        let warnings = svc
            .set(
                &pkg(),
                "group:oidc1:eng",
                &["releases:read".to_owned()],
                &admin(),
            )
            .await
            .unwrap();
        assert!(
            matches!(warnings.as_slice(), [GrantWarning::Redundant { .. }]),
            "got {warnings:?}"
        );
    }

    /// Partly redundant is not redundant: one verb the broader tier does not
    /// grant is the whole reason the row is worth writing.
    #[tokio::test]
    async fn a_partly_new_grant_is_not_reported_redundant() {
        let svc = service(
            Arc::new(Rows::default()),
            &[Action::ReleasesRead],
            None,
            None,
        );
        let warnings = svc
            .set(
                &pkg(),
                "group:oidc1:eng",
                &["releases:read".to_owned(), "releases:list".to_owned()],
                &admin(),
            )
            .await
            .unwrap();
        assert!(warnings.is_empty(), "got {warnings:?}");
    }

    #[tokio::test]
    async fn a_different_subject_is_not_reported_redundant() {
        let svc = service(
            Arc::new(Rows::default()),
            &[Action::ReleasesRead],
            None,
            None,
        );
        let warnings = svc
            .set(
                &pkg(),
                "group:oidc1:qa",
                &["releases:read".to_owned()],
                &admin(),
            )
            .await
            .unwrap();
        assert!(warnings.is_empty(), "got {warnings:?}");
    }

    /// Re-writing an existing row must not report itself redundant: the
    /// question is what the subject holds *without* this row.
    #[tokio::test]
    async fn rewriting_the_same_row_is_not_reported_redundant() {
        let rows = Arc::new(Rows::default());
        let svc = service(rows, &[], None, None);
        for _ in 0..2 {
            let warnings = svc
                .set(
                    &pkg(),
                    "group:oidc1:eng",
                    &["releases:read".to_owned()],
                    &admin(),
                )
                .await
                .unwrap();
            assert!(warnings.is_empty(), "got {warnings:?}");
        }
        assert_eq!(
            svc.list(&pkg()).await.unwrap().len(),
            1,
            "a write replaces that subject's row rather than adding a second"
        );
    }

    // ── removal ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn removing_reports_whether_a_row_was_there() {
        let rows = Arc::new(Rows::default());
        let svc = service(rows, &[], None, None);
        assert!(
            !svc.remove(&pkg(), "user:alice").await.unwrap(),
            "nothing to remove"
        );
        svc.set(
            &pkg(),
            "user:alice",
            &["releases:read".to_owned()],
            &admin(),
        )
        .await
        .unwrap();
        assert!(svc.remove(&pkg(), "user:alice").await.unwrap());
        assert!(svc.list(&pkg()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_removal_leaves_other_subjects_alone() {
        let rows = Arc::new(Rows::default());
        let svc = service(rows, &[], None, None);
        svc.set(
            &pkg(),
            "user:alice",
            &["releases:read".to_owned()],
            &admin(),
        )
        .await
        .unwrap();
        svc.set(&pkg(), "user:bob", &["releases:read".to_owned()], &admin())
            .await
            .unwrap();
        svc.remove(&pkg(), "user:alice").await.unwrap();
        let left = svc.list(&pkg()).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].subject, SubjectMatcher::User("bob".to_owned()));
    }

    // ── both tiers, for the admin listing ────────────────────────────────────

    #[tokio::test]
    async fn listing_a_package_reports_both_tiers() {
        let versions =
            Arc::new(Versions(vec![("1.0.0", false), ("2.0.0", false)])) as Arc<dyn VersionLookup>;
        let rows = Arc::new(Rows::default());
        let svc = service(rows, &[], Some(versions), None);
        svc.set(
            &pkg(),
            "user:alice",
            &["releases:read".to_owned()],
            &admin(),
        )
        .await
        .unwrap();
        svc.set(
            &GrantTarget::version("npm1", "@acme/billing", "2.0.0"),
            "user:bob",
            &["releases:read".to_owned()],
            &admin(),
        )
        .await
        .unwrap();

        let all = svc.list_for_package("npm1", "@acme/billing").await.unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|g| g.node_kind == NodeKind::Package));
        assert!(all
            .iter()
            .any(|g| g.node_kind == NodeKind::Version && g.node_key == "@acme/billing@2.0.0"));
    }
}
