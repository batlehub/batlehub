//! Why a version is not pullable, or not pullable *yet* (RFC 0040 §6.1).
//!
//! One answer, computed from the functions the download funnels call and in
//! their order — the grant chain, the local package's visibility, the
//! metadata the download would resolve, the registry's rule chain — so `why`
//! cannot report a cause the download does not apply, or miss one it does.
//! Every cause is reported, not only the first that refuses.
//!
//! **Evaluate-only.** Everything runs under [`scoped`]: the verdict gate reads
//! the verdict without queuing a scan or persisting a hold, and a shadowed
//! grant adds nothing to the shadow evidence (T2b). The one write is the
//! metadata cache a proxied miss fills, exactly as the install would
//! (decision 35).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::entities::{
    Action, Identity, PackageId, PackageMetadata, PublishedPackage, ReasonCode, Role, Verdict,
    VerdictState,
};
use crate::error::CoreError;
use crate::rules::{RuleContext, RuleDecision};
use crate::services::{LocalRegistryService, ProxyRequest, ProxyService};

tokio::task_local! {
    static EVALUATE_ONLY: ();
}

/// Whether the current task is computing a pullability answer, which must
/// write nothing the download would.
pub fn evaluate_only() -> bool {
    EVALUATE_ONLY.try_with(|_| ()).is_ok()
}

/// Run `f` evaluate-only.
pub async fn scoped<F: std::future::Future>(f: F) -> F::Output {
    EVALUATE_ONLY.scope((), f).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CauseState {
    Pass,
    Warn,
    /// Lifts on its own: a clock, or a scanner's answer.
    Wait,
    /// Lifts only when somebody acts.
    Refuse,
}

/// One of the things the download checks, and what it said.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Cause {
    /// `access`, `block`, `lifecycle`, `policy`, `verdict` or `upstream`.
    pub kind: String,
    pub state: CauseState,
    /// The stable code (`MIN_AGE_NOT_MET`, `BLOCK_LIST`, …) when the cause has
    /// one; the docs anchor is derived from it.
    pub reason_code: Option<String>,
    /// When a `wait` lifts, if it is a clock.
    pub until: Option<DateTime<Utc>>,
    /// One line, fit for the person asking: never another subject's grant, a
    /// rule's configuration or an administrator's note (T2).
    pub detail: String,
    /// What the caller can do about it, when it is not `pass`.
    pub next: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Headline {
    Pullable,
    Waiting,
    Refused,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Pullability {
    pub headline: Headline,
    /// For `waiting`: the latest clock among the causes, when every one that
    /// waits has a clock. Absent when one waits on a scanner.
    pub until: Option<DateTime<Utc>>,
    pub causes: Vec<Cause>,
    /// The version judged: the one asked for, or — with none asked — the
    /// newest the caller would receive.
    pub version: String,
}

/// The answer, or that there is nothing the caller may know about.
#[derive(Debug)]
pub enum Answer {
    /// Invisible to this caller, or unknown: the two are answered alike (T1).
    NotFound,
    Found(Pullability),
}

/// What the web layer knows about the registry and the caller.
#[derive(Debug, Clone, Copy)]
pub struct Scope {
    /// The caller may see the registry at all.
    pub visible: bool,
    /// A version this instance does not hold is asked of the upstream
    /// (`proxy`, `hybrid`); `false` for a `local` registry.
    pub falls_through: bool,
}

pub struct PullabilityService {
    pub proxy: Arc<ProxyService>,
    pub local: Arc<LocalRegistryService>,
}

impl PullabilityService {
    pub fn new(proxy: Arc<ProxyService>, local: Arc<LocalRegistryService>) -> Self {
        Self { proxy, local }
    }

    /// Why `registry/name@version` is or is not pullable by `identity`.
    /// `scope` is the web layer's half: registry access and the registry's
    /// mode live there. Everything else is decided here.
    pub async fn explain(
        &self,
        identity: &Identity,
        package: &PackageId,
        scope: Scope,
    ) -> Result<Answer, CoreError> {
        let started = std::time::Instant::now();
        let answer = scoped(self.explain_inner(identity, package, scope)).await;
        let headline = match &answer {
            Ok(Answer::Found(p)) => match p.headline {
                Headline::Pullable => "pullable",
                Headline::Waiting => "waiting",
                Headline::Refused => "refused",
            },
            Ok(Answer::NotFound) => "not_found",
            Err(_) => "error",
        };
        metrics::counter!("batlehub_pullability_requests_total", "headline" => headline)
            .increment(1);
        metrics::histogram!("batlehub_pullability_duration_seconds")
            .record(started.elapsed().as_secs_f64());
        answer
    }

    async fn explain_inner(
        &self,
        identity: &Identity,
        package: &PackageId,
        scope: Scope,
    ) -> Result<Answer, CoreError> {
        let registry_visible = scope.visible;
        let (registry, name, version) = (&package.registry, &package.name, &package.version);
        // ── 1. Visibility: the registry, the grant chain, the package ────────
        if !registry_visible
            || crate::services::authz::authorize_grants_public(
                &self.local.hot,
                package,
                identity,
                Action::ReleasesRead,
            )
            .await
            .is_err()
            || self
                .local
                .check_visibility(registry, name, identity)
                .await
                .is_err()
        {
            return Ok(Answer::NotFound);
        }
        let mut causes = vec![Cause {
            kind: "access".into(),
            state: CauseState::Pass,
            reason_code: None,
            until: None,
            detail: format!("you may read {registry}"),
            next: None,
        }];

        // ── 2. What the download would judge: the local row, else upstream ───
        let row: Option<PublishedPackage> = self
            .local
            .backend
            .get_versions(registry, name)
            .await?
            .into_iter()
            .find(|p| &p.version == version);
        let (metadata, policy) = match &row {
            Some(pkg) => (local_metadata(package, pkg), None),
            // A `local` registry has nothing behind it: no row is a 404 for
            // the download, so it is not-found here.
            None if !scope.falls_through => return Ok(Answer::NotFound),
            None => {
                let req = ProxyRequest {
                    package_id: package.clone(),
                    identity: identity.clone(),
                    action: Action::ReleasesRead,
                    ip_address: None,
                    user_agent: None,
                };
                match self.proxy.explain_inputs(&req).await {
                    Ok((metadata, policy)) => {
                        causes.push(Cause {
                            kind: "upstream".into(),
                            state: CauseState::Pass,
                            reason_code: None,
                            until: None,
                            detail: "metadata resolved".into(),
                            next: None,
                        });
                        (metadata, policy)
                    }
                    // Unknown upstream and unknown here: the same not-found an
                    // invisible package gets.
                    Err(CoreError::NotFound(_)) => return Ok(Answer::NotFound),
                    Err(e) => {
                        causes.push(Cause {
                            kind: "upstream".into(),
                            state: CauseState::Refuse,
                            reason_code: None,
                            until: None,
                            detail: format!("the upstream could not be read: {e}"),
                            next: Some("retry later; the upstream or this instance's route to it is failing".into()),
                        });
                        return Ok(Answer::Found(finish(causes, version)));
                    }
                }
            }
        };

        // ── 3. The version's lifecycle (local rows) ───────────────────────────
        if let Some(pkg) = &row {
            let (state, detail) = match (pkg.yanked, pkg.unlisted) {
                (true, _) => (
                    CauseState::Warn,
                    "yanked: new resolutions skip it, an exact pin still installs it",
                ),
                (false, true) => (
                    CauseState::Warn,
                    "unlisted: hidden from search, an exact version still installs it",
                ),
                (false, false) => (CauseState::Pass, "not yanked, listed"),
            };
            causes.push(Cause {
                kind: "lifecycle".into(),
                state,
                reason_code: None,
                until: None,
                detail: detail.into(),
                next: None,
            });
        }

        // ── 4. The rule chain, every rule ─────────────────────────────────────
        let policy = match policy {
            Some(p) => Some(p),
            None => {
                let hot = self.local.hot.read().await;
                crate::services::authz::policy_in(&hot, registry, name)
            }
        };
        let ctx = RuleContext {
            identity,
            package: &metadata,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: Some(version),
        };
        let empty: Vec<Box<dyn crate::rules::Rule>> = Vec::new();
        let rules = policy
            .as_ref()
            .map(|p| p.rules.as_slice())
            .unwrap_or(&empty);
        let (decisions, verdict) = crate::services::verdict::with_request_verdict(
            crate::services::authz::evaluate_chain_all(&self.local.hot, rules, &metadata.id, &ctx),
        )
        .await;
        let is_admin = identity.role == Role::Admin;
        let mut blocked = false;
        for (rule, decision) in &decisions {
            if let Some(cause) = rule_cause(rule, decision, verdict.as_ref(), &metadata, is_admin) {
                blocked |= cause.kind == "block" && cause.state == CauseState::Refuse;
                causes.push(cause);
            }
        }
        if !blocked {
            causes.push(Cause {
                kind: "block".into(),
                state: CauseState::Pass,
                reason_code: None,
                until: None,
                detail: "not blocked".into(),
                next: None,
            });
        }
        Ok(Answer::Found(finish(causes, version)))
    }
}

/// `why <registry/name>` with no version (§4.8): which version a client asking
/// for the newest receives, and the state of the newest few.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Resolution {
    /// The newest stable version that is pullable now; `None` when none is.
    pub resolved_version: Option<String>,
    /// Newest first, at most [`RESOLUTION_DEPTH`].
    pub versions: Vec<Pullability>,
}

/// How many versions a resolution answer judges (§4.8: "the last ten").
pub const RESOLUTION_DEPTH: usize = 10;

impl PullabilityService {
    /// [`Self::explain`] for the newest versions of a package. `None` when
    /// the package is invisible or unknown, alike (T1).
    pub async fn explain_resolution(
        &self,
        identity: &Identity,
        registry: &str,
        name: &str,
        scope: Scope,
    ) -> Result<Option<Resolution>, CoreError> {
        // (version, published_at, prerelease), from the local rows and the
        // upstream's own list — the console's discovery read, which asks
        // nothing when the registry turns it off.
        let mut candidates: Vec<(String, Option<DateTime<Utc>>, bool)> = self
            .local
            .backend
            .get_versions(registry, name)
            .await?
            .into_iter()
            .map(|p| {
                let pre = p.version.contains('-');
                (p.version, Some(p.published_at), pre)
            })
            .collect();
        let upstream = if scope.falls_through {
            scoped(self.proxy.upstream_detail(registry, name, identity)).await
        } else {
            Ok(None)
        };
        if let Ok(Some(found)) = upstream {
            for v in found.detail.versions {
                if !candidates.iter().any(|(c, _, _)| *c == v.version) {
                    candidates.push((v.version, v.published_at, v.is_prerelease));
                }
            }
        }
        candidates.sort_by_key(|c| std::cmp::Reverse(c.1));
        candidates.truncate(RESOLUTION_DEPTH);
        let mut versions = Vec::new();
        let mut resolved = None;
        for (version, _, prerelease) in candidates {
            let id = PackageId::new(registry, name, &version);
            match self.explain(identity, &id, scope).await? {
                Answer::NotFound => continue,
                Answer::Found(p) => {
                    if resolved.is_none() && !prerelease && p.headline == Headline::Pullable {
                        resolved = Some(version);
                    }
                    versions.push(p);
                }
            }
        }
        if versions.is_empty() {
            return Ok(None);
        }
        Ok(Some(Resolution {
            resolved_version: resolved,
            versions,
        }))
    }
}

/// The headline from the causes: any refusal refuses; else any wait waits;
/// else pullable.
fn finish(mut causes: Vec<Cause>, version: &str) -> Pullability {
    // The server's order (§4.8): access, block, lifecycle, policy, verdict,
    // upstream. Stable, so causes of one kind keep chain order.
    let rank = |k: &str| match k {
        "access" => 0,
        "block" => 1,
        "lifecycle" => 2,
        "policy" => 3,
        "verdict" => 4,
        _ => 5,
    };
    causes.sort_by_key(|c| rank(&c.kind));
    let worst = causes
        .iter()
        .map(|c| c.state)
        .max()
        .unwrap_or(CauseState::Pass);
    let headline = match worst {
        CauseState::Refuse => Headline::Refused,
        CauseState::Wait => Headline::Waiting,
        CauseState::Pass | CauseState::Warn => Headline::Pullable,
    };
    let waits: Vec<&Cause> = causes
        .iter()
        .filter(|c| c.state == CauseState::Wait)
        .collect();
    let until = if headline == Headline::Waiting && waits.iter().all(|c| c.until.is_some()) {
        waits.iter().filter_map(|c| c.until).max()
    } else {
        None
    };
    Pullability {
        headline,
        until,
        causes,
        version: version.to_owned(),
    }
}

/// One rule's decision as a cause. `None` for a rule that allowed and has
/// nothing to say (the verdict gate always says something).
fn rule_cause(
    rule: &str,
    decision: &RuleDecision,
    verdict: Option<&Verdict>,
    metadata: &PackageMetadata,
    is_admin: bool,
) -> Option<Cause> {
    if rule == "verdict_gate" {
        return Some(verdict_cause(verdict));
    }
    let RuleDecision::Deny { reason } = decision else {
        return None;
    };
    let cause = |kind: &str, state, code: Option<ReasonCode>, detail: String, next: &str| Cause {
        kind: kind.into(),
        state,
        reason_code: code.map(|c| c.as_str().to_owned()),
        until: None,
        detail,
        next: Some(next.into()),
    };
    Some(match rule {
        // The administrator's note is theirs (T2): the code says what kind of
        // refusal it is, and an admin reading `why` sees the note too.
        "block_list" => cause(
            "block",
            CauseState::Refuse,
            Some(ReasonCode::BlockList),
            if is_admin {
                reason.clone()
            } else {
                "blocked by an administrator".into()
            },
            "ask an administrator to unblock it, or use another version",
        ),
        // A clock, not a decision: it lifts once the version is old enough.
        "release_age_gate" => Cause {
            until: None,
            ..cause(
                "policy",
                if metadata.published_at.is_some() {
                    CauseState::Wait
                } else {
                    CauseState::Refuse
                },
                Some(if metadata.published_at.is_some() {
                    ReasonCode::MinAgeNotMet
                } else {
                    ReasonCode::TimestampMissing
                }),
                reason.clone(),
                "wait for the release-age gate, or pin an older version",
            )
        },
        "license_gate" => cause(
            "policy",
            CauseState::Refuse,
            Some(ReasonCode::LicenseDenied),
            reason.clone(),
            "use another version, or ask an administrator for an exemption",
        ),
        "require_signed_release" => cause(
            "policy",
            CauseState::Refuse,
            Some(ReasonCode::SignatureMissing),
            reason.clone(),
            "use a signed version, or ask an administrator for an exemption",
        ),
        "trusted_publisher" => cause(
            "policy",
            CauseState::Refuse,
            Some(ReasonCode::UntrustedPublisher),
            reason.clone(),
            "use another version, or ask an administrator for an exemption",
        ),
        "cve_gate" => cause(
            "policy",
            CauseState::Refuse,
            Some(ReasonCode::Vulnerability),
            reason.clone(),
            "use a fixed version, or ask an administrator for an exemption",
        ),
        // deny_latest, version_gate, flags and the rest: the registry's own
        // rules, refused until the request or the policy changes.
        _ => cause(
            "policy",
            CauseState::Refuse,
            None,
            reason.clone(),
            "pin another version, or ask an administrator about the registry's policy",
        ),
    })
}

/// The code a refusal by `rule` is recorded under: what `why` would name for
/// it. For the funnels' access log (decision 39), so the two never disagree.
pub fn reason_code_for(
    rule: &str,
    verdict: Option<&Verdict>,
    metadata: &PackageMetadata,
) -> Option<String> {
    let deny = RuleDecision::Deny {
        reason: String::new(),
    };
    rule_cause(rule, &deny, verdict, metadata, false).and_then(|c| c.reason_code)
}

/// The verdict gate's word: its state, the first reason code, the clock.
fn verdict_cause(verdict: Option<&Verdict>) -> Cause {
    let Some(v) = verdict else {
        // The gate allowed without leaving a verdict (no security profile on
        // this registry, or an exemption): nothing held.
        return Cause {
            kind: "verdict".into(),
            state: CauseState::Pass,
            reason_code: None,
            until: None,
            detail: "no verdict holds it".into(),
            next: None,
        };
    };
    let code = v.reason_codes.first().map(|c| c.as_str().to_owned());
    let (state, next) = match v.state {
        VerdictState::Allowed => (CauseState::Pass, None),
        VerdictState::Warned => (CauseState::Warn, None),
        VerdictState::Quarantined if v.reason_codes.contains(&ReasonCode::TimestampMissing) => (
            CauseState::Refuse,
            Some("the upstream gives no date to wait for; ask an administrator".to_owned()),
        ),
        VerdictState::Quarantined => (
            CauseState::Wait,
            Some("`batlehub-cli wait` returns when it is served".to_owned()),
        ),
        VerdictState::Denied => (
            CauseState::Refuse,
            Some("use another version, or ask for a rescan".to_owned()),
        ),
    };
    Cause {
        kind: "verdict".into(),
        state,
        reason_code: code,
        until: v.available_at,
        detail: format!(
            "{}{}",
            v.state,
            if v.reason_codes.is_empty() {
                String::new()
            } else {
                format!(
                    " — {}",
                    v.reason_codes
                        .iter()
                        .map(ReasonCode::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        ),
        next,
    }
}

/// The metadata a local read judges, built as `read_gates` builds it.
fn local_metadata(id: &PackageId, pkg: &PublishedPackage) -> PackageMetadata {
    PackageMetadata {
        id: id.clone(),
        published_at: Some(pkg.published_at),
        download_url: None,
        checksum: Some(pkg.checksum.clone()),
        is_signed: Some(
            pkg.signature_bytes
                .as_deref()
                .is_some_and(|b| !b.is_empty()),
        ),
        extra: serde_json::Value::Null,
        cache_control: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(kind: &str, state: CauseState, until: Option<DateTime<Utc>>) -> Cause {
        Cause {
            kind: kind.into(),
            state,
            reason_code: None,
            until,
            detail: String::new(),
            next: None,
        }
    }

    #[test]
    fn the_headline_is_the_worst_cause() {
        let t = |s| {
            "2026-11-04T14:00:00Z"
                .parse::<DateTime<Utc>>()
                .ok()
                .map(|d| d + chrono::Duration::hours(s))
        };
        let p = finish(
            vec![
                c("access", CauseState::Pass, None),
                c("verdict", CauseState::Warn, None),
            ],
            "1",
        );
        assert_eq!(p.headline, Headline::Pullable);
        let p = finish(
            vec![
                c("verdict", CauseState::Wait, t(1)),
                c("policy", CauseState::Wait, t(5)),
            ],
            "1",
        );
        assert_eq!((p.headline, p.until), (Headline::Waiting, t(5)));
        let p = finish(
            vec![
                c("verdict", CauseState::Wait, None),
                c("policy", CauseState::Wait, t(5)),
            ],
            "1",
        );
        assert_eq!(
            (p.headline, p.until),
            (Headline::Waiting, None),
            "a scanner has no clock"
        );
        let p = finish(
            vec![
                c("verdict", CauseState::Wait, t(1)),
                c("block", CauseState::Refuse, None),
            ],
            "1",
        );
        assert_eq!(p.headline, Headline::Refused);
    }

    #[test]
    fn causes_come_in_the_servers_order() {
        let p = finish(
            vec![
                c("upstream", CauseState::Pass, None),
                c("verdict", CauseState::Pass, None),
                c("block", CauseState::Pass, None),
                c("access", CauseState::Pass, None),
            ],
            "1",
        );
        let kinds: Vec<&str> = p.causes.iter().map(|c| c.kind.as_str()).collect();
        assert_eq!(kinds, ["access", "block", "verdict", "upstream"]);
    }

    #[test]
    fn a_block_hides_the_administrators_note_from_a_non_admin() {
        let meta = local_metadata(
            &PackageId::new("r", "p", "1"),
            &PublishedPackage {
                registry: "r".into(),
                name: "p".into(),
                version: "1".into(),
                checksum: String::new(),
                yanked: false,
                deprecated: false,
                deprecation_message: None,
                unlisted: false,
                index_metadata: serde_json::Value::Null,
                published_at: Utc::now(),
                published_by: None,
                signature_bytes: None,
                signature_type: None,
                visibility: Default::default(),
                retention_keep: false,
            },
        );
        let deny = RuleDecision::Deny {
            reason: "internal: ticket SEC-42, vendor dispute".into(),
        };
        let user = rule_cause("block_list", &deny, None, &meta, false).unwrap();
        assert_eq!(user.detail, "blocked by an administrator");
        assert_eq!(user.reason_code.as_deref(), Some("BLOCK_LIST"));
        let admin = rule_cause("block_list", &deny, None, &meta, true).unwrap();
        assert!(admin.detail.contains("SEC-42"));
    }

    #[tokio::test]
    async fn evaluate_only_is_scoped() {
        assert!(!evaluate_only());
        assert!(scoped(async { evaluate_only() }).await);
        assert!(!evaluate_only());
    }
}
