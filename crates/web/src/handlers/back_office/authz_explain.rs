//! `GET /api/v1/admin/authz/explain` — why the resolver decided that.
//!
//! RFC 0015 §4.8. Every mechanism this RFC removes had its own way of being
//! invisible: ownership lived in a table nobody rendered, `bypass_roles` was a
//! field inside a rule, the beta channel was a config block, and the only way to
//! answer "why was this refused?" was to read Rust. A single model deserves a
//! single place to watch it, and without one an operator's first encounter with
//! a denial is still a support ticket.
//!
//! §13 puts this in **phase 3**, not phase 5, and says why: *"the first thing
//! anyone asks of a grant resolver is why it did that, and a migration (§10)
//! reviewed without it is reviewed by reading code."*
//!
//! # `granted_by` is the point
//!
//! A resolved set without provenance tells an operator *what* they have. Naming
//! the tier **and the subject form** that produced each verb tells them which
//! line to edit — which is the difference between a debugging tool and a
//! diagnostic.
//!
//! # This resolves; it does not perform
//!
//! Nothing is fetched, nothing is written, no audit row is recorded for the
//! resource named. That makes it a second implementation of the thing it
//! describes, and §11.6 is explicit about the risk: *"a diagnostic that can
//! disagree with reality is worse than none, because it is trusted."* So it is
//! tested as an **oracle** rather than on its own — `authz_explain_oracle.rs`
//! asserts its verdict against the verdict the real request received, for every
//! row of the authorization matrix.
//!
//! # Why the query is not a single `resource=`
//!
//! §4.8 writes `?subject=…&action=…&resource=…`. The first two are taken
//! literally; the third is split into `registry`, `package` and `version`,
//! because a package name contains the separator a single string would have to
//! be split on — `@acme/billing/cards` for npm, `example.com/team/lib` for Go.
//! `authz_matrix.rs` records the same hazard one layer down, where a
//! hand-written route matcher got path parameters wrong in both directions
//! because "a path parameter is not always a single segment".

use std::sync::Arc;

use actix_web::{get, web, Responder};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use batlehub_core::entities::{
    resolve, Action, GroupProvider, Identity, Role, Subject, SubjectMatcher,
};
use batlehub_core::services::ProxyService;

use crate::{error::AppError, extractors::AuthIdentity};

#[derive(Debug, Deserialize, IntoParams)]
pub struct ExplainQuery {
    /// The registry to resolve against.
    pub registry: String,
    /// The subject to resolve *as*, in grant spelling: `*`, `role:user`,
    /// `group:oidc1:eng`, `group:*:eng`, `user:alice`.
    ///
    /// A real caller matches several subject forms at once — a user is also a
    /// role and several groups — so this answers about **one** form. For a
    /// whole identity, `POST /api/v1/admin/access-check` takes `user_id`,
    /// `role` and `groups` together and runs the block layers too.
    pub subject: String,
    /// The permission to ask about, e.g. `releases:read`.
    pub action: String,
    /// The package, when asking about one. Omit for a registry-tier question.
    pub package: Option<String>,
    /// The version, when asking about one.
    pub version: Option<String>,
}

/// One verb, and where it came from.
#[derive(Debug, Serialize, ToSchema)]
pub struct ResolvedVerb {
    pub action: String,
    /// The node that granted it — `registry:npm1`, `namespace:@acme/billing`.
    pub granted_by: String,
    /// The subject form that matched.
    pub subject: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ExplainResponse {
    /// `"allow"` or `"deny"`.
    pub decision: String,
    /// Why, when the answer is deny.
    pub reason: Option<String>,
    /// Every verb this subject holds on this resource, with provenance.
    pub resolved: Vec<ResolvedVerb>,
    /// The nodes walked, outermost first.
    ///
    /// Includes the ones that granted nothing. A tier missing from this list is
    /// a tier the resolver did not consider, which is a different diagnosis from
    /// a tier that considered the subject and matched no grant.
    pub tiers_walked: Vec<String>,
    /// What this answer does **not** account for.
    ///
    /// The same discipline `access-check`'s `covers` field carries, and for the
    /// same reason (RFC 0004-bis B4): a bare verdict is ambiguous between
    /// "nothing denies this" and "nothing I looked at denies this". Grants are
    /// the first gate, not the only one — the artifact gates, per-package
    /// visibility and the block layers all sit behind them.
    pub not_covered: Vec<String>,
    /// The resource attributes that apply here, composed across the tiers
    /// (§4.1, §4.8).
    ///
    /// §4.8 puts these on the response beside the resolved verbs, and the pair
    /// is the point: **a caller needs both** — a grant for the verb and
    /// membership of the audience (§4.5). A resolved set that showed
    /// `releases:read` without saying the package is `team`-visible answers half
    /// the question and reads as the whole one.
    pub attributes: Attributes,
    /// Set when a node on this path is in **shadow** (§4.7), meaning a `deny`
    /// here is *not* what the request would receive — it would be served, and
    /// the refusal only recorded.
    ///
    /// Reported rather than folded into `decision`, and the distinction is the
    /// one this endpoint exists to make. Answering `allow` would hide that the
    /// grants refuse; answering a bare `deny` would contradict what the server
    /// actually does, and §11.6 is explicit that *"a diagnostic that can
    /// disagree with reality is worse than none, because it is trusted."* So it
    /// says both: the grants refuse, and the shadow serves it anyway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadowed_by: Option<ShadowNote>,
    /// §4.1's compensating warnings: deeper tiers on this path that **dropped**
    /// a constraint their parent declared.
    ///
    /// Composition is wholesale, which is what makes a narrower policy on a
    /// deeper tier expressible — and what makes dropping one silent. A namespace
    /// that omits `enforce_semver` does not inherit it; it turns it off, and
    /// nothing about the resolved answer says so. This is the edit most likely
    /// to be a mistake in the direction that matters, so the endpoint whose job
    /// is *why* is where it surfaces.
    ///
    /// Empty for almost every coordinate.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub narrowing: Vec<NarrowingNote>,
}

/// One tier that dropped a constraint its parent declared (§4.1).
#[derive(Debug, Serialize, ToSchema)]
pub struct NarrowingNote {
    /// The node that dropped it, e.g. `namespace:@acme/billing`.
    pub node: String,
    /// What it dropped, in the words §4.1 uses for it.
    pub dropped: String,
}

/// A node whose shadow is currently serving what its grants refuse.
#[derive(Debug, Serialize, ToSchema)]
pub struct ShadowNote {
    pub node: String,
    pub until: chrono::NaiveDate,
}

/// The resource attributes §4.8 shows beside the resolved verbs.
///
/// Every field is what the *tiers* composed to, not what a particular version
/// row holds: a per-package `visibility` override lives in the `policy` table
/// and is composed in here, while the legacy per-package visibility on the
/// package row is one of the layers `not_covered` names.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct Attributes {
    /// How wide the audience is (§4.5) — the model's one narrowing dimension.
    pub visibility: String,
    /// The audience for a pre-release, which follows `visibility` when no tier
    /// declared its own.
    pub prerelease_visibility: String,
    /// Whether these bytes may be replaced (§4.5).
    pub immutable: String,
    /// Whether a new version must sort above the newest existing one.
    pub monotonic: bool,
    /// Whether the versioning policy here evaluates and does not enforce (§4.7).
    pub versioning_dry_run: bool,
    /// The gates a live exemption is silencing on this coordinate (§4.5).
    ///
    /// Empty for almost every coordinate. Present here because an `allow` on a
    /// version whose `cve_gate` is exempt is a materially different answer from
    /// an `allow` on one that passed it.
    pub exempt_gates: Vec<String>,
}

/// The layers this endpoint deliberately does not evaluate.
///
/// Listed rather than summarised, because an operator reading an `allow` needs
/// to know which questions it answered. Every entry here is a real gate a
/// request would still meet.
const NOT_COVERED: &[&str] = &[
    "per-package visibility (public/internal/team)",
    "the pre-release beta channel",
    "the artifact gates: block_list, cve_gate, license_gate, release_age, \
     require_signed_release, trusted_publisher, version_gate",
    "the IP and account block layers",
];

/// Synthesise the smallest identity a subject form matches.
///
/// The resolver matches an `Identity` against `SubjectMatcher`s, so a question
/// phrased as a subject has to be turned back into a caller. Smallest on
/// purpose: `role:user` produces a user with no groups and no id, so the answer
/// is about *that form alone* rather than about a caller who happens to satisfy
/// several.
fn identity_for(matcher: &SubjectMatcher) -> Identity {
    let mut identity = Identity::anonymous();
    match matcher {
        SubjectMatcher::Anyone => {}
        SubjectMatcher::Role(role) => identity.role = role.clone(),
        SubjectMatcher::Group { provider, name } => {
            identity.groups = vec![match provider {
                // `group:*:eng` requires *a* provider; any will do, and naming
                // one that cannot collide keeps the answer about the wildcard.
                GroupProvider::Any => format!("explain-provider:{name}"),
                GroupProvider::Named(p) => format!("{p}:{name}"),
                GroupProvider::Unprefixed => name.clone(),
            }];
        }
        SubjectMatcher::User(principal) => {
            // `<provider>:<id>`, as `Identity::user_principal` spells it.
            match principal.split_once(':') {
                Some((p, id)) => {
                    identity.auth_provider = Some(p.to_owned());
                    identity.user_id = Some(id.to_owned());
                }
                None => identity.user_id = Some(principal.clone()),
            }
            // A `user:` grant is written for someone who has logged in; an
            // anonymous identity with a user id is not a state the auth
            // providers produce.
            identity.role = Role::User;
        }
        // No principal is a machine token yet (§4.3), so nothing this endpoint
        // can synthesise matches one. Answering about an anonymous caller would
        // be answering a different question, so the handler refuses instead.
        SubjectMatcher::Token(_) => {}
    }
    identity
}

/// Resolve a subject's permissions on a resource, without performing anything.
#[utoipa::path(
    get,
    path = "/api/v1/admin/authz/explain",
    tag = "back-office",
    params(ExplainQuery),
    responses(
        (status = 200, description = "The resolved permission set, with provenance", body = ExplainResponse),
        (status = 400, description = "Unparseable subject or action"),
        (status = 403, description = "`authz:read` required"),
        (status = 404, description = "Registry not configured"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/authz/explain")]
pub async fn admin_authz_explain(
    identity: AuthIdentity,
    proxy_svc: web::Data<Arc<ProxyService>>,
    query: web::Query<ExplainQuery>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::AuthzRead,
        None,
        &hot,
    )
    .await?;

    let mut matcher = SubjectMatcher::parse(&query.subject)
        .map_err(|e| AppError::bad_request(format!("subject: {e}")))?;
    if let SubjectMatcher::User(principal) = &matcher {
        let providers = hot.read().await.user_providers.clone();
        matcher = SubjectMatcher::User(
            batlehub_core::entities::scope_user_principal(principal, &providers)
                .map_err(|e| AppError::bad_request(format!("subject: {e}")))?,
        );
    }
    if matches!(matcher, SubjectMatcher::Token(_)) {
        return Err(AppError::bad_request(
            "no principal is a machine token yet (RFC 0015 §4.3), so a 'token:' subject \
             matches nobody and this endpoint cannot answer about one without inventing \
             a caller",
        ));
    }
    let action: Action =
        query
            .action
            .parse()
            .map_err(|e: batlehub_core::entities::ActionParseError| {
                AppError::bad_request(format!("action: {e}"))
            })?;

    let grants = {
        let hot = proxy_svc.hot.read().await;
        hot.grants.get(query.registry.as_str()).cloned()
    };
    let grants = grants.ok_or_else(|| AppError::not_found("registry not configured"))?;

    let package = query.package.clone().unwrap_or_default();
    let subject = Subject::Identity(identity_for(&matcher));
    // `resolution_path_for_coordinate`, not `grants.path_for` — the latter
    // cannot see the instance tier, which lives above every registry, so this
    // endpoint used to answer about a hierarchy missing its top node: a subject
    // granted a verb only there resolved to `deny` here and `allow` at the
    // server. The stored package and version tiers are the same defect at the
    // other end of the path, and RFC 0017's editor is what made it reachable for
    // every verb rather than for the three the ownership projection writes.
    let coordinate = batlehub_core::entities::PackageId::new(
        &query.registry,
        &package,
        query.version.clone().unwrap_or_default(),
    );
    let path = batlehub_core::services::authz::resolution_path_for_coordinate(
        &proxy_svc.hot,
        &grants,
        &coordinate,
    )
    .await
    .map_err(AppError::from)?;
    let resolved = resolve(&path, &subject);

    // The package and version tiers are named in `tiers_walked` when the caller
    // asked about them, whether or not storage holds a row for either: a tier
    // absent from the list reads as "not considered", and these *are*
    // considered. Saying so is the difference between "we did not look" and "we
    // looked and found nothing", which is exactly what this endpoint exists to
    // distinguish.
    //
    // Since RFC 0017 a row *can* exist, so the node may already be on the path
    // under the same label — hence the de-duplication rather than an
    // unconditional push. Listing a tier twice would suggest two nodes and a
    // precedence between them, and the model has neither.
    let mut tiers_walked: Vec<String> = path.iter().map(|n| n.label.clone()).collect();
    if !package.is_empty() {
        let mut name_tier = |label: String| {
            if !tiers_walked.contains(&label) {
                tiers_walked.push(label);
            }
        };
        name_tier(format!("package:{package}"));
        if let Some(version) = query.version.as_deref().filter(|v| !v.is_empty()) {
            name_tier(format!("version:{version}"));
        }
    }

    let allowed = resolved.holds(action);

    // §4.1's other five policies, composed the same way the enforcement path
    // composes them — one function, so the diagnostic cannot drift from the
    // thing it describes (§11.6).
    let policy = batlehub_core::services::authz::resolve_policy(
        &proxy_svc.hot,
        &query.registry,
        &package,
        query.version.as_deref().filter(|v| !v.is_empty()),
    )
    .await
    .map_err(AppError::from)?;

    // §4.7: a `deny` under an active shadow is not what the request receives.
    let today = chrono::Utc::now().date_naive();
    let shadowed_by = (!allowed)
        .then(|| {
            path.iter().find_map(|n| {
                n.dry_run
                    .as_ref()
                    .filter(|d| d.is_active(today))
                    .map(|d| ShadowNote {
                        node: n.label.clone(),
                        until: d.until,
                    })
            })
        })
        .flatten();

    Ok(web::Json(ExplainResponse {
        attributes: Attributes {
            visibility: policy.visibility.to_string(),
            prerelease_visibility: policy.prerelease_visibility.to_string(),
            immutable: policy.versioning.immutable.to_string(),
            monotonic: policy.versioning.monotonic,
            versioning_dry_run: policy.versioning.dry_run,
            exempt_gates: policy
                .exempt_gates(chrono::Utc::now())
                .into_iter()
                .map(|e| e.gate)
                .collect(),
        },
        shadowed_by,
        decision: if allowed { "allow" } else { "deny" }.to_owned(),
        reason: (!allowed)
            .then(|| format!("no grant for '{action}' on registry '{}'", query.registry)),
        resolved: resolved
            .provenance()
            .iter()
            .map(|p| ResolvedVerb {
                action: p.action.to_string(),
                granted_by: p.granted_by.clone(),
                subject: p.subject.to_string(),
            })
            .collect(),
        tiers_walked,
        not_covered: NOT_COVERED.iter().map(|s| (*s).to_owned()).collect(),
        narrowing: policy
            .narrowing
            .iter()
            .map(|(node, dropped)| NarrowingNote {
                node: node.clone(),
                dropped: dropped.clone(),
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The synthesised identity matches the subject it was built from.
    ///
    /// The round trip is the property: if it did not hold, `explain` would
    /// resolve as a caller the operator did not ask about, and report a verdict
    /// about someone else entirely.
    #[test]
    fn every_subject_form_synthesises_an_identity_that_matches_it() {
        for spelling in [
            "*",
            "role:anonymous",
            "role:user",
            "role:admin",
            "group:oidc1:eng",
            "group:*:eng",
            "group::eng",
            "user:alice",
        ] {
            let matcher = SubjectMatcher::parse(spelling).expect(spelling);
            let subject = Subject::Identity(identity_for(&matcher));
            assert!(
                matcher.matches(&subject),
                "{spelling} synthesised an identity it does not match"
            );
        }
    }

    /// A `user:` subject is a logged-in caller, so it also matches `role:user`.
    ///
    /// Worth pinning: an anonymous identity carrying a user id is not a state
    /// any auth provider produces, and resolving as one would under-report what
    /// a real `user:alice` holds.
    #[test]
    fn a_user_subject_is_authenticated() {
        let matcher = SubjectMatcher::parse("user:alice").unwrap();
        let identity = identity_for(&matcher);
        assert_eq!(identity.role, Role::User);
        assert_eq!(identity.user_id.as_deref(), Some("alice"));
    }

    /// A wildcard-provider group does not accidentally answer for a bare one.
    #[test]
    fn a_wildcard_group_subject_gets_a_provider() {
        let matcher = SubjectMatcher::parse("group:*:eng").unwrap();
        let identity = identity_for(&matcher);
        let group = &identity.groups[0];
        assert!(
            group.contains(':'),
            "a `*:` subject requires a provider: {group}"
        );
        assert!(group.ends_with(":eng"));
    }
}
