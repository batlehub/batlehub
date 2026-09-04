//! Pushed flags on a registry **without** a `[security]` profile (RFC 0002
//! §13 decision 1): the one place a flag is judged when there is no verdict
//! to carry it.
//!
//! A `hard_block` flag denies outright, whatever the gate settings — it is
//! the SOC's word, the same standing `BlockListRule` gives an administrator's
//! block. A `gate` flag is judged like a recorded vulnerability: against the
//! registry's `cve_gate` threshold when one is configured, else `high`, and
//! only when that gate blocks. `warn` and `inform` never deny here; they are
//! in the report and the console.
//!
//! Named `flags`, so a `GateExemption` on that gate silences it for one
//! version (`EXEMPTIBLE_GATES`) — the recast's replacement for the suppress
//! endpoint RFC 0002 first designed.
//!
//! **Fails open** on a store error, as `BlockListRule` and `CveGateRule` do
//! on this path: on a registry that chose no quarantine, a database blip
//! must not turn the proxy into a wall. A registry that wants fail-closed
//! opts into `[security]`, where the same flags become findings and a
//! store error is `SCANNER_ERROR`.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use crate::entities::{FlagEffect, Severity};
use crate::ports::AdvisoryRepository;
use crate::rules::{Rule, RuleContext, RuleDecision};

pub const FLAGS_GATE: &str = "flags";

pub struct FlagsRule {
    pub repo: Arc<dyn AdvisoryRepository>,
    /// The threshold a `gate` flag is judged at.
    pub gate_min_severity: Severity,
    /// Whether a `gate` flag over the threshold denies (the `cve_gate`
    /// rule's `block`, when one is configured; else `true`).
    pub gate_blocks: bool,
}

impl FlagsRule {
    pub fn new(repo: Arc<dyn AdvisoryRepository>) -> Self {
        Self {
            repo,
            gate_min_severity: Severity::High,
            gate_blocks: true,
        }
    }

    pub fn with_gate(mut self, min_severity: Severity, blocks: bool) -> Self {
        self.gate_min_severity = min_severity;
        self.gate_blocks = blocks;
        self
    }
}

#[async_trait]
impl Rule for FlagsRule {
    fn name(&self) -> &str {
        FLAGS_GATE
    }

    async fn evaluate(&self, ctx: &RuleContext<'_>) -> RuleDecision {
        let id = &ctx.package.id;
        let now = Utc::now();
        let flags = match self
            .repo
            .live_flags_for_package(&id.registry, &id.name, now)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(package = %id, error = %e, "FlagsRule: flag store unreadable, failing open");
                return RuleDecision::Allow;
            }
        };
        let covering: Vec<_> = flags
            .iter()
            .filter(|f| f.is_live(now) && f.covers(&id.version))
            .collect();
        if let Some(f) = covering.iter().find(|f| f.effect == FlagEffect::HardBlock) {
            return RuleDecision::Deny {
                reason: format!(
                    "blocked: {} flagged by {} ({}): {}",
                    f.kind.as_str(),
                    f.source,
                    f.external_id,
                    f.summary
                ),
            };
        }
        if !self.gate_blocks {
            return RuleDecision::Allow;
        }
        let worst = covering
            .iter()
            .filter(|f| f.effect == FlagEffect::Gate)
            .filter(|f| f.gate_severity() >= self.gate_min_severity)
            .max_by_key(|f| f.gate_severity());
        match worst {
            Some(f) => RuleDecision::Deny {
                reason: format!(
                    "blocked: {} {} flagged by {} ({}) (minimum gated severity: {})",
                    f.gate_severity().as_str(),
                    f.kind.as_str(),
                    f.source,
                    f.external_id,
                    self.gate_min_severity.as_str(),
                ),
            },
            None => RuleDecision::Allow,
        }
    }
}
