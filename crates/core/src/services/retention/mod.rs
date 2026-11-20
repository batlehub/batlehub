//! Reclaiming the bytes of locally published versions nobody is using
//! (RFC 0016, phases 3–4).
//!
//! # Why this shares no code with `services::eviction`
//!
//! The two look alike enough that the first implementation instinct is to widen
//! `EvictionConfig` to reach local rows. That instinct is the one thing this
//! module exists to resist:
//!
//! | | Cache eviction | Retention |
//! | --- | --- | --- |
//! | Governs | proxy-cached artifacts | locally published versions |
//! | Another copy exists | yes, upstream | **frequently not** |
//! | Cost of a wrong reclaim | a re-fetch | the artifact |
//! | Default | configured per registry | keep everything, forever |
//!
//! That asymmetry sets every default here. A shared implementation would inherit
//! eviction's defaults, its reporting and its silence, and would apply a policy
//! calibrated for recoverable data to data that is not (RFC 0016 §5.1).
//!
//! # Keep conditions are a union of vetoes
//!
//! **A version survives if *any* configured condition matches.** There is no
//! expression to evaluate and no ordering to get wrong: the only way to reclaim
//! a version is for every configured condition to decline. Wrong configuration
//! therefore fails toward keeping, which is the direction that is recoverable.
//!
//! # Retention reclaims bytes, not names
//!
//! A reclaimed version leaves a tombstone and its number can never be taken
//! again. Freeing disk must not free the *namespace*, or retention becomes a
//! supply-chain mechanism by accident — which is why this calls the same
//! `LocalRegistryService::delete_version` a human deletion goes through rather
//! than reaching into the backend itself. It asks for one thing to be different:
//! the reclamation is audited as `retention_reclaim`, not `delete`, so the trail
//! can tell a policy apart from a person (RFC 0016 §3).
//!
//! # What a run leaves behind
//!
//! | | Live run | Dry run |
//! | --- | --- | --- |
//! | Registry-scoped run event | `retention_run` | `retention_dry_run` |
//! | Per-version event | `retention_reclaim` | none |
//! | Tombstone | one per version | none |
//!
//! A `retention_reclaim` event therefore always means a version is gone, and the
//! report is the only place a dry run's *coordinates* exist — deliberately, since
//! rows saying a version was reclaimed when it was not are worse than no rows.

mod report;
pub use report::{KeepReason, RetentionDecision, RetentionReport};

use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::entities::{AccessAction, Identity, PublishedPackage};
use crate::error::CoreError;
use crate::ports::PackageRepository;
use crate::services::LocalRegistryService;

/// The date before which an absent download record proves nothing, when the
/// operator has not said otherwise (RFC 0016 §4.3).
///
/// 2026-08-27, the day after the survey remediation that gave the Maven and
/// NuGet local artifact paths a download event at all. Before it the audit trail
/// is silent for those ecosystems, and a retention run that read the silence as
/// disuse would reclaim versions the estate was using every day.
///
/// A constant rather than a config default because it is a fact about this
/// software's history, not a preference: every instance upgraded through that
/// version has the same gap in the same place.
pub const DEFAULT_DOWNLOAD_SIGNAL_FLOOR: &str = "2026-08-27T00:00:00Z";

/// How many per-version decisions one report carries.
///
/// A registry with two million versions must not serialise two million rows into
/// one response. What is dropped is *counted* and reported, because a bounded
/// list that does not say it is bounded reads as "this is everything".
pub const MAX_REPORTED_DECISIONS: usize = 2_000;

/// The resolved keep conditions for one registry, as plain data.
///
/// Mirrors the config block, with the days already turned into instants against
/// a single `now` — so every version in one run is judged against the same
/// clock, and a sweep that takes ten minutes cannot decide two identically-aged
/// versions differently.
#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    pub keep_versions: Option<u32>,
    pub keep_for: Option<Duration>,
    pub keep_if_pulled: Option<Duration>,
    pub keep_yanked: bool,
    pub download_signal_floor: DateTime<Utc>,
    pub reclaim_delay: Duration,
    pub dry_run: bool,
}

impl RetentionPolicy {
    /// Whether any reclamation condition is configured — i.e. whether a run
    /// would do anything at all. `keep_yanked` does not count: it only ever
    /// vetoes.
    pub fn reclaims_anything(&self) -> bool {
        self.keep_versions.is_some() || self.keep_for.is_some() || self.keep_if_pulled.is_some()
    }
}

/// Drives retention over one registry's locally published versions.
pub struct RetentionService {
    pub local: Arc<LocalRegistryService>,
    /// The download signal `keep_if_pulled` reads. `None` disables that
    /// condition entirely — and, because the conditions are a union of vetoes,
    /// disabling a veto makes the policy *more* aggressive. So a run with a
    /// configured `keep_if_pulled` and no repository refuses rather than
    /// silently reclaiming what it cannot prove is idle.
    pub packages: Option<Arc<dyn PackageRepository>>,
}

/// One version and everything the decision needs, gathered before any of the
/// conditions are asked.
struct Candidate<'a> {
    pkg: &'a PublishedPackage,
    /// Index from the newest by publish date among non-yanked versions: 0 is
    /// the newest. Yanked versions get `usize::MAX` and never use up a slot.
    rank_from_newest: usize,
    /// The highest stable, non-yanked version by version order. Kept whenever
    /// `keep_versions` is set, so a backport published later can't push it out.
    newest_stable: bool,
    last_download: Option<DateTime<Utc>>,
}

impl RetentionService {
    pub fn new(
        local: Arc<LocalRegistryService>,
        packages: Option<Arc<dyn PackageRepository>>,
    ) -> Self {
        Self { local, packages }
    }

    /// Run retention over every locally published version in `registry`.
    ///
    /// Under `policy.dry_run` nothing is written and the report says what would
    /// have happened. That is the default and the only mode an operator should
    /// meet first.
    pub async fn run(
        &self,
        registry: &str,
        policy: &RetentionPolicy,
        identity: &Identity,
    ) -> Result<RetentionReport, CoreError> {
        if policy.keep_if_pulled.is_some() && self.packages.is_none() {
            return Err(CoreError::Config(format!(
                "registry '{registry}': keep_if_pulled_days is configured but this deployment has \
                 no package repository, so there is no download signal to read. Retention would \
                 reclaim versions it cannot prove are idle; refusing to run"
            )));
        }

        let mut report = RetentionReport {
            dry_run: policy.dry_run,
            ..Default::default()
        };

        // No keep condition means nothing is reclaimable. Reported as an
        // ordinary run over the whole registry rather than short-circuited, so
        // the operator sees the version count they are protecting and a
        // `no_policy` reason against it, instead of an empty answer they have to
        // interpret.
        let names = self.local.backend.list_package_names(registry).await?;
        let now = Utc::now();

        for name in names {
            let versions = self.local.backend.get_versions(registry, &name).await?;
            if versions.is_empty() {
                continue;
            }
            let downloads = self.last_downloads(registry, &name).await?;
            let doomed = Self::judge(&versions, &downloads, policy, now, &mut report);

            // A `Break` means one delete failed. The report is returned as it
            // stands rather than propagated as an error — see `reclaim`.
            if self
                .reclaim(registry, &doomed, policy, identity, &mut report)
                .await
                .is_break()
            {
                break;
            }
        }

        report.reclaimed_coordinates.sort();
        self.record_run(registry, policy, &report, identity).await;
        Ok(report)
    }

    /// Record the run itself, registry-scoped, one event.
    ///
    /// # Why a dry run is audited here and nowhere else
    ///
    /// `compact_tombstone_detail` declines to audit its own preview, on the
    /// grounds that "an audit trail that records reads is a trail nobody
    /// finishes reading". That reasoning holds for the *versions*: this writes
    /// no `retention_reclaim` event for a version it merely considered, and an
    /// auditor can trust that every one of those means a version is actually
    /// gone.
    ///
    /// It does not hold for the run. Pointing a reclamation policy at a
    /// production registry is an operator's action against that registry — the
    /// probe whose answer decides whether `dry_run` comes off next — not a read
    /// of package data, and it costs one row. The alternative is that the only
    /// runs with any trace are the ones that happened to delete something,
    /// which makes "has anyone been testing this policy against prod" a
    /// question the trail cannot answer.
    ///
    /// The mode is carried by the *action*
    /// ([`AccessAction::RetentionRun`] vs [`AccessAction::RetentionDryRun`])
    /// rather than by a flag, because `AccessEvent` has nowhere to put a flag
    /// and because an auditor filtering `?action=retention_run` must get
    /// exactly the runs that were allowed to write.
    ///
    /// The counts are deliberately not in the event — there is no field for
    /// them, and inventing one to hold a summary would put a number in the
    /// audit trail that nothing else can check. They go to the log line beside
    /// it, and the reclaimed coordinates are in the trail one row each.
    async fn record_run(
        &self,
        registry: &str,
        policy: &RetentionPolicy,
        report: &RetentionReport,
        identity: &Identity,
    ) {
        let action = if policy.dry_run {
            AccessAction::RetentionDryRun
        } else {
            AccessAction::RetentionRun
        };
        tracing::info!(
            registry,
            dry_run = policy.dry_run,
            examined = report.examined,
            kept = report.kept,
            reclaimed = report.reclaimed,
            incomplete = report.incomplete_because.is_some(),
            user_id = identity.user_id.as_deref().unwrap_or(""),
            "retention run finished"
        );
        self.local
            .record_registry_action(registry, action, identity)
            .await;
    }

    /// Score every version of one package: record a decision against each, and
    /// return the ones no condition kept.
    ///
    /// Split out of [`Self::run`] so the judging pass and the destructive pass
    /// are separately readable — and separately reviewable, which matters more
    /// here than usual, because everything this returns is about to be deleted.
    fn judge<'a>(
        versions: &'a [PublishedPackage],
        downloads: &[(String, DateTime<Utc>)],
        policy: &RetentionPolicy,
        now: DateTime<Utc>,
        report: &mut RetentionReport,
    ) -> Vec<&'a PublishedPackage> {
        // `get_versions` is `published_at ASC`; rank 0 must be the newest.
        // Yanked rows are not installable, so they don't count toward the N.
        let live = versions.iter().filter(|p| !p.yanked).count();
        let newest_stable = versions
            .iter()
            .filter(|p| !p.yanked && !crate::services::version_order::is_prerelease(&p.version))
            .map(|p| p.version.as_str())
            .min_by(|a, b| crate::services::version_order::newest_first(a, b));
        let mut doomed: Vec<&PublishedPackage> = Vec::new();
        let mut live_seen = 0usize;

        for pkg in versions {
            let rank_from_newest = if pkg.yanked {
                usize::MAX
            } else {
                live_seen += 1;
                live - live_seen
            };
            let candidate = Candidate {
                pkg,
                rank_from_newest,
                newest_stable: newest_stable == Some(pkg.version.as_str()),
                last_download: downloads
                    .iter()
                    .find(|(v, _)| *v == pkg.version)
                    .map(|(_, t)| *t),
            };
            report.examined += 1;
            let decision = Self::decide(&candidate, policy, now);
            if decision.is_none() {
                doomed.push(pkg);
            } else {
                report.kept += 1;
            }
            if report.decisions.len() < MAX_REPORTED_DECISIONS {
                report.decisions.push(RetentionDecision {
                    name: pkg.name.clone(),
                    version: pkg.version.clone(),
                    kept_because: decision,
                });
            } else {
                report.decisions_truncated += 1;
            }
        }

        doomed
    }

    /// Reclaim the versions `judge` condemned.
    ///
    /// [`ControlFlow::Break`] means a delete failed and the sweep must stop;
    /// `report.incomplete_because` carries the reason and the coordinate it
    /// stopped at. Not a `Result`, because there is nothing for the caller to
    /// propagate: **the report is the return value even on failure.** By the time
    /// one delete fails the run has already reclaimed real versions, and an error
    /// would throw away the only record of which ones, leaving an operator to
    /// reconstruct it from the audit log. Nor does it continue: a failed delete
    /// is a storage or database fault, and grinding through 200 000 more packages
    /// against a broken backend turns one fault into a very long one.
    async fn reclaim(
        &self,
        registry: &str,
        doomed: &[&PublishedPackage],
        policy: &RetentionPolicy,
        identity: &Identity,
        report: &mut RetentionReport,
    ) -> ControlFlow<()> {
        for pkg in doomed {
            if policy.dry_run {
                report.reclaimed += 1;
                report
                    .reclaimed_coordinates
                    .push(format!("{}@{}", pkg.name, pkg.version));
                continue;
            }
            // `judge` read a snapshot, and a live run can last minutes: a pin set
            // since then must still win, so re-read the row right before deleting.
            match self.local.backend.get_versions(registry, &pkg.name).await {
                Ok(now)
                    if now
                        .iter()
                        .any(|p| p.version == pkg.version && p.retention_keep) =>
                {
                    report.kept += 1;
                    if let Some(d) = report
                        .decisions
                        .iter_mut()
                        .find(|d| d.name == pkg.name && d.version == pkg.version)
                    {
                        d.kept_because = Some(KeepReason::Pinned);
                    }
                    continue;
                }
                Ok(_) => {}
                Err(e) => {
                    report.incomplete_because = Some(format!(
                        "stopped at {}@{}: {e}. Everything listed above was reclaimed; \
                         nothing after it was attempted.",
                        pkg.name, pkg.version
                    ));
                    return ControlFlow::Break(());
                }
            }
            // The same call a human deletion goes through, so a reclaimed
            // version leaves the same tombstone and drops the same bytes — and
            // is audited as `retention_reclaim` rather than `delete`, which is
            // what tells the two apart in the trail. The run carries the
            // operator's own identity, so the subject alone never could.
            match self
                .local
                .delete_version_audited(
                    registry,
                    &pkg.name,
                    &pkg.version,
                    identity,
                    AccessAction::RetentionReclaim,
                )
                .await
            {
                // Nothing tombstoned: someone else deleted it first. Not ours to count.
                Ok(false) => {}
                Ok(true) => {
                    report.reclaimed += 1;
                    report
                        .reclaimed_coordinates
                        .push(format!("{}@{}", pkg.name, pkg.version));
                }
                Err(e) => {
                    report.incomplete_because = Some(format!(
                        "stopped at {}@{}: {e}. Everything listed above was reclaimed; \
                         nothing after it was attempted.",
                        pkg.name, pkg.version
                    ));
                    return ControlFlow::Break(());
                }
            }
            if !policy.reclaim_delay.is_zero() {
                tokio::time::sleep(policy.reclaim_delay).await;
            }
        }

        ControlFlow::Continue(())
    }

    /// **The union of vetoes.** `Some(reason)` keeps, `None` reclaims.
    ///
    /// Ordered so the cheapest and most absolute conditions come first, and so
    /// the reported reason is the most informative one when several apply: an
    /// operator told a version survived because it is *pinned* learns more than
    /// one told it survived because it is recent, when both are true.
    ///
    /// The floor-date check sits **last** on purpose. It is the one that turns
    /// *absence of evidence* into a keep, and putting it after the positive
    /// conditions makes it obvious that it can only ever add survivors
    /// (RFC 0016 §5.1).
    fn decide(
        candidate: &Candidate<'_>,
        policy: &RetentionPolicy,
        now: DateTime<Utc>,
    ) -> Option<KeepReason> {
        // An inert policy keeps everything. Checked first so a registry with a
        // block that configures only compaction never reclaims by accident.
        if !policy.reclaims_anything() {
            return Some(KeepReason::NoPolicy);
        }

        // The version-tier pin outranks every policy above it.
        if candidate.pkg.retention_keep {
            return Some(KeepReason::Pinned);
        }

        // Each condition is one flat question. An unconfigured condition never
        // fires, so `is_some_and` reads exactly as the policy does: "keep it if
        // the operator asked for this and it holds".
        if policy.keep_versions.is_some_and(|n| {
            n > 0 && (candidate.newest_stable || candidate.rank_from_newest < n as usize)
        }) {
            return Some(KeepReason::KeepVersions);
        }

        if policy
            .keep_for
            .is_some_and(|window| within(candidate.pkg.published_at, window, now))
        {
            return Some(KeepReason::KeepFor);
        }

        // Both halves are required: a window with no download record cannot
        // keep, which is what the signal-floor check below exists to catch.
        if policy
            .keep_if_pulled
            .zip(candidate.last_download)
            .is_some_and(|(window, last)| within(last, window, now))
        {
            return Some(KeepReason::KeepIfPulled);
        }

        if policy.keep_yanked && candidate.pkg.yanked {
            return Some(KeepReason::KeepYanked);
        }

        // Absence of evidence, last. A version with no download record that was
        // published before the floor might have been pulled every day of its
        // life without anything writing it down.
        //
        // Only consulted when `keep_if_pulled` is configured: with no download
        // condition in the policy, the signal is not being read at all and its
        // gaps are nobody's business.
        if policy.keep_if_pulled.is_some()
            && candidate.last_download.is_none()
            && candidate.pkg.published_at < policy.download_signal_floor
        {
            return Some(KeepReason::BeforeSignalFloor);
        }

        None
    }

    /// The download signal for one package, or an empty list when the policy
    /// does not read it.
    ///
    /// Skipped rather than fetched-and-ignored when `keep_if_pulled` is unset:
    /// this is a query per package over a table with the whole estate's access
    /// history in it, and a policy that does not consult the signal should not
    /// pay for it.
    async fn last_downloads(
        &self,
        registry: &str,
        name: &str,
    ) -> Result<Vec<(String, DateTime<Utc>)>, CoreError> {
        let Some(ref repo) = self.packages else {
            return Ok(vec![]);
        };
        repo.last_downloads(registry, name).await
    }
}

/// Whether `then` is within `window` of `now`.
///
/// Saturating rather than signed: a `published_at` in the future — a clock skew,
/// a restored backup — is "within" every window rather than outside all of them,
/// which keeps rather than reclaims.
fn within(then: DateTime<Utc>, window: Duration, now: DateTime<Utc>) -> bool {
    let Ok(window) = chrono::Duration::from_std(window) else {
        // An absurd window (>292 years) is not a reason to reclaim.
        return true;
    };
    then >= now - window
}

#[cfg(test)]
mod tests;
