//! Pushed vulnerability flags (RFC 0002 §4.3–§4.5, recast by §13): a
//! trusted source's assertions accepted, capped to what the source may do,
//! stored, and applied to the verdicts they change.
//!
//! # What a push changes, and when
//!
//! On a registry **with** a `[security]` profile the flag is a finding of
//! the version's verdict (decision 1): the `flags` internal scanner emits
//! it on every scan, so the durable path is a `Webhook`-priority rescan of
//! each affected coordinate. A `hard_block` does not wait for the worker:
//! the stored verdict, when there is one, is denied here and now — it is
//! the SOC's word, and seconds matter on a malware flag. The rescan that
//! follows re-derives the same denial from the scanner, so the two agree.
//! A version never seen has no verdict to touch and is judged at first
//! sight, when the scanner reads the flag.
//!
//! On a registry **without** one, [`crate::rules::FlagsRule`] reads the
//! store on every request; there is nothing to update.
//!
//! # Rate and size
//!
//! A batch is at most [`MAX_BATCH`] items, and a source has
//! `max_flags_per_minute`; over it the whole batch is refused with a
//! `Retry-After`, so a runaway feed cannot rewrite the estate's verdicts
//! faster than anyone can read the log.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::entities::{
    FlagEffect, FlagItemOutcome, FlagPush, FlagPushResponse, PackageFlag, PackageId, ReasonCode,
    ScanTrigger, Verdict, VerdictState, ANY_VERSION, FLAGS_SCANNER,
};
use crate::error::CoreError;
use crate::ports::AdvisoryRepository;
use crate::services::hot_config::HotConfigLock;
use crate::services::verdict::coordinate_key;

/// The most items one push may carry.
pub const MAX_BATCH: usize = 1000;

/// What `[[flag_sources]]` says a source may do — the config crate's entry,
/// as core reads it.
#[derive(Debug, Clone)]
pub struct FlagSourceLimits {
    pub name: String,
    /// A pushed effect stronger than this is stored at this.
    pub max_effect: FlagEffect,
    /// Registries the source may flag; empty means any.
    pub registries: Vec<String>,
    pub max_flags_per_minute: u32,
}

#[derive(Debug)]
pub enum FlagPushError {
    /// The batch has more than [`MAX_BATCH`] items.
    TooMany {
        max: usize,
    },
    /// The source is over its per-minute budget.
    RateLimited {
        retry_after_secs: u64,
    },
    /// One item is malformed: an outcome of that item, never of the batch.
    Item(String),
    Core(CoreError),
}

impl From<CoreError> for FlagPushError {
    fn from(e: CoreError) -> Self {
        Self::Core(e)
    }
}

impl std::fmt::Display for FlagPushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooMany { max } => write!(f, "a push carries at most {max} flags"),
            Self::RateLimited { retry_after_secs } => {
                write!(
                    f,
                    "over the source's per-minute budget; retry in {retry_after_secs}s"
                )
            }
            Self::Item(e) => f.write_str(e),
            Self::Core(e) => write!(f, "{e}"),
        }
    }
}

pub struct FlagService {
    pub repo: Arc<dyn AdvisoryRepository>,
    pub hot: HotConfigLock,
    /// Per source: the minute (Unix minutes) and the items accepted in it.
    budget: Mutex<HashMap<String, (i64, u32)>>,
}

impl FlagService {
    pub fn new(repo: Arc<dyn AdvisoryRepository>, hot: HotConfigLock) -> Self {
        Self {
            repo,
            hot,
            budget: Mutex::new(HashMap::new()),
        }
    }

    /// Charge `n` items to `source`'s minute; `Err(retry_after)` over budget.
    fn charge(&self, source: &FlagSourceLimits, n: u32, now: DateTime<Utc>) -> Result<(), u64> {
        if source.max_flags_per_minute == 0 {
            return Ok(());
        }
        let minute = now.timestamp().div_euclid(60);
        let mut budget = self.budget.lock().unwrap_or_else(|p| p.into_inner());
        let entry = budget.entry(source.name.clone()).or_insert((minute, 0));
        if entry.0 != minute {
            *entry = (minute, 0);
        }
        if entry.1.saturating_add(n) > source.max_flags_per_minute {
            let retry = 60 - now.timestamp().rem_euclid(60);
            return Err(retry as u64);
        }
        entry.1 += n;
        Ok(())
    }

    /// Accept a batch from `source`: per-item validation and outcomes, the
    /// batch as a whole refused only for size or rate.
    pub async fn push(
        &self,
        source: &FlagSourceLimits,
        items: Vec<FlagPush>,
        now: DateTime<Utc>,
    ) -> Result<FlagPushResponse, FlagPushError> {
        if items.len() > MAX_BATCH {
            return Err(FlagPushError::TooMany { max: MAX_BATCH });
        }
        if let Err(retry_after_secs) = self.charge(source, items.len() as u32, now) {
            return Err(FlagPushError::RateLimited { retry_after_secs });
        }
        let known_registries: Vec<String> = {
            let hot = self.hot.read().await;
            hot.registries.keys().cloned().collect()
        };
        let mut out = FlagPushResponse {
            accepted: 0,
            rejected: 0,
            items: Vec::with_capacity(items.len()),
        };
        for item in items {
            let external_id = item.external_id.clone();
            match self.accept_one(source, item, &known_registries, now).await {
                Ok(outcome) => {
                    out.accepted += 1;
                    out.items.push(outcome);
                }
                Err(FlagPushError::Item(error)) => {
                    out.rejected += 1;
                    out.items
                        .push(FlagItemOutcome::Rejected { external_id, error });
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    async fn accept_one(
        &self,
        source: &FlagSourceLimits,
        item: FlagPush,
        known_registries: &[String],
        now: DateTime<Utc>,
    ) -> Result<FlagItemOutcome, FlagPushError> {
        let reject = FlagPushError::Item;
        let external_id = item.external_id.trim().to_owned();
        if external_id.is_empty() || external_id.len() > 200 {
            return Err(reject("external_id must be 1–200 characters".into()));
        }
        if !known_registries.iter().any(|r| r == &item.registry) {
            return Err(reject(format!("unknown registry '{}'", item.registry)));
        }
        if !source.registries.is_empty() && !source.registries.contains(&item.registry) {
            return Err(reject(format!(
                "source '{}' may not flag registry '{}'",
                source.name, item.registry
            )));
        }
        crate::services::validate_package_name(&item.package_name)
            .map_err(|e| reject(format!("package_name: {e}")))?;
        let version = match (item.version.as_deref(), item.version_range.as_deref()) {
            (Some(v), None) => {
                crate::services::validate_coordinate(&item.package_name, v, None)
                    .map_err(|e| reject(format!("version: {e}")))?;
                v.to_owned()
            }
            (None, Some(r)) if r.trim() == ANY_VERSION => ANY_VERSION.to_owned(),
            (None, Some(r)) => {
                return Err(reject(format!(
                    "version_range '{r}' is not accepted: only '*' (every version) is, \
                     ranges wait for the version-scheme RFC (RFC 0002 §13)"
                )))
            }
            _ => {
                return Err(reject(
                    "exactly one of version and version_range is required".into(),
                ))
            }
        };
        let summary = item.summary.trim().to_owned();
        if summary.is_empty() || summary.len() > 2000 {
            return Err(reject("summary must be 1–2000 characters".into()));
        }
        if let Some(exp) = item.expires_at {
            if exp <= now {
                return Err(reject("expires_at is in the past".into()));
            }
        }
        let effect_capped = item.effect > source.max_effect;
        let effect = item.effect.min(source.max_effect);
        let flag = PackageFlag {
            id: Uuid::new_v4(),
            source: source.name.clone(),
            external_id: external_id.clone(),
            registry: item.registry,
            package_name: item.package_name,
            version,
            kind: item.kind,
            effect,
            severity: item.severity,
            summary,
            url: item.url.filter(|u| !u.trim().is_empty()),
            first_seen: now,
            updated_at: now,
            expires_at: item.expires_at,
            revoked_at: None,
        };
        let (id, created) = self.repo.upsert_flag(flag.clone()).await?;
        // A cap is not a rejection, but it is a fact the source must see:
        // it asked for a block and got a warning.
        self.apply(&flag, now).await;
        Ok(FlagItemOutcome::Accepted {
            external_id,
            id,
            created,
            effect,
            effect_capped,
        })
    }

    /// Tombstone `(source, external_id)` and re-judge what it covered.
    pub async fn revoke(
        &self,
        source: &str,
        external_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, CoreError> {
        let Some(flag) = self.repo.get_flag(source, external_id).await? else {
            return Ok(false);
        };
        if !self.repo.revoke_flag(source, external_id, now).await? {
            return Ok(false);
        }
        self.rescan_covered(&flag).await;
        Ok(true)
    }

    /// The coordinates a flag covers that this instance has a verdict for.
    async fn covered_verdicts(&self, flag: &PackageFlag) -> Vec<Verdict> {
        let verdicts = {
            let hot = self.hot.read().await;
            if !hot.security.contains_key(&flag.registry) {
                return Vec::new();
            }
            hot.verdicts.clone()
        };
        let Some(verdicts) = verdicts else {
            return Vec::new();
        };
        let found = if flag.version == ANY_VERSION {
            verdicts
                .list_for_package(&flag.registry, &flag.package_name)
                .await
        } else {
            verdicts
                .get(&PackageId::new(
                    &flag.registry,
                    &flag.package_name,
                    &flag.version,
                ))
                .await
                .map(|v| v.into_iter().collect())
        };
        match found {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(source = %flag.source, external_id = %flag.external_id, error = %e,
                    "flags: verdict store unreadable, the rescan will catch up");
                Vec::new()
            }
        }
    }

    /// Queue a `Webhook`-priority rescan of every covered coordinate with a
    /// verdict. A flag on `*` and a version never seen: nothing to queue,
    /// the first sight reads the flag.
    async fn rescan_covered(&self, flag: &PackageFlag) {
        let queue = {
            let hot = self.hot.read().await;
            hot.scan_queue.clone()
        };
        let Some(queue) = queue else { return };
        for v in self.covered_verdicts(flag).await {
            if let Err(e) = queue.enqueue(&v.package, None, ScanTrigger::Webhook).await {
                tracing::warn!(package = %v.package, error = %e, "flags: could not queue rescan");
            }
        }
    }

    /// What a stored flag does to the verdicts it covers.
    async fn apply(&self, flag: &PackageFlag, now: DateTime<Utc>) {
        if flag.effect == FlagEffect::HardBlock && flag.is_live(now) {
            let verdicts = {
                let hot = self.hot.read().await;
                hot.verdicts.clone()
            };
            if let Some(store) = verdicts {
                for mut v in self.covered_verdicts(flag).await {
                    v.findings.retain(|f| {
                        f.scanner != FLAGS_SCANNER || f.reference != Some(reference(flag))
                    });
                    v.findings.push(flag.as_finding());
                    v.state = VerdictState::Denied;
                    if !v.reason_codes.contains(&ReasonCode::SocVerdict) {
                        v.reason_codes.push(ReasonCode::SocVerdict);
                    }
                    v.available_at = None;
                    v.evaluated_at = now;
                    if let Err(e) = store.upsert(&v).await {
                        tracing::warn!(package = %v.package, error = %e, "flags: could not deny verdict");
                    } else {
                        tracing::info!(package = %coordinate_key(&v.package), source = %flag.source,
                            external_id = %flag.external_id, "security: denied by pushed flag");
                    }
                }
            }
        }
        self.rescan_covered(flag).await;
    }
}

fn reference(flag: &PackageFlag) -> String {
    format!("{}:{}", flag.source, flag.external_id)
}
