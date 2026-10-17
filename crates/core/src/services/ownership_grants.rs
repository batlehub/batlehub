//! Ownership, projected into package-tier grants.
//!
//! RFC 0015 §10 rule 9: *"Ownership rows migrate to package-level grants —
//! `releases:publish`, `owners:read` and `owners:write` on the one package,
//! which is the scope `OwnershipPort` already has."* Migration 042 moved the
//! rows that existed; this keeps them in step from then on.
//!
//! # Why a decorator and not a call in each handler
//!
//! Ownership changes through five doors: a first publish, the two admin
//! governance routes, the two `cargo owner` routes, and the name release when a
//! package's last version is deleted. The first of those was written through and
//! the other four were not, so `package_owners` and `grants` diverged from the
//! first owner change on any estate — a removed owner kept a package-tier
//! `releases:publish` and `owners:write` grant permanently, and `explain`
//! reported it as live.
//!
//! Adding the projection to each door would be four call sites and a fifth that
//! a later contributor forgets, which is *"authorization by convention rather
//! than by construction"* — the sentence §2 opens with. Wrapping the port
//! instead means every caller gets it because there is no other port to call:
//! handlers reach ownership through `LocalRegistryService::ownership`, and what
//! is behind that handle is this.
//!
//! [`OwnershipGrants::wrap`] is therefore the one constructor, and both
//! `server/src/main.rs` and the test fixtures call it. §13.5 records why that
//! matters more than it sounds: when `RbacRule` came out of the chain, a fixture
//! that kept building the rule while production resolved grants went on passing
//! while testing a path nobody ran.
//!
//! # What it is not
//!
//! Not the enforcement path. `can_publish` still decides who may publish to an
//! existing package, for the reason §13.5 gives at length: ownership **narrows**
//! and §4.3's union only widens, so reading these rows as the authority would let
//! any user publish over any other user's package. These grants are the *read*
//! model — what the owners API is a view over, and what `explain` reports.

use std::sync::Arc;

use async_trait::async_trait;

use crate::entities::{Action, SubjectMatcher};
use crate::error::CoreError;
use crate::ports::{GrantRepository, NodeKind, OwnerEntry, OwnershipPort, StoredGrant};

/// The verbs an ownership row projects to.
///
/// §10 rule 9's three and no more: *"Registry-wide `owners:write` is rule 5's
/// admin grant and nothing else; a publisher does not acquire it by
/// publishing."* Kept as one constant because migration 042 writes the same
/// three in SQL and the two must not drift — a fourth verb added here and not
/// there would appear only on estates that changed an owner after upgrading.
pub const OWNERSHIP_ACTIONS: &[Action] = &[
    Action::ReleasesPublish,
    Action::OwnersRead,
    Action::OwnersWrite,
];

/// The grant subject an owner row maps to.
///
/// **The mapping preserves shape rather than normalising it**, and migration 042
/// does the same thing in SQL for the same reason. §13.5: a bare `eng` and a
/// prefixed `oidc1:eng` are different groups today, and reading a bare one as
/// `group:*:<name>` would make it start matching `oidc1:eng` on every deployment
/// on upgrade — the silent widening §7 calls the migration's central risk.
///
/// An unrecognised `principal_type` answers `None` rather than guessing. The
/// column is `'user'` or `'group'` and nothing else writes it, so this arm is
/// unreachable through the API; guessing would invent a subject that matches
/// somebody.
pub fn subject_for_owner(principal_type: &str, principal_id: &str) -> Option<SubjectMatcher> {
    match principal_type {
        "user" => Some(SubjectMatcher::User(principal_id.to_owned())),
        "group" => Some(match principal_id.split_once(':') {
            Some((provider, name)) => SubjectMatcher::Group {
                provider: crate::entities::GroupProvider::Named(provider.to_owned()),
                name: name.to_owned(),
            },
            None => SubjectMatcher::Group {
                provider: crate::entities::GroupProvider::Unprefixed,
                name: principal_id.to_owned(),
            },
        }),
        _ => None,
    }
}

/// An [`OwnershipPort`] that keeps the package-tier `grants` rows in step.
pub struct OwnershipGrants {
    inner: Arc<dyn OwnershipPort>,
    grants: Arc<dyn GrantRepository>,
}

impl OwnershipGrants {
    /// Wrap `inner` so every ownership mutation projects into `grants`.
    pub fn wrap(
        inner: Arc<dyn OwnershipPort>,
        grants: Arc<dyn GrantRepository>,
    ) -> Arc<dyn OwnershipPort> {
        Arc::new(OwnershipGrants { inner, grants })
    }

    /// The actions currently written for `subject` on this package's node.
    ///
    /// Read before every write, because the row is **shared**: the schema is
    /// `UNIQUE (registry, node_kind, node_key, subject)`, so an operator who
    /// wrote a package-tier grant for the same subject through the admin API
    /// occupies the same row this projection does. There is no `source` column
    /// to tell the two apart, so the only safe arithmetic is union on the way in
    /// and subtraction on the way out.
    async fn existing(
        &self,
        registry: &str,
        package: &str,
        subject: &SubjectMatcher,
    ) -> Result<Vec<Action>, CoreError> {
        Ok(self
            .grants
            .grants_on_node(registry, NodeKind::Package, package)
            .await?
            .into_iter()
            .find(|g| &g.subject == subject)
            .map(|g| g.actions)
            .unwrap_or_default())
    }

    /// Union the ownership verbs into `subject`'s row.
    async fn project_add(
        &self,
        registry: &str,
        package: &str,
        subject: SubjectMatcher,
        granted_by: Option<String>,
    ) -> Result<(), CoreError> {
        let mut actions = self.existing(registry, package, &subject).await?;
        for action in OWNERSHIP_ACTIONS {
            if !actions.contains(action) {
                actions.push(*action);
            }
        }
        self.grants
            .put_grant(StoredGrant {
                registry: registry.to_owned(),
                node_kind: NodeKind::Package,
                node_key: package.to_owned(),
                subject,
                actions,
                granted_by,
            })
            .await
    }

    /// Subtract the ownership verbs from `subject`'s row, deleting it if nothing
    /// is left.
    ///
    /// Deleting rather than writing an empty set is not a choice: an empty action
    /// set is what a **seal** is, `ck_grants_actions_non_empty` refuses one, and
    /// §4.3 confines sealing to the config file. A row with no verbs is
    /// unrepresentable by construction, which is the property §7 asks for.
    async fn project_remove(
        &self,
        registry: &str,
        package: &str,
        subject: SubjectMatcher,
    ) -> Result<(), CoreError> {
        let remaining: Vec<Action> = self
            .existing(registry, package, &subject)
            .await?
            .into_iter()
            .filter(|a| !OWNERSHIP_ACTIONS.contains(a))
            .collect();

        if remaining.is_empty() {
            return self
                .grants
                .delete_grant(registry, NodeKind::Package, package, &subject)
                .await;
        }
        // Something else wrote verbs for this subject on this package. Losing an
        // owner is not a reason to lose those.
        self.grants
            .put_grant(StoredGrant {
                registry: registry.to_owned(),
                node_kind: NodeKind::Package,
                node_key: package.to_owned(),
                subject,
                actions: remaining,
                granted_by: None,
            })
            .await
    }

    /// A projection failure is logged and swallowed, and the ownership mutation
    /// stands.
    ///
    /// There is no transaction across two ports, so one of the two writes has to
    /// go first and the other has to be able to fail after it. Ownership goes
    /// first because it is still what **enforces** (`can_publish`); the grant is
    /// the read model. So a failure here leaves a stale diagnostic, where the
    /// other order would leave a stale *decision* — and `list_owners` remains the
    /// answer to "who owns this", which is what the API and the console read.
    ///
    /// Same reading `register_initial_owner` already takes of the same trade: a
    /// publish that stored its bytes and its row has succeeded, and failing it
    /// afterwards because a governance write did not land would lose the artifact
    /// to report a bookkeeping error.
    fn warn(context: &str, registry: &str, package: &str, err: &CoreError) {
        tracing::warn!(
            registry, package, error = %err,
            "{context}: the ownership row was written but its package-tier grant was not, \
             so `explain` and the owners view will disagree until the next change to this \
             package's owners"
        );
    }
}

#[async_trait]
impl OwnershipPort for OwnershipGrants {
    async fn claim_if_unowned(
        &self,
        registry: &str,
        package: &str,
        user_id: &str,
    ) -> Result<bool, CoreError> {
        let claimed = self
            .inner
            .claim_if_unowned(registry, package, user_id)
            .await?;
        if claimed {
            if let Err(e) = self
                .project_add(
                    registry,
                    package,
                    SubjectMatcher::User(user_id.to_owned()),
                    Some(user_id.to_owned()),
                )
                .await
            {
                Self::warn("claim_if_unowned", registry, package, &e);
            }
        }
        Ok(claimed)
    }

    async fn initialize_owner(
        &self,
        registry: &str,
        package: &str,
        user_id: &str,
    ) -> Result<(), CoreError> {
        self.inner
            .initialize_owner(registry, package, user_id)
            .await?;
        if let Err(e) = self
            .project_add(
                registry,
                package,
                SubjectMatcher::User(user_id.to_owned()),
                Some(user_id.to_owned()),
            )
            .await
        {
            Self::warn("initialize_owner", registry, package, &e);
        }
        Ok(())
    }

    async fn can_publish(
        &self,
        registry: &str,
        package: &str,
        identity: &crate::entities::Identity,
    ) -> Result<bool, CoreError> {
        self.inner.can_publish(registry, package, identity).await
    }

    async fn add_owner(
        &self,
        registry: &str,
        package: &str,
        entry: OwnerEntry,
    ) -> Result<(), CoreError> {
        let subject = subject_for_owner(&entry.principal_type, &entry.principal_id);
        let granted_by = entry.granted_by.clone();
        // The inner store decides whether this is a duplicate — it answers
        // `Conflict` — so the projection runs only on a mutation that happened.
        self.inner.add_owner(registry, package, entry).await?;

        if let Some(subject) = subject {
            if let Err(e) = self
                .project_add(registry, package, subject, granted_by)
                .await
            {
                Self::warn("add_owner", registry, package, &e);
            }
        }
        Ok(())
    }

    async fn remove_owner(
        &self,
        registry: &str,
        package: &str,
        principal_type: &str,
        principal_id: &str,
    ) -> Result<(), CoreError> {
        self.inner
            .remove_owner(registry, package, principal_type, principal_id)
            .await?;

        if let Some(subject) = subject_for_owner(principal_type, principal_id) {
            if let Err(e) = self.project_remove(registry, package, subject).await {
                Self::warn("remove_owner", registry, package, &e);
            }
        }
        Ok(())
    }

    async fn list_owners(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<OwnerEntry>, CoreError> {
        self.inner.list_owners(registry, package).await
    }

    /// The name release (RFC 0016 §4.4), which the default implementation would
    /// get right by looping [`remove_owner`] — but `PgOwnershipStore` overrides
    /// it with one statement, so the override has to be overridden in turn.
    ///
    /// Listing first and then delegating means the projection sees the same rows
    /// the inner store is about to drop, whichever way it drops them.
    async fn remove_all_owners(&self, registry: &str, package: &str) -> Result<(), CoreError> {
        let owners = self.inner.list_owners(registry, package).await?;
        self.inner.remove_all_owners(registry, package).await?;

        for entry in owners {
            let Some(subject) = subject_for_owner(&entry.principal_type, &entry.principal_id)
            else {
                continue;
            };
            if let Err(e) = self.project_remove(registry, package, subject).await {
                Self::warn("remove_all_owners", registry, package, &e);
            }
        }
        Ok(())
    }

    async fn list_owned_by(
        &self,
        identity: &crate::entities::Identity,
    ) -> Result<Vec<(String, String)>, CoreError> {
        self.inner.list_owned_by(identity).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::GroupProvider;
    use std::collections::HashMap;
    use tokio::sync::RwLock;

    /// A minimal `OwnershipPort`, so these tests are about the projection rather
    /// than about an adapter.
    #[derive(Default)]
    struct MemOwners(RwLock<HashMap<(String, String), Vec<OwnerEntry>>>);

    #[async_trait]
    impl OwnershipPort for MemOwners {
        async fn initialize_owner(&self, r: &str, p: &str, u: &str) -> Result<(), CoreError> {
            self.add_owner(
                r,
                p,
                OwnerEntry {
                    principal_type: "user".to_owned(),
                    principal_id: u.to_owned(),
                    role: "admin".to_owned(),
                    granted_by: Some(u.to_owned()),
                },
            )
            .await
        }
        async fn can_publish(
            &self,
            _: &str,
            _: &str,
            _: &crate::entities::Identity,
        ) -> Result<bool, CoreError> {
            Ok(true)
        }
        async fn add_owner(&self, r: &str, p: &str, e: OwnerEntry) -> Result<(), CoreError> {
            self.0
                .write()
                .await
                .entry((r.to_owned(), p.to_owned()))
                .or_default()
                .push(e);
            Ok(())
        }
        async fn remove_owner(
            &self,
            r: &str,
            p: &str,
            ty: &str,
            id: &str,
        ) -> Result<(), CoreError> {
            if let Some(v) = self.0.write().await.get_mut(&(r.to_owned(), p.to_owned())) {
                v.retain(|e| !(e.principal_type == ty && e.principal_id == id));
            }
            Ok(())
        }
        async fn list_owners(&self, r: &str, p: &str) -> Result<Vec<OwnerEntry>, CoreError> {
            Ok(self
                .0
                .read()
                .await
                .get(&(r.to_owned(), p.to_owned()))
                .cloned()
                .unwrap_or_default())
        }
    }

    /// A `GrantRepository` with the one property these tests turn on: the schema's
    /// `UNIQUE (registry, node_kind, node_key, subject)`, so `put_grant` upserts
    /// a subject's row rather than appending a second one. A double that appended
    /// would make the union tests pass for the wrong reason.
    #[derive(Default)]
    struct MemGrants(RwLock<Vec<StoredGrant>>);

    #[async_trait]
    impl GrantRepository for MemGrants {
        async fn grants_for(
            &self,
            _: &str,
            _: &str,
            _: Option<&str>,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            Ok(Vec::new())
        }
        async fn put_grant(&self, grant: StoredGrant) -> Result<(), CoreError> {
            if grant.actions.is_empty() {
                return Err(CoreError::InvalidInput(
                    "a seal is not writable here".into(),
                ));
            }
            let mut rows = self.0.write().await;
            match rows.iter_mut().find(|g| {
                g.registry == grant.registry
                    && g.node_kind == grant.node_kind
                    && g.node_key == grant.node_key
                    && g.subject == grant.subject
            }) {
                Some(existing) => *existing = grant,
                None => rows.push(grant),
            }
            Ok(())
        }
        async fn delete_grant(
            &self,
            registry: &str,
            node_kind: NodeKind,
            node_key: &str,
            subject: &SubjectMatcher,
        ) -> Result<(), CoreError> {
            self.0.write().await.retain(|g| {
                !(g.registry == registry
                    && g.node_kind == node_kind
                    && g.node_key == node_key
                    && &g.subject == subject)
            });
            Ok(())
        }
        async fn package_grants_in_registry(&self, _: &str) -> Result<Vec<StoredGrant>, CoreError> {
            Ok(Vec::new())
        }
        async fn version_grants_in_registry(&self, _: &str) -> Result<Vec<StoredGrant>, CoreError> {
            Ok(Vec::new())
        }
        async fn version_grants_for_package(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            // The projection writes package rows only, so this double never
            // needs to answer with one — which is the property this file's own
            // tests are about.
            Ok(Vec::new())
        }
        async fn grants_on_node(
            &self,
            registry: &str,
            node_kind: NodeKind,
            node_key: &str,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            Ok(self
                .0
                .read()
                .await
                .iter()
                .filter(|g| {
                    g.registry == registry && g.node_kind == node_kind && g.node_key == node_key
                })
                .cloned()
                .collect())
        }
        async fn delete_package_grants(&self, _: &str, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
    }

    fn wrapped() -> (Arc<dyn OwnershipPort>, Arc<MemGrants>) {
        let grants = Arc::new(MemGrants::default());
        let port = OwnershipGrants::wrap(Arc::new(MemOwners::default()), grants.clone());
        (port, grants)
    }

    async fn actions_for(grants: &MemGrants, subject: &SubjectMatcher) -> Option<Vec<Action>> {
        GrantRepository::grants_on_node(grants, "reg", NodeKind::Package, "pkg")
            .await
            .unwrap()
            .into_iter()
            .find(|g| &g.subject == subject)
            .map(|g| g.actions)
    }

    fn user(id: &str) -> SubjectMatcher {
        SubjectMatcher::User(id.to_owned())
    }

    /// The bug: `cargo owner --add` wrote `package_owners` and nothing else.
    #[tokio::test]
    async fn adding_an_owner_writes_the_package_tier_grant() {
        let (port, grants) = wrapped();
        port.add_owner(
            "reg",
            "pkg",
            OwnerEntry {
                principal_type: "user".to_owned(),
                principal_id: "alice".to_owned(),
                role: "maintainer".to_owned(),
                granted_by: Some("root".to_owned()),
            },
        )
        .await
        .unwrap();

        let actions = actions_for(&grants, &user("alice")).await.expect("a row");
        for verb in OWNERSHIP_ACTIONS {
            assert!(actions.contains(verb), "{verb} must be projected");
        }
    }

    /// …and `cargo owner --remove` took it back.
    ///
    /// Before this, a removed owner kept `releases:publish` and `owners:write` on
    /// the package permanently, and `explain` reported it as live.
    #[tokio::test]
    async fn removing_an_owner_takes_the_grant_back() {
        let (port, grants) = wrapped();
        port.initialize_owner("reg", "pkg", "alice").await.unwrap();
        assert!(actions_for(&grants, &user("alice")).await.is_some());

        port.remove_owner("reg", "pkg", "user", "alice")
            .await
            .unwrap();
        assert!(
            actions_for(&grants, &user("alice")).await.is_none(),
            "a row with no verbs is a seal, so removal deletes rather than empties it"
        );
    }

    /// A hand-written grant for the same subject survives both directions.
    ///
    /// The row is shared — `UNIQUE (registry, node_kind, node_key, subject)` — and
    /// there is no column saying which writer put a verb there. So the projection
    /// unions on the way in and subtracts only its own three on the way out; a
    /// wholesale `put_grant` would clobber the operator's verbs on add, and a
    /// wholesale `delete_grant` would destroy them on remove.
    #[tokio::test]
    async fn a_hand_written_grant_for_the_same_subject_survives() {
        let (port, grants) = wrapped();
        GrantRepository::put_grant(
            &*grants,
            StoredGrant {
                registry: "reg".to_owned(),
                node_kind: NodeKind::Package,
                node_key: "pkg".to_owned(),
                subject: user("alice"),
                actions: vec![Action::ReleasesRead],
                granted_by: Some("operator".to_owned()),
            },
        )
        .await
        .unwrap();

        port.initialize_owner("reg", "pkg", "alice").await.unwrap();
        let actions = actions_for(&grants, &user("alice")).await.expect("a row");
        assert!(
            actions.contains(&Action::ReleasesRead),
            "the operator's verb must survive the projection: {actions:?}"
        );
        assert!(actions.contains(&Action::ReleasesPublish));

        port.remove_owner("reg", "pkg", "user", "alice")
            .await
            .unwrap();
        let actions = actions_for(&grants, &user("alice"))
            .await
            .expect("the row must survive, carrying what the projection did not write");
        assert_eq!(actions, vec![Action::ReleasesRead]);
    }

    /// A group principal keeps its shape, exactly as migration 042 writes it.
    ///
    /// §13.5's sharpest migration edge: a bare `eng` and a prefixed `oidc1:eng`
    /// are different groups today, so normalising either into `group:*:eng` would
    /// widen every deployment that uses the bare form.
    #[tokio::test]
    async fn a_group_owner_keeps_its_provider_shape() {
        assert_eq!(
            subject_for_owner("group", "oidc1:eng"),
            Some(SubjectMatcher::Group {
                provider: GroupProvider::Named("oidc1".to_owned()),
                name: "eng".to_owned()
            })
        );
        assert_eq!(
            subject_for_owner("group", "eng"),
            Some(SubjectMatcher::Group {
                provider: GroupProvider::Unprefixed,
                name: "eng".to_owned()
            }),
            "a bare key must not become the any-provider wildcard"
        );
        assert_eq!(subject_for_owner("user", "alice"), Some(user("alice")));
        assert_eq!(subject_for_owner("wat", "x"), None);

        // …and the projection round-trips one.
        let (port, grants) = wrapped();
        port.add_owner(
            "reg",
            "pkg",
            OwnerEntry {
                principal_type: "group".to_owned(),
                principal_id: "oidc1:eng".to_owned(),
                role: "maintainer".to_owned(),
                granted_by: None,
            },
        )
        .await
        .unwrap();
        let subject = subject_for_owner("group", "oidc1:eng").unwrap();
        assert!(actions_for(&grants, &subject).await.is_some());
    }

    /// Releasing the name drops every owner's grant (RFC 0016 §4.4).
    ///
    /// `PgOwnershipStore` overrides `remove_all_owners` with one statement, so
    /// the default's loop through `remove_owner` would not run and the
    /// projection has to override it in turn. §4.3's reasoning is why it
    /// matters: grants keyed by a name that outlive the package leave a previous
    /// owner holding `releases:publish` on a name someone else may take.
    #[tokio::test]
    async fn releasing_the_name_drops_every_owners_grant() {
        let (port, grants) = wrapped();
        port.initialize_owner("reg", "pkg", "alice").await.unwrap();
        port.add_owner(
            "reg",
            "pkg",
            OwnerEntry {
                principal_type: "group".to_owned(),
                principal_id: "eng".to_owned(),
                role: "maintainer".to_owned(),
                granted_by: None,
            },
        )
        .await
        .unwrap();

        port.remove_all_owners("reg", "pkg").await.unwrap();
        assert!(actions_for(&grants, &user("alice")).await.is_none());
        assert!(
            actions_for(&grants, &subject_for_owner("group", "eng").unwrap())
                .await
                .is_none()
        );
    }

    /// A duplicate add does not double the verbs, and a remove of somebody who
    /// was never an owner does not touch anybody else's row.
    #[tokio::test]
    async fn the_projection_is_idempotent_in_both_directions() {
        let (port, grants) = wrapped();
        port.initialize_owner("reg", "pkg", "alice").await.unwrap();
        port.initialize_owner("reg", "pkg", "alice").await.unwrap();
        assert_eq!(
            actions_for(&grants, &user("alice")).await.unwrap().len(),
            OWNERSHIP_ACTIONS.len()
        );

        port.remove_owner("reg", "pkg", "user", "bob")
            .await
            .unwrap();
        assert!(
            actions_for(&grants, &user("alice")).await.is_some(),
            "removing a non-owner must not disturb an owner"
        );
    }
}
