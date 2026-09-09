//! The registry rule chain, evaluated against a coordinate that carries no
//! resolved upstream metadata.
//!
//! # Why this is a free function over `HotConfigLock`
//!
//! The rule chain — RBAC, block list, release-age, licence and signature gates —
//! is configured per registry in `HotConfig::policies` and was, until this
//! module existed, reachable only through [`ProxyService`](super::ProxyService).
//! That put it on the *proxy* side of a fork every download handler makes:
//!
//! ```text
//! local/hybrid hit  → LocalRegistryService  → visibility only
//! proxy fall-through → ProxyService::handle → the whole chain
//! ```
//!
//! Handlers were expected to call `ProxyService::authorize_read` themselves
//! before taking the local branch. That is a convention, and the 2026-08-26
//! security survey found eight handlers that did not follow it — after the same
//! defect had already been found and fixed once on the OpenVSX route. A
//! convention that has to be re-applied for every new registry adapter is not a
//! control.
//!
//! Both services hold the same `Arc<RwLock<HotConfig>>`, so the chain does not
//! need `ProxyService` at all: it needs the policies. Hoisting it here lets
//! `LocalRegistryService` run the chain inside its own read funnels, which is
//! what makes the guarded path the only path rather than the polite one.

use std::sync::Arc;

use crate::entities::Action;
use crate::entities::{Identity, PackageId, PackageMetadata};
use crate::error::CoreError;
use crate::rules::{RuleContext, RuleDecision};
use crate::services::hot_config::{HotConfigLock, RegistryPolicy};

/// The coordinate the authorization entry points judge when no upstream
/// metadata has been resolved for it — a path-addressed file, or a listing that
/// names no single version. Every version-derived field is `None`, which is what
/// confines these calls to identity-keyed rules.
pub fn synthetic_metadata(package_id: &PackageId) -> PackageMetadata {
    PackageMetadata {
        id: package_id.clone(),
        published_at: None,
        download_url: None,
        checksum: None,
        is_signed: None,
        extra: serde_json::Value::Null,
        cache_control: None,
    }
}

/// The rule chain that judges this coordinate.
///
/// RFC 0015 §4.1: a `[[registries.namespaces]]` block may override a gate, so
/// the chain is a property of the **package** rather than of the registry alone.
/// The deepest matching namespace wins — several may match, and the last one in
/// config order is the most specific the operator wrote.
///
/// Falls back to the registry's chain, which is what every package in a registry
/// with no namespace overrides gets, and is the only case before phase 4.
async fn policy_for(
    hot: &HotConfigLock,
    registry: &str,
    package: &str,
) -> Option<Arc<RegistryPolicy>> {
    let hot = hot.read().await;

    if !package.is_empty() {
        if let (Some(chains), Some(grants)) = (
            hot.namespace_policies.get(registry),
            hot.grants.get(registry),
        ) {
            // `grants.kind` is the ecosystem, and the ecosystem decides the
            // separator — matching `com.acme` with `/` instead of `:` would
            // silently change which packages a namespace's gates reach. The
            // grant hierarchy carries it for exactly this reason, so it is read
            // from there rather than looked up again.
            if let Some((_, chain)) = chains.iter().rev().find(|(prefix, _)| {
                crate::entities::namespace_matches(grants.kind, prefix, package)
            }) {
                return Some(Arc::clone(chain));
            }
        }
    }
    hot.policies.get(registry).cloned()
}

/// Resolve the caller's grants, and answer the denial when the action is not
/// among them.
///
/// # Why this is here and not one layer up
///
/// RFC 0015 §5.1 replaces `RbacRule` with grant resolution, and §5.2 says the
/// two funnels in this file "stay… This RFC changes what they *call*, not that
/// they are the only way in." Both sentences point at the same place: **this**
/// is what every read path goes through.
///
/// Putting the resolution in `Authorizer::authorize` instead looked tidier and
/// was wrong, and the authorization matrix said so in one run — 44 routes
/// disclosing to a caller the config denies, because the handlers reach
/// `authorize_read` and `authorize_listing` directly and nothing was consulting
/// grants on the way. A funnel that the requests do not pass through is not a
/// funnel.
///
/// A registry with no hierarchy configured returns `Ok(())`, matching
/// `evaluate_rules`' documented "an empty rule list allows": an unknown registry
/// is a routing question, answered `404` by the handler, not an authorization
/// one.
/// The package- and version-tier nodes for a coordinate, from storage.
///
/// # The cost of asking
///
/// This is a read on the hot path, and §11.7 budgets a single-coordinate
/// `authorize` at 2 ms p99. Two things keep it affordable, and both are
/// properties of the data rather than of the code:
///
/// - **Most coordinates have no rows.** A package-tier grant is something an
///   operator wrote deliberately; the overwhelming majority of packages have
///   none, so the common case is a single indexed lookup returning nothing.
/// - **One round trip, both tiers.** `grants_for` takes the version as an
///   argument rather than being called twice.
///
/// It is deliberately *not* cached here. A cache keyed by coordinate would be
/// identity-blind, which is safe for this data — the rows are the same for
/// everyone, and the *resolution* is what differs per caller — but phase 0b
/// found the grant-set cache key to be load-bearing for **documents**, not for
/// single coordinates, and adding an unmeasured cache in front of an unmeasured
/// query is how a measurement stops meaning anything. §11.7's arm 3 measures
/// this path; the cache belongs after that number exists, not before.
async fn stored_nodes(
    hot: &HotConfigLock,
    package_id: &PackageId,
) -> Result<Vec<crate::entities::Node>, CoreError> {
    let repo = {
        let hot = hot.read().await;
        hot.grant_repo.clone()
    };
    let Some(repo) = repo else {
        return Ok(Vec::new());
    };
    if package_id.name.is_empty() {
        return Ok(Vec::new());
    }

    let version = Some(package_id.version.as_str()).filter(|v| !v.is_empty());
    let stored = repo
        .grants_for(&package_id.registry, &package_id.name, version)
        .await?;

    // One node per tier, not one per row: two nodes for the same tier would
    // make `explain` report a tier twice and would suggest a precedence the
    // model does not have.
    let mut package = crate::entities::GrantMap::new();
    let mut version_map = crate::entities::GrantMap::new();
    let (mut has_package, mut has_version) = (false, false);
    for g in stored {
        match g.node_kind {
            crate::ports::NodeKind::Package => {
                has_package = true;
                package = package.grant(g.subject, g.actions);
            }
            crate::ports::NodeKind::Version => {
                has_version = true;
                version_map = version_map.grant(g.subject, g.actions);
            }
        }
    }

    // A tier with no rows contributes `None` — *inherits* — not an empty map.
    // An empty map is a seal, and a seal on a package would stop the registry's
    // grants from reaching it: every package with no grants of its own would
    // become unreadable. §4.3's "absence is not everything" has a twin here,
    // and this is it — absence is not *nothing* either.
    let mut nodes = Vec::new();
    if has_package {
        nodes.push(crate::entities::Node::new(
            crate::entities::Tier::Package,
            format!("package:{}", package_id.name),
            Some(package),
        ));
    }
    if has_version {
        nodes.push(crate::entities::Node::new(
            crate::entities::Tier::Version,
            format!("version:{}", package_id.version),
            Some(version_map),
        ));
    }
    Ok(nodes)
}

/// [`authorize_grants`], for the proxy path.
///
/// `ProxyService::handle` and `resolve_metadata` do not go through the three
/// funnels above — they resolve upstream metadata first and evaluate the chain
/// themselves — so they need the grant check directly. Exported rather than
/// duplicated: two implementations of "does this caller hold this verb" is the
/// shape §5.0 exists to remove.
pub async fn authorize_grants_public(
    hot: &HotConfigLock,
    package_id: &PackageId,
    subject: &Identity,
    action: Action,
) -> Result<(), CoreError> {
    authorize_grants(hot, package_id, subject, action).await
}

async fn authorize_grants(
    hot: &HotConfigLock,
    package_id: &PackageId,
    subject: &Identity,
    action: Action,
) -> Result<(), CoreError> {
    let grants = {
        let hot = hot.read().await;
        hot.grants.get(package_id.registry.as_str()).cloned()
    };
    let Some(grants) = grants else {
        return Ok(());
    };
    let subject = crate::entities::Subject::Identity(subject.clone());
    let mut path = resolution_path(hot, &grants, package_id.name.as_str()).await;

    // **Short-circuit before touching storage.** Grants only widen (§4.3), so if
    // the config tiers already hold the action the deeper ones cannot take it
    // back — and the answer is the same whether or not any package or version
    // row exists.
    //
    // This is not a micro-optimisation. Without it every per-package read in a
    // whole-registry document costs one `grants` query, which is precisely the
    // N+1 phase 0b measured at 806× the cached document on the M corpus
    // (§13.2) — reintroduced by putting grants in the funnels, one layer below
    // where that measurement was taken. `Readable::Everything` is the same
    // insight applied to the document; this is it applied to the coordinate.
    if crate::entities::resolve(&path, &subject).holds(action) {
        return Ok(());
    }

    // The two tiers a config file cannot express (§4.1). Appended after the
    // config nodes because resolution walks outermost-first, and a deeper node
    // that arrived earlier would report the wrong tier in `explain`'s
    // provenance — the union is the same either way, which is exactly why the
    // ordering has to be deliberate rather than incidental.
    path.extend(stored_nodes(hot, package_id).await?);
    if crate::entities::resolve(&path, &subject).holds(action) {
        return Ok(());
    }

    // RFC 0015 §4.7 — shadow mode. The request would be refused; if any node on
    // this path is in an active shadow it is **served**, and the would-have-been
    // is recorded three ways: a log line, a counter, and the admin endpoint's
    // buffer.
    //
    // **Any node on the path, not the deepest.** A denial is the *absence* of a
    // grant rather than one node's decision, so there is no originating node to
    // take the shadow from — and the reading §4.7 needs is the permissive one:
    // "enable the new model in shadow, watch a week of real traffic, then
    // enforce" is a registry-tier shadow covering everything beneath it.
    //
    // An **expired** shadow enforces. That is the fail-closed direction and the
    // only defensible one: the alternative is a node quietly serving what it
    // should refuse because a date passed and nobody noticed, which is exactly
    // the failure the required `dry_run_until` exists to prevent.
    if let Some(node) = active_shadow(&path) {
        record_shadowed(hot, package_id, &subject, action, node).await;
        return Ok(());
    }

    Err(CoreError::AccessDenied(format!(
        "no grant for '{action}' on registry '{}'",
        package_id.registry
    )))
}

/// The instance node, as a one-element path prefix.
///
/// RFC 0015 §4.1's tier above `registry`. Prepended to every resolution so a
/// grant written there reaches everything beneath it by §4.3's union — there is
/// no new composition rule, only one more node on the path.
///
/// Absent when nothing was written, which contributes nothing rather than
/// sealing: a deployment that has never used this tier has to resolve exactly as
/// it did before the tier existed.
async fn instance_prefix(hot: &HotConfigLock) -> Vec<crate::entities::Node> {
    match hot.read().await.instance.clone() {
        Some(node) => vec![(*node).clone()],
        None => Vec::new(),
    }
}

/// **The** path a coordinate resolves over: instance, registry, then every
/// matching namespace.
///
/// One function because there is one path. `RegistryGrants::path_for` builds only
/// the part a registry knows about — it cannot see the instance tier, which lives
/// in `HotConfig` above it — so every caller that used `path_for` directly was
/// resolving against a hierarchy **missing its top node**.
///
/// That was not hypothetical. `explain` and `access-check` both did, and both are
/// diagnostics: a subject granted a verb only at the instance tier resolved to
/// *deny* in the answer and *allow* in the server. §11.6 calls that the failure
/// worth more than a missing feature — *"a diagnostic that can disagree with
/// reality is worse than none, because it is trusted"* — and §13.7 records the
/// same shape arriving through shadow mode. Two callers of a path-builder that
/// silently omitted a tier is how it arrived a second time.
///
/// So the composition happens here and the callers take it whole.
pub async fn resolution_path(
    hot: &HotConfigLock,
    grants: &crate::entities::RegistryGrants,
    package: &str,
) -> Vec<crate::entities::Node> {
    let mut path = instance_prefix(hot).await;
    path.extend(grants.path_for(package));
    path
}

/// [`resolution_path`] plus the package- and version-tier nodes from storage.
///
/// # Why the diagnostics need their own entry point
///
/// [`authorize_grants`] walks both halves, but it reaches the second one only
/// after a short-circuit: grants union, so a caller the config tiers already
/// satisfy needs no query, and skipping it is what keeps a whole-registry
/// document off the N+1 §13.2 measured at 806×. The answer is identical either
/// way — that is the point of the short-circuit — so the enforcement path can
/// afford to stop early.
///
/// A diagnostic cannot. `explain` and `access-check` are asked precisely about
/// the callers the config tiers *do not* satisfy, which is the branch the
/// short-circuit skips, and both were resolving over the config nodes alone.
/// Before RFC 0017 that under-reported the three verbs the ownership projection
/// writes; since the grants editor it under-reports any verb on either tier, and
/// `explain` was naming `package:…` and `version:…` in `tiers_walked` while
/// resolving without them — a page saying it looked where it had not.
///
/// §11.6 is explicit about which way that fails: *"a diagnostic that can
/// disagree with reality is worse than none, because it is trusted"*. So the
/// diagnostics pay the query and take the whole path.
pub async fn resolution_path_for_coordinate(
    hot: &HotConfigLock,
    grants: &crate::entities::RegistryGrants,
    package_id: &PackageId,
) -> Result<Vec<crate::entities::Node>, CoreError> {
    let mut path = resolution_path(hot, grants, package_id.name.as_str()).await;
    // Appended after the config nodes for the same reason `authorize_grants`
    // appends them there: resolution walks outermost-first, and a deeper node
    // arriving earlier would report the wrong tier in `explain`'s provenance.
    path.extend(stored_nodes(hot, package_id).await?);
    Ok(path)
}

/// Authorize a **control** verb — the endpoints RFC 0015 §4.2 deferred as
/// *"control surfaces stay `role:admin`"*.
///
/// # Two things this does that [`authorize_grants`] does not
///
/// **`registry` is optional**, because about a dozen of these endpoints name no
/// registry: config, health, the notification wiring, the block lists, the
/// authorization diagnostics. Those resolve against the instance node alone,
/// which is the tier that exists for them.
///
/// **An unknown registry is refused, not allowed.** `authorize_grants` answers
/// `Ok` for a registry it has no hierarchy for, because there an unknown name is
/// a routing question the handler answers `404`. A control endpoint takes the
/// registry from the request and acts on the server, so the same reading would
/// let a caller reach one by naming a registry that does not exist. There is no
/// `404` to fall through to when the effect is an eviction.
pub async fn authorize_control(
    hot: &HotConfigLock,
    registry: Option<&str>,
    subject: &Identity,
    action: Action,
) -> Result<(), CoreError> {
    let subject = crate::entities::Subject::Identity(subject.clone());
    let mut path = instance_prefix(hot).await;

    if let Some(registry) = registry {
        let grants = {
            let hot = hot.read().await;
            hot.grants.get(registry).cloned()
        };
        // A registry with no configured hierarchy contributes **no node** — it
        // does not refuse outright. The distinction matters and the first
        // version had it wrong: refusing here turned "this registry does not
        // exist" into a `403` even for an administrator holding the verb at the
        // instance tier, so an endpoint that should answer `404` disclosed less
        // than it should and more confusingly.
        //
        // Nothing is opened by this. Unlike `authorize_grants`, which answers
        // `Ok` for an unknown registry because there the question is routing,
        // this function requires the verb from *some* node on the path — so a
        // caller whose only grant is on another registry still resolves to
        // nothing and is refused.
        if let Some(grants) = grants {
            // The registry node only. A control verb is about the registry as a
            // whole, and namespaces match a package name this coordinate does
            // not have.
            path.push(grants.registry.clone());
        }
    }

    if crate::entities::resolve(&path, &subject).holds(action) {
        return Ok(());
    }
    Err(CoreError::AccessDenied(match registry {
        Some(r) => format!("no grant for '{action}' on registry '{r}'"),
        None => format!("no grant for '{action}'"),
    }))
}

/// The registries this caller may browse the catalogue of (RFC 0015 §4.2).
///
/// # This replaces a set, not a boolean
///
/// `AccessConfig::explore_accessible_registries_for` returned *proxy access
/// intersected with the `explore` flags*, and the explore listing uses it to
/// **scope its query** rather than to decide yes or no. So `catalogue:browse`
/// could not be wired as a gate in front of it — the shape the handlers need is
/// the set, and the verb has to produce one.
///
/// It does, because §10 rule 2 translates that same intersection into the grant:
///
/// ```text
/// (has_anonymous || has_group) && rbac.explore.anonymous
/// (has_user      || has_group) && rbac.explore.user
/// (has_admin     || has_group) && rbac.explore.admin
/// ```
///
/// That conjunction is not a guess. §13.5 records the naive reading — the flag
/// alone — producing **19 disagreements** on the §11.3 harness's first run, and
/// the corrected version is what `build_grants` emits. So this is a substitution
/// between two computations already known to agree, which is why it can be one
/// commit rather than a migration.
///
/// # Absence is still nothing
///
/// A registry with no configured hierarchy is **not** browsable. That differs
/// from `authorize_grants`, which answers `Ok` for one because there an unknown
/// registry is a routing question — here the answer is a scope, and a scope that
/// grew by one on an unknown name is survey finding 2's shape.
pub async fn browsable_registries(
    hot: &HotConfigLock,
    identity: &Identity,
) -> std::collections::HashSet<String> {
    let (instance, grants) = {
        let hot = hot.read().await;
        (hot.instance.clone(), hot.grants.clone())
    };
    let subject = crate::entities::Subject::Identity(identity.clone());

    grants
        .iter()
        .filter(|(_, registry)| {
            let mut path: Vec<crate::entities::Node> =
                instance.iter().map(|n| (**n).clone()).collect();
            path.push(registry.registry.clone());
            crate::entities::resolve(&path, &subject).holds(Action::CatalogueBrowse)
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// The first node on the path whose shadow is still in force today.
fn active_shadow(path: &[crate::entities::Node]) -> Option<&crate::entities::Node> {
    let today = chrono::Utc::now().date_naive();
    path.iter()
        .find(|n| n.dry_run.as_ref().is_some_and(|d| d.is_active(today)))
}

/// §4.7's three records of one would-have-been.
async fn record_shadowed(
    hot: &HotConfigLock,
    package_id: &PackageId,
    subject: &crate::entities::Subject,
    action: Action,
    node: &crate::entities::Node,
) {
    let until = node
        .dry_run
        .as_ref()
        .map(|d| d.until)
        .unwrap_or_else(|| chrono::Utc::now().date_naive());

    // The subject in **grant spelling**, so an operator reading the console can
    // copy it into the block that would fix this. A bare user id would leave
    // them guessing which of the five subject forms to write.
    let written = subject_spelling(subject);

    // 1. The structured log line. `warn`, not `info`: a served request that
    //    would have been refused is not routine, and an operator tailing at
    //    `info` during a migration is tailing everything.
    tracing::warn!(
        policy = "grants",
        node = %node.label,
        registry = %package_id.registry,
        package = %package_id.name,
        version = %package_id.version,
        action = %action,
        subject = %written,
        shadow_until = %until,
        "shadow mode served a request enforcement would have refused"
    );

    // 2. The counter §4.7 names, labelled by policy and node.
    metrics::counter!(
        "batlehub_policy_dryrun_total",
        "policy" => "grants",
        "node" => node.label.clone(),
    )
    .increment(1);

    // 3. The buffer the admin endpoint reads.
    let log = {
        let hot = hot.read().await;
        hot.shadow_log.clone()
    };
    if let Some(log) = log {
        log.record(crate::services::shadow::ShadowedDenial {
            at: chrono::Utc::now(),
            registry: package_id.registry.clone(),
            package: package_id.name.clone(),
            version: package_id.version.clone(),
            action: action.to_string(),
            subject: written,
            node: node.label.clone(),
            shadow_until: until,
        })
        .await;
    }
}

/// A subject in the spelling a grant would be written in.
///
/// Not `Debug`: the console shows this next to "add a grant for", and a
/// `Subject::Identity(Identity { .. })` dump is not something anyone can paste
/// into a config file.
fn subject_spelling(subject: &crate::entities::Subject) -> String {
    let identity = subject.identity();
    match &identity.user_id {
        Some(id) => format!("user:{id}"),
        // An anonymous caller has no user form, so the role is the narrowest
        // true statement about them.
        None => format!("role:{}", identity.role),
    }
}

/// Authorize a read against a registry's policy rules **without** resolving
/// upstream metadata or streaming an artifact.
///
/// The full chain runs. Callers are the paths that serve *bytes* — a local
/// artifact, a path-addressed deb/rpm file — where the proxy fall-through would
/// have run the same rules against the same synthetic coordinate.
/// Returns `AccessDenied` when the policy denies the read.
pub async fn authorize_read(
    hot: &HotConfigLock,
    package_id: &PackageId,
    identity: &Identity,
    action: Action,
) -> Result<(), CoreError> {
    // Minimal metadata: deb/rpm files have no per-version upstream metadata,
    // and the RBAC rule keys only off the identity. (The proxy fall-through
    // evaluates the same rule set against the same synthetic coordinate.)
    authorize_read_against(hot, &synthetic_metadata(package_id), identity, action).await
}

/// [`authorize_read`] against metadata the caller already holds.
///
/// **Prefer this wherever the metadata is real.** `synthetic_metadata` reports
/// `published_at`, `is_signed` and `checksum` as `None`, and two rules read
/// "absent" as "refuse": `require_signed_release` with `deny_missing_signature`,
/// and `release_age` with `deny_missing_timestamp`. Handing them a synthetic
/// coordinate for a version this instance has the row for does not gate the
/// download, it refuses it — every artifact in the registry, including the
/// properly signed ones the operator turned the gate on to require.
///
/// The proxy path has always done this: `ProxyService::handle` resolves the
/// upstream metadata first and judges the chain against *that*. This is the
/// local half of the same rule, for the local half of the same coordinate.
pub async fn authorize_read_against(
    hot: &HotConfigLock,
    metadata: &PackageMetadata,
    identity: &Identity,
    action: Action,
) -> Result<(), CoreError> {
    authorize_grants(hot, &metadata.id, identity, action).await?;

    let policy = policy_for(
        hot,
        metadata.id.registry.as_str(),
        metadata.id.name.as_str(),
    )
    .await;
    let empty: Vec<Box<dyn crate::rules::Rule>> = vec![];
    let rules = policy
        .as_ref()
        .map(|p| p.rules.as_slice())
        .unwrap_or(empty.as_slice());

    // RFC 0015 §4.5 — a gate exemption written on this version silences that
    // gate and no other. Applied in the funnel rather than inside `CveGateRule`
    // and `LicenseGateRule` for the reason this whole module exists: a rule that
    // consulted the policy table itself would be a second place the question is
    // answered, and the two would drift.
    let exempt = exempt_gates(hot, &metadata.id).await;
    let running: Vec<&Box<dyn crate::rules::Rule>> = rules
        .iter()
        .filter(|r| !exempt.iter().any(|e| e == r.name()))
        .collect();

    let ctx = RuleContext {
        identity,
        package: metadata,
        action,
        cache_entry: None,
        requested_version: Some(&metadata.id.version),
    };
    for rule in running {
        if let RuleDecision::Deny { reason } = rule.evaluate(&ctx).await {
            return Err(CoreError::AccessDenied(reason));
        }
    }
    Ok(())
}

/// The gates an active exemption silences for this coordinate (§4.5).
///
/// Empty for every coordinate with no exemption, which is almost all of them —
/// and the lookup is skipped entirely when no policy store is wired, so a
/// deployment that has never written one pays nothing.
///
/// Expired exemptions do not appear: `exempt_until` is required precisely so
/// that an exemption stops on its own rather than being revisited by someone who
/// remembers to.
async fn exempt_gates(hot: &HotConfigLock, id: &PackageId) -> Vec<String> {
    let repo = {
        let hot = hot.read().await;
        hot.policy_repo.clone()
    };
    exempt_gates_in(repo.as_ref(), id).await
}

/// [`exempt_gates`] against a policy store handed in directly — for the
/// verdict gate (RFC 0018), which holds its store rather than the hot lock.
pub async fn exempt_gates_in(
    repo: Option<&Arc<dyn crate::ports::PolicyRepository>>,
    id: &PackageId,
) -> Vec<String> {
    use crate::entities::{PolicyNode, PolicyPath, Tier};
    use crate::ports::NodeKind;

    if id.name.is_empty() || id.version.is_empty() {
        return Vec::new();
    }
    let Some(repo) = repo else {
        return Vec::new();
    };
    let Ok(stored) = repo
        .policy_for(&id.registry, &id.name, Some(&id.version))
        .await
    else {
        // A storage error must not silence a gate — the fail-closed direction
        // here is to keep every gate running, which is what an empty list does.
        return Vec::new();
    };

    let mut path = PolicyPath::default();
    for row in stored {
        let (tier, key) = match row.node_kind {
            NodeKind::Package => (Tier::Package, format!("package:{}", row.node_key)),
            NodeKind::Version => (Tier::Version, format!("version:{}", row.node_key)),
        };
        let mut node = PolicyNode::new(tier, key);
        node.rules = row.rules;
        path.nodes.push(node);
    }
    path.resolve()
        .exempt_gates(chrono::Utc::now())
        .into_iter()
        .map(|e| e.gate)
        .collect()
}

/// The rules whose verdict comes from the **version's own metadata** rather than
/// from the coordinate or the caller.
///
/// `release_age_gate` reads `published_at` and `require_signed_release` reads
/// `is_signed`, and both treat absent as *deny* when configured to. That is the
/// right answer for an upstream that did not supply the fact — which is what the
/// flags are named for — and the wrong one for a coordinate this instance simply
/// does not hold a row for.
///
/// `verdict_gate` (RFC 0018) is here for the same reason: it dates the
/// coordinate from `published_at`, and judging a synthetic `None` would hold
/// every hybrid read open-ended instead of letting it fall through to the
/// path that resolves the real date.
const METADATA_DERIVED_RULES: &[&str] =
    &["release_age_gate", "require_signed_release", "verdict_gate"];

/// The chain for a coordinate this instance holds **no version row for**.
///
/// Everything runs except [`METADATA_DERIVED_RULES`]. `block_list`, `cve_gate`,
/// `license_gate`, `version_gate`, `deny_latest` and `rbac` all answer from the
/// coordinate and the caller — both of which are in hand — so they judge here
/// exactly as they would anywhere else. A blocked version is still refused, and
/// refused with `AccessDenied` rather than becoming a `NotFound` that a Hybrid
/// registry would hand to its upstream.
///
/// What defers is the judgement that needs a version we do not have. A Hybrid
/// registry reaches this for everything it proxies, and judging that against
/// `published_at: None` / `is_signed: None` would refuse every proxied artifact
/// on a registry with either gate configured — instead of falling through to the
/// path that resolves the real metadata and runs the same chain against it.
///
/// **A skip-list, not an allow-list**, and deliberately: a rule added later runs
/// here by default. If it turns out to read metadata too, the symptom is a
/// visible over-refusal on hybrid reads; an allow-list would instead skip it
/// silently, which is how a `block_list` stops blocking.
pub async fn authorize_unheld_read(
    hot: &HotConfigLock,
    package_id: &PackageId,
    identity: &Identity,
    action: Action,
) -> Result<(), CoreError> {
    authorize_grants(hot, package_id, identity, action).await?;

    let Some(policy) =
        policy_for(hot, package_id.registry.as_str(), package_id.name.as_str()).await
    else {
        return Ok(());
    };
    let metadata = synthetic_metadata(package_id);
    // Matching on `Deny` alone is correct here now, and was not before RFC 0015
    // phase 2. A gate with a non-empty `bypass_roles` used to answer
    // `RequireRole { minimum }`, leaving the comparison to a `.resolve()` the
    // caller had to remember — so `Deny`-only matching read "admins may bypass
    // this" as "nobody is gated by this", and `version_gate`, `deny_latest` and
    // `trusted_publisher` all became no-ops on this path. Rules resolve against
    // `ctx.identity` themselves now, so there is no third verdict left to drop.
    for rule in policy
        .rules
        .iter()
        .filter(|r| !METADATA_DERIVED_RULES.contains(&r.name()))
    {
        let ctx = RuleContext {
            identity,
            package: &metadata,
            action,
            cache_entry: None,
            requested_version: Some(&package_id.version),
        };
        if let RuleDecision::Deny { reason } = rule.evaluate(&ctx).await {
            return Err(CoreError::AccessDenied(reason));
        }
    }
    Ok(())
}

/// Authorize a *listing* — a request for a whole package's version document,
/// not for one version of it.
///
/// Only the identity-keyed `rbac` rule runs. Every other rule in the chain
/// judges a **concrete version**, and a listing has none: the coordinate
/// carries the pseudo-version `"latest"` and metadata that is synthetic by
/// construction (`published_at`, `is_signed` and `checksum` are all `None`,
/// because no upstream document has been resolved for a single version).
///
/// Handing that to the full chain does not gate the listing, it blanks it.
/// `LicenseGateRule` with `allow_unknown = false` finds no licence recorded for
/// `"latest"` and denies; `ReleaseAgeGateRule` with `deny_missing_timestamp =
/// true` sees `published_at: None` and denies; `require_signed_release` sees
/// `is_signed: None` and denies; a `version_gate` allowlist matches nothing
/// against the literal `"latest"`. Each of those turns "one version in this
/// package is gated" into "`npm install` of anything from this registry fails",
/// which is the opposite of letting a resolver route *past* a gated version to
/// one it may have.
///
/// The chain is not skipped, only deferred: it still runs in full on the
/// download that follows, against the concrete version and its real metadata.
pub async fn authorize_listing(
    hot: &HotConfigLock,
    package_id: &PackageId,
    identity: &Identity,
    action: Action,
) -> Result<(), CoreError> {
    authorize_grants(hot, package_id, identity, action).await?;

    let Some(policy) =
        policy_for(hot, package_id.registry.as_str(), package_id.name.as_str()).await
    else {
        return Ok(());
    };
    // **No rule runs here, and the `rbac` filter that used to is deleted.**
    //
    // It selected `r.name() == "rbac"` from the chain, which was right when
    // `RbacRule` was in it. §5.1 took it out — grant resolution answers in its
    // place — so in production the filter has matched nothing since phase 3.
    //
    // What it still matched was **test fixtures**, which is worse than dead code.
    // `dynamic_groups.rs` builds its own policy with an `RbacRule` in it, and the
    // moment this funnel began requesting `releases:list` (§4.2) that stale rule
    // refused a group holding `releases:read` — a denial produced by a rule
    // production does not have, on a path production reaches. §13.5 records the
    // mirror image: *"a fixture that kept building the rule while production
    // resolved grants would go on passing while testing a path nobody runs."*
    // Here it went on failing.
    //
    // The grant check above is the whole of the gate. Every other rule in the
    // chain judges a concrete version and a listing has none, which is why this
    // funnel exists at all.
    let _ = &policy;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::Role;
    use crate::rules::{Rule, VersionGateRule};
    use crate::services::hot_config::HotConfig;
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

    fn blocking_gate() -> HotConfigLock {
        hot_with(vec![Box::new(VersionGateRule::new(
            &[],
            &["1.2.3".to_owned()],
            vec![Role::Admin],
        ))])
    }

    /// A gate with a non-empty `bypass_roles` still refuses a caller who lacks
    /// them.
    ///
    /// This was a real bug, not a hypothetical: the gate answered
    /// `RequireRole`, this function matched on `Deny` alone, and the blocked
    /// version went to *everyone* — the rule became a no-op the moment an
    /// operator named a bypass role. The verdict that made it possible is
    /// deleted (RFC 0015 §5.1), and this row is what proves the behaviour
    /// survived the deletion.
    #[tokio::test]
    async fn a_gate_with_bypass_roles_still_refuses_a_caller_who_lacks_them() {
        let pkg = PackageId::new("reg", "pkg", "1.2.3");
        let err = authorize_unheld_read(
            &blocking_gate(),
            &pkg,
            &Identity::anonymous(),
            Action::ReleasesRead,
        )
        .await
        .expect_err("a blocked version must not be readable by an anonymous caller");
        assert!(matches!(err, CoreError::AccessDenied(_)), "{err:?}");
    }

    /// …and the role the operator named does still bypass it.
    #[tokio::test]
    async fn a_caller_holding_a_bypass_role_is_allowed() {
        let pkg = PackageId::new("reg", "pkg", "1.2.3");
        let admin = Identity {
            user_id: Some("root".to_owned()),
            role: Role::Admin,
            auth_provider: None,
            groups: vec![],
        };
        authorize_unheld_read(&blocking_gate(), &pkg, &admin, Action::ReleasesRead)
            .await
            .expect("a bypass role must still bypass");
    }

    /// The gate is a gate, not a wall: an unblocked coordinate is unaffected.
    #[tokio::test]
    async fn an_ungated_version_is_allowed() {
        let pkg = PackageId::new("reg", "pkg", "1.2.4");
        authorize_unheld_read(
            &blocking_gate(),
            &pkg,
            &Identity::anonymous(),
            Action::ReleasesRead,
        )
        .await
        .expect("only the blocked version is gated");
    }

    // ── RFC 0015 §4.5: gate exemptions ───────────────────────────────────────

    /// A gate that always refuses, under a name the test chooses.
    ///
    /// Standing in for `CveGateRule` so the assertion is about the **funnel**
    /// rather than about a finding: what is being tested is that an exemption
    /// removes a gate from the chain, and a real CVE rule would need a
    /// vulnerability fixture to say the same thing less directly.
    struct AlwaysDeny(&'static str);

    #[async_trait::async_trait]
    impl Rule for AlwaysDeny {
        fn name(&self) -> &str {
            self.0
        }
        async fn evaluate(&self, _ctx: &RuleContext<'_>) -> RuleDecision {
            RuleDecision::Deny {
                reason: format!("{} refused", self.0),
            }
        }
    }

    /// Wire a version-tier exemption for `gate` into `hot`.
    async fn exempt(hot: &HotConfigLock, gate: &str, until: chrono::DateTime<chrono::Utc>) {
        use crate::entities::GateExemption;
        use crate::ports::{version_node_key, NodeKind, PolicyRepository, StoredPolicy};

        let repo = {
            let hot_read = hot.read().await;
            hot_read.policy_repo.clone()
        };
        let repo = repo.expect("the fixture wired a policy store");
        let mut policy =
            StoredPolicy::new("reg", NodeKind::Version, version_node_key("pkg", "1.2.3"));
        let exemption = GateExemption {
            exempt: true,
            gate: gate.to_owned(),
            exempt_until: until,
            reason: "assessed: not reachable from our usage".to_owned(),
            granted_by: Some("sec".to_owned()),
            self_approved: false,
        };
        policy.rules.push(crate::entities::RuleOverride {
            gate: gate.to_owned(),
            settings: serde_json::to_value(&exemption).expect("serialises"),
        });
        PolicyRepository::put_policy(&*repo, policy)
            .await
            .expect("stores");
    }

    fn hot_with_policy_store(rules: Vec<Box<dyn Rule>>) -> HotConfigLock {
        let hot = hot_with(rules);
        // A test-local `PolicyRepository`, so `crates/core` does not depend on
        // the adapter crate to exercise its own funnel.
        #[derive(Default)]
        struct MemPolicy(RwLock<Vec<crate::ports::StoredPolicy>>);

        #[async_trait::async_trait]
        impl crate::ports::PolicyRepository for MemPolicy {
            async fn policy_for(
                &self,
                registry: &str,
                package: &str,
                version: Option<&str>,
            ) -> Result<Vec<crate::ports::StoredPolicy>, CoreError> {
                let key = version.map(|v| crate::ports::version_node_key(package, v));
                Ok(self
                    .0
                    .read()
                    .await
                    .iter()
                    .filter(|p| {
                        p.registry == registry
                            && match p.node_kind {
                                crate::ports::NodeKind::Package => p.node_key == package,
                                crate::ports::NodeKind::Version => {
                                    key.as_deref() == Some(p.node_key.as_str())
                                }
                            }
                    })
                    .cloned()
                    .collect())
            }
            async fn put_policy(
                &self,
                policy: crate::ports::StoredPolicy,
            ) -> Result<(), CoreError> {
                self.0.write().await.push(policy);
                Ok(())
            }
            async fn delete_policy(
                &self,
                _r: &str,
                _k: crate::ports::NodeKind,
                _n: &str,
            ) -> Result<(), CoreError> {
                Ok(())
            }
            async fn policy_on_node(
                &self,
                _r: &str,
                _k: crate::ports::NodeKind,
                _n: &str,
            ) -> Result<Option<crate::ports::StoredPolicy>, CoreError> {
                Ok(None)
            }
            async fn exemptions_in_registry(
                &self,
                _r: &str,
            ) -> Result<Vec<crate::ports::StoredPolicy>, CoreError> {
                Ok(Vec::new())
            }
            async fn delete_package_policy(&self, _r: &str, _p: &str) -> Result<(), CoreError> {
                Ok(())
            }
        }

        {
            let h = hot.clone();
            futures::executor::block_on(async move {
                h.write().await.policy_repo =
                    Some(Arc::new(MemPolicy::default()) as Arc<dyn crate::ports::PolicyRepository>);
            });
        }
        hot
    }

    fn identity() -> Identity {
        Identity {
            user_id: Some("u".to_owned()),
            role: Role::User,
            auth_provider: None,
            groups: Vec::new(),
        }
    }

    fn metadata_for() -> PackageMetadata {
        synthetic_metadata(&PackageId::new("reg", "pkg", "1.2.3"))
    }

    /// An exemption silences its gate.
    #[tokio::test]
    async fn an_active_exemption_silences_its_gate() {
        let hot = hot_with_policy_store(vec![Box::new(AlwaysDeny("cve_gate"))]);
        let md = metadata_for();

        // Control: the gate refuses before the exemption exists.
        assert!(
            authorize_read_against(&hot, &md, &identity(), Action::ReleasesRead)
                .await
                .is_err(),
            "without the exemption the gate must refuse, or this test proves nothing"
        );

        exempt(
            &hot,
            "cve_gate",
            chrono::Utc::now() + chrono::Duration::days(30),
        )
        .await;
        assert!(
            authorize_read_against(&hot, &md, &identity(), Action::ReleasesRead)
                .await
                .is_ok()
        );
    }

    /// …and only its gate.
    ///
    /// The per-gate rule (§4.1) at the point it matters most: an exemption is a
    /// deliberate weakening, and one that silenced a gate nobody assessed would
    /// be the worst possible reading of it.
    #[tokio::test]
    async fn an_exemption_silences_only_the_gate_it_names() {
        let hot = hot_with_policy_store(vec![
            Box::new(AlwaysDeny("cve_gate")),
            Box::new(AlwaysDeny("release_age_gate")),
        ]);
        exempt(
            &hot,
            "cve_gate",
            chrono::Utc::now() + chrono::Duration::days(30),
        )
        .await;

        let err = authorize_read_against(&hot, &metadata_for(), &identity(), Action::ReleasesRead)
            .await
            .expect_err("the un-exempted gate must still refuse");
        assert!(
            err.to_string().contains("release_age_gate"),
            "the surviving refusal must come from the other gate: {err}"
        );
    }

    /// An expired exemption silences nothing.
    ///
    /// `exempt_until` is required precisely so an exemption stops on its own —
    /// §4.5's realistic failure is *"not a wrong assessment, it is a right
    /// assessment nobody revisited"*. This is that sentence as a test.
    #[tokio::test]
    async fn an_expired_exemption_does_not_silence_its_gate() {
        let hot = hot_with_policy_store(vec![Box::new(AlwaysDeny("cve_gate"))]);
        exempt(
            &hot,
            "cve_gate",
            chrono::Utc::now() - chrono::Duration::days(1),
        )
        .await;

        assert!(
            authorize_read_against(&hot, &metadata_for(), &identity(), Action::ReleasesRead)
                .await
                .is_err(),
            "an exemption that has expired must let its gate run again"
        );
    }

    /// An exemption written on a gate that may not carry one is ignored on the
    /// read path, not trusted.
    ///
    /// Belt and braces: the API refuses to write one, and this is what happens
    /// if a row reaches storage another way. The fail-closed direction here is
    /// to keep the gate running.
    #[tokio::test]
    async fn an_exemption_on_a_non_exemptible_gate_is_ignored() {
        let hot = hot_with_policy_store(vec![Box::new(AlwaysDeny("release_age_gate"))]);
        exempt(
            &hot,
            "release_age_gate",
            chrono::Utc::now() + chrono::Duration::days(30),
        )
        .await;

        assert!(
            authorize_read_against(&hot, &metadata_for(), &identity(), Action::ReleasesRead)
                .await
                .is_err(),
            "a quarantine a version can skip is not a quarantine"
        );
    }
}

#[cfg(test)]
mod instance_tier_tests {
    use super::*;
    use crate::entities::{
        GrantMap, Node, RegistryGrants, RegistryKind, Role, SubjectMatcher, Tier,
    };
    use crate::services::hot_config::HotConfig;
    use tokio::sync::RwLock;

    fn identity(role: Role) -> Identity {
        Identity {
            user_id: Some("u".to_owned()),
            role,
            auth_provider: None,
            groups: vec![],
        }
    }

    fn node(tier: Tier, label: &str, role: Role, actions: &[Action]) -> Node {
        Node::new(
            tier,
            label,
            Some(GrantMap::new().grant(SubjectMatcher::Role(role), actions.to_vec())),
        )
    }

    /// A server with an optional instance node and an optional registry `reg`.
    fn hot(instance: Option<Node>, registry: Option<Node>) -> HotConfigLock {
        let mut cfg = HotConfig {
            instance: instance.map(Arc::new),
            ..Default::default()
        };
        if let Some(registry) = registry {
            cfg.grants.insert(
                "reg".to_owned(),
                Arc::new(RegistryGrants {
                    kind: RegistryKind::Npm,
                    registry,
                    namespaces: Vec::new(),
                }),
            );
        }
        Arc::new(RwLock::new(cfg))
    }

    /// A registry that grants **this** caller nothing, without sealing.
    ///
    /// Not an empty `GrantMap`: §4.3 makes that a *seal*, and a seal at the
    /// registry tier now cuts the instance tier off above it — which is correct
    /// and is why config load refuses one (§13.5: *"a seal stops a node
    /// inheriting from its ancestors, and a registry has none"*, a sentence the
    /// instance tier has since made half-true). A fixture that sealed by accident
    /// would have made the instance-tier tests pass for the wrong reason.
    fn irrelevant_registry() -> Node {
        Node::new(
            Tier::Registry,
            "registry:reg",
            Some(GrantMap::new().grant(
                SubjectMatcher::User("somebody-else".to_owned()),
                [Action::ReleasesRead],
            )),
        )
    }

    /// A registry whose grant map is empty — which is a seal (§4.3).
    fn sealed_registry() -> Node {
        Node::new(Tier::Registry, "registry:reg", Some(GrantMap::new()))
    }

    // ── authorize_control ────────────────────────────────────────────────────

    /// An instance grant reaches an endpoint that names no registry.
    ///
    /// The tier exists for exactly this: about a dozen control endpoints name
    /// none, and §4.1's hierarchy started at `registry`, so before it there was
    /// no node their grants could attach to.
    #[tokio::test]
    async fn an_instance_grant_answers_an_endpoint_that_names_no_registry() {
        let hot = hot(
            Some(node(
                Tier::Instance,
                "instance",
                Role::Admin,
                &[Action::ConfigRead],
            )),
            None,
        );
        assert!(
            authorize_control(&hot, None, &identity(Role::Admin), Action::ConfigRead)
                .await
                .is_ok()
        );
        assert!(
            authorize_control(&hot, None, &identity(Role::User), Action::ConfigRead)
                .await
                .is_err(),
            "and reaches nobody it does not name"
        );
    }

    /// A registry-scoped check resolves the **union** of the two tiers.
    ///
    /// Both directions, because an implementation that consulted only one would
    /// pass whichever test used the other: an administrator holding the verb at
    /// the instance tier passes on any registry, and a delegate holding it on one
    /// registry passes there and nowhere else.
    #[tokio::test]
    async fn a_registry_scoped_check_unions_the_instance_and_registry_tiers() {
        let hot = hot(
            Some(node(
                Tier::Instance,
                "instance",
                Role::Admin,
                &[Action::CacheEvict],
            )),
            Some(node(
                Tier::Registry,
                "registry:reg",
                Role::User,
                &[Action::CacheEvict],
            )),
        );
        // The administrator, through the instance tier.
        assert!(authorize_control(
            &hot,
            Some("reg"),
            &identity(Role::Admin),
            Action::CacheEvict
        )
        .await
        .is_ok());
        // The delegate, through the registry tier.
        assert!(
            authorize_control(&hot, Some("reg"), &identity(Role::User), Action::CacheEvict)
                .await
                .is_ok()
        );
        // …and the delegate reaches no other registry, because there is no node
        // for one.
        assert!(authorize_control(
            &hot,
            Some("other"),
            &identity(Role::User),
            Action::CacheEvict
        )
        .await
        .is_err());
    }

    /// An unknown registry contributes **no node** — it does not refuse.
    ///
    /// The first version refused outright, which turned "this registry does not
    /// exist" into a `403` even for an administrator holding the verb at the
    /// instance tier, so an endpoint that should answer `404` never got the
    /// chance. Nothing is opened by the correction, which the second half
    /// asserts: unlike `authorize_grants`, this function still requires the verb
    /// from *some* node.
    #[tokio::test]
    async fn an_unknown_registry_contributes_no_node_rather_than_refusing() {
        let hot = hot(
            Some(node(
                Tier::Instance,
                "instance",
                Role::Admin,
                &[Action::CacheEvict],
            )),
            None,
        );
        assert!(
            authorize_control(
                &hot,
                Some("nope"),
                &identity(Role::Admin),
                Action::CacheEvict
            )
            .await
            .is_ok(),
            "the instance grant still answers, so the handler reaches its own 404"
        );
        assert!(
            authorize_control(
                &hot,
                Some("nope"),
                &identity(Role::User),
                Action::CacheEvict
            )
            .await
            .is_err(),
            "…and a caller with no grant anywhere is still refused"
        );
    }

    /// A registry that grants nothing does not become a hole.
    #[tokio::test]
    async fn a_registry_granting_nothing_refuses() {
        let hot = hot(None, Some(irrelevant_registry()));
        assert!(authorize_control(
            &hot,
            Some("reg"),
            &identity(Role::Admin),
            Action::CacheEvict
        )
        .await
        .is_err());
    }

    /// No instance node at all resolves exactly as the server did before the
    /// tier existed — it contributes nothing, it does not seal.
    #[tokio::test]
    async fn an_absent_instance_node_contributes_nothing_and_seals_nothing() {
        let hot = hot(
            None,
            Some(node(
                Tier::Registry,
                "registry:reg",
                Role::Admin,
                &[Action::CacheEvict],
            )),
        );
        assert!(authorize_control(
            &hot,
            Some("reg"),
            &identity(Role::Admin),
            Action::CacheEvict
        )
        .await
        .is_ok());
    }

    // ── the ordinary read path ───────────────────────────────────────────────

    /// An instance-tier grant reaches an ordinary package read.
    ///
    /// `authorize_grants` prepends the instance node, so a subject granted
    /// `releases:read` there reads every package beneath — the union of §4.3
    /// applied to one more tier, with no new rule.
    #[tokio::test]
    async fn an_instance_grant_reaches_a_package_read() {
        let hot = hot(
            Some(node(
                Tier::Instance,
                "instance",
                Role::User,
                &[Action::ReleasesRead],
            )),
            Some(irrelevant_registry()),
        );
        let id = PackageId::new("reg", "pkg", "1.0.0");
        assert!(
            authorize_read(&hot, &id, &identity(Role::User), Action::ReleasesRead)
                .await
                .is_ok(),
            "the registry grants nothing, so this can only be the instance tier"
        );
        assert!(
            authorize_read(&hot, &id, &identity(Role::Anonymous), Action::ReleasesRead)
                .await
                .is_err()
        );
    }

    /// `resolution_path` names the instance tier first, and it is the same path
    /// the decision uses.
    ///
    /// `explain` reports `tiers_walked` from this, and reported a hierarchy
    /// missing its top node while the server resolved with it — §11.6's
    /// *"a diagnostic that can disagree with reality is worse than none"*. The
    /// order matters too: outermost-first is what makes `granted_by` name the
    /// tier an operator would edit.
    #[tokio::test]
    async fn the_resolution_path_starts_at_the_instance_tier() {
        let instance = node(
            Tier::Instance,
            "instance",
            Role::Admin,
            &[Action::ConfigRead],
        );
        let hot = hot(Some(instance), Some(irrelevant_registry()));
        let grants = { hot.read().await.grants.get("reg").cloned().unwrap() };
        let path = resolution_path(&hot, &grants, "pkg").await;
        let labels: Vec<&str> = path.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(labels, vec!["instance", "registry:reg"]);
    }

    /// A registry-tier **seal** cuts off the instance tier above it.
    ///
    /// Correct by §4.3 — a seal stops inheritance from every ancestor, and the
    /// registry now has one — and recorded because §13.5 justifies refusing a
    /// registry-tier seal at config load with *"a registry has none"*, which the
    /// instance tier has made half-true. The config rejection is what keeps this
    /// unreachable in practice; this pins what would happen if it were not.
    #[tokio::test]
    async fn a_registry_tier_seal_cuts_off_the_instance_tier() {
        let hot = hot(
            Some(node(
                Tier::Instance,
                "instance",
                Role::User,
                &[Action::ReleasesRead],
            )),
            Some(sealed_registry()),
        );
        assert!(authorize_read(
            &hot,
            &PackageId::new("reg", "pkg", "1.0.0"),
            &identity(Role::User),
            Action::ReleasesRead
        )
        .await
        .is_err());
    }
}
