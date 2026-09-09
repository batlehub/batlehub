//! Vulnerability flags pushed by a trusted source, and the exposure they
//! imply (RFC 0002, recast by §13 over RFC 0018).
//!
//! A [`PackageFlag`] is one assertion a SOC, a corporate vulnerability
//! platform or an advisory feed makes about a package version: *what* it
//! claims ([`FlagKind`]) and *how hard* it wants this proxy to react
//! ([`FlagEffect`]). On a registry with a `[security]` profile a live flag
//! becomes a `SocVerdict` finding in the version's verdict, judged with the
//! rest; on a registry without one it reaches `CveGateRule` as a recorded
//! vulnerability would.
//!
//! Exposure is the question the report answers: *who pulled a flagged
//! version, and was the flag already known when they did?* — a join of the
//! access log against the flags, grouped by consumer and coordinate.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{Finding, FindingKind, ReasonCode, Severity};

/// What a flag asserts. Open on purpose: a source's taxonomy is its own,
/// and an unknown kind is stored as it came rather than refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FlagKind {
    /// A published vulnerability (a CVE, a GHSA, an OSV id).
    Cve,
    /// A malicious release: an install hook, a credential stealer.
    Malware,
    /// A licence the estate does not accept.
    License,
    /// An internal policy: end of life, a vendor ban.
    Policy,
    /// Anything else, under the source's own name.
    #[serde(untagged)]
    Other(String),
}

impl FlagKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Cve => "cve",
            Self::Malware => "malware",
            Self::License => "license",
            Self::Policy => "policy",
            Self::Other(s) => s.as_str(),
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "cve" => Self::Cve,
            "malware" => Self::Malware,
            "license" | "licence" => Self::License,
            "policy" => Self::Policy,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// How hard a flag wants the proxy to react. Ordered: `HardBlock` is the
/// strongest, and a source's configured ceiling caps a pushed effect down.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FlagEffect {
    /// Visible in the report and the console; nothing else changes.
    Inform,
    /// As `inform`, and the version's verdict says `warned`.
    Warn,
    /// Judged by severity like a recorded vulnerability: the registry's
    /// threshold decides.
    Gate,
    /// Refused regardless of threshold or mode: the SOC's word.
    HardBlock,
}

impl FlagEffect {
    pub const ALL: [FlagEffect; 4] = [Self::Inform, Self::Warn, Self::Gate, Self::HardBlock];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Inform => "inform",
            Self::Warn => "warn",
            Self::Gate => "gate",
            Self::HardBlock => "hard_block",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "inform" => Some(Self::Inform),
            "warn" => Some(Self::Warn),
            "gate" => Some(Self::Gate),
            "hard_block" | "hard-block" | "block" => Some(Self::HardBlock),
            _ => None,
        }
    }
}

impl std::fmt::Display for FlagEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The version selector every flag carries: one exact version, or every
/// version of the package.
///
/// Ranges are deliberately absent (RFC 0002 §13): a range needs the
/// registry kind's ordering, and the version-scheme work is its own RFC. A
/// pushed `version_range` other than `*` is refused per item, not stored.
pub const ANY_VERSION: &str = "*";

/// The scanner name flags are recorded under, so a finding traces back to
/// the push that made it.
pub const FLAGS_SCANNER: &str = "flags";

/// One assertion about a package version, from one source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PackageFlag {
    pub id: Uuid,
    /// The `[[flag_sources]]` entry that pushed it.
    pub source: String,
    /// The source's own id for the flag; `(source, external_id)` is the
    /// identity a re-push updates and a revoke names.
    pub external_id: String,
    pub registry: String,
    pub package_name: String,
    /// An exact version, or [`ANY_VERSION`].
    pub version: String,
    pub kind: FlagKind,
    pub effect: FlagEffect,
    /// The severity a `gate` flag is judged at; absent, `High`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
    pub summary: String,
    /// Where the source says more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// When the source first told this proxy: the retroactive line of the
    /// exposure report.
    pub first_seen: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// After this instant the flag is dead without a revoke.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// The tombstone: a revoked flag stays for the report and stops judging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl PackageFlag {
    /// Alive at `now`: neither revoked nor expired.
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        self.revoked_at.is_none() && self.expires_at.is_none_or(|t| t > now)
    }

    /// Whether the flag covers `version`.
    pub fn covers(&self, version: &str) -> bool {
        self.version == ANY_VERSION || self.version == version
    }

    /// The severity a `gate` flag is judged at.
    pub fn gate_severity(&self) -> Severity {
        self.severity.unwrap_or(Severity::High)
    }

    /// The flag as the finding a `[security]` registry's verdict carries
    /// (RFC 0002 §13 decision 1): `hard_block` is `SOC_VERDICT`, denied
    /// under any policy; `gate` is a vulnerability at the pushed severity,
    /// which the threshold judges; `warn` and `inform` are below every
    /// threshold and recorded for the report.
    pub fn as_finding(&self) -> Finding {
        let (code, severity) = match self.effect {
            FlagEffect::HardBlock => (ReasonCode::SocVerdict, Severity::Critical),
            FlagEffect::Gate => (ReasonCode::Vulnerability, self.gate_severity()),
            FlagEffect::Warn | FlagEffect::Inform => (ReasonCode::Vulnerability, Severity::Low),
        };
        Finding::new(
            FLAGS_SCANNER,
            FindingKind::SocVerdict,
            code,
            severity,
            format!(
                "{} flagged by {}: {}",
                self.kind.as_str(),
                self.source,
                self.summary
            ),
        )
        .with_reference(format!("{}:{}", self.source, self.external_id))
        .with_raw(serde_json::json!({
            "source": self.source,
            "external_id": self.external_id,
            "effect": self.effect.as_str(),
            "kind": self.kind.as_str(),
            "url": self.url,
            "first_seen": self.first_seen,
        }))
    }
}

/// One item of a push, as the source sends it.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct FlagPush {
    pub external_id: String,
    pub registry: String,
    pub package_name: String,
    /// An exact version. Exactly one of `version` and `version_range`.
    #[serde(default)]
    pub version: Option<String>,
    /// Only `*` is accepted (every version); ranges wait for the
    /// version-scheme RFC.
    #[serde(default)]
    pub version_range: Option<String>,
    #[serde(default = "default_kind")]
    pub kind: FlagKind,
    pub effect: FlagEffect,
    #[serde(default)]
    pub severity: Option<Severity>,
    pub summary: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

fn default_kind() -> FlagKind {
    FlagKind::Other("unspecified".into())
}

/// What became of one pushed item.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FlagItemOutcome {
    Accepted {
        external_id: String,
        id: Uuid,
        /// `false` on an update of a flag already known.
        created: bool,
        /// The effect stored, after the source's ceiling was applied.
        effect: FlagEffect,
        /// The push asked for more than the source may do.
        effect_capped: bool,
    },
    Rejected {
        external_id: String,
        error: String,
    },
}

/// The answer to a push: one outcome per item, in order.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FlagPushResponse {
    pub accepted: u64,
    pub rejected: u64,
    pub items: Vec<FlagItemOutcome>,
}

/// The listing's filter.
#[derive(Debug, Clone, Default)]
pub struct FlagFilter {
    pub registry: Option<String>,
    pub package_name: Option<String>,
    pub source: Option<String>,
    pub effect: Option<FlagEffect>,
    /// Tombstones and expired flags too.
    pub include_dead: bool,
    pub limit: u64,
    pub offset: u64,
}

/// Which pulls the exposure report keeps, relative to the flag's
/// `first_seen`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExposureWhen {
    #[default]
    Any,
    /// Pulled before the flag was known: the retroactive case.
    BeforeFlag,
    /// Pulled while the flag was already live.
    AfterFlag,
}

/// The exposure question.
#[derive(Debug, Clone, Default)]
pub struct ExposureQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub registry: Option<String>,
    pub package_name: Option<String>,
    pub source: Option<String>,
    /// Keep flags at this effect or stronger.
    pub min_effect: Option<FlagEffect>,
    pub when: ExposureWhen,
    /// Keyset cursor: the last row of the previous page.
    pub after: Option<ExposureCursor>,
    pub limit: u64,
}

/// Where a page ended: rows sort by `last_pull DESC`, then by the row key,
/// and the cursor names the last one served.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExposureCursor {
    pub last_pull: DateTime<Utc>,
    pub consumer: String,
    pub registry: String,
    pub package_name: String,
    pub version: String,
    pub flag_id: Uuid,
}

impl ExposureCursor {
    /// URL-safe encoding for the `after` query parameter.
    pub fn encode(&self) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(self).unwrap_or_default())
    }

    pub fn decode(s: &str) -> Option<Self> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s.trim())
            .ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

/// One consumer × one coordinate × one flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExposureRow {
    /// The user id, or `anonymous`.
    pub consumer: String,
    pub consumer_role: String,
    pub registry: String,
    pub package_name: String,
    pub version: String,
    pub flag_id: Uuid,
    pub source: String,
    pub external_id: String,
    pub kind: FlagKind,
    pub effect: FlagEffect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
    pub summary: String,
    /// When the flag was first known here.
    pub flag_first_seen: DateTime<Utc>,
    /// Pulls in the window, and how many of them preceded the flag.
    pub pulls: u64,
    pub pulls_before_flag: u64,
    pub first_pull: DateTime<Utc>,
    pub last_pull: DateTime<Utc>,
}

impl ExposureRow {
    pub fn cursor(&self) -> ExposureCursor {
        ExposureCursor {
            last_pull: self.last_pull,
            consumer: self.consumer.clone(),
            registry: self.registry.clone(),
            package_name: self.package_name.clone(),
            version: self.version.clone(),
            flag_id: self.flag_id,
        }
    }
}

/// A page of the report.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct ExposurePage {
    pub rows: Vec<ExposureRow>,
    /// Present when another page follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// One source's standing, for the coverage block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FlagSourceCoverage {
    pub source: String,
    pub live_flags: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_push_at: Option<DateTime<Utc>>,
}

/// When the SBOM re-scan last covered a registry (RFC 0002 §4.6): a report
/// that cannot say when it last looked is not a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RegistryScanState {
    pub registry: String,
    pub last_scan_at: DateTime<Utc>,
    pub artifacts_scanned: u64,
    pub findings: u64,
    pub errors: u64,
}

/// What the report can and cannot see, printed beside its rows.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct ExposureCoverage {
    pub registries_total: u64,
    /// Registries with an SBOM extractor, so the CVE scan can cover them.
    pub sbom_configured: u64,
    /// Registries with a `[security]` profile, where a flag denies at once.
    pub security_profiles: u64,
    pub last_scan: Vec<RegistryScanState>,
    pub flag_sources: Vec<FlagSourceCoverage>,
    /// Events older than this are gone (audit retention), so a window that
    /// reaches past it is silently shorter than asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_truncated_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag(effect: FlagEffect, version: &str) -> PackageFlag {
        PackageFlag {
            id: Uuid::nil(),
            source: "soc".into(),
            external_id: "CASE-1".into(),
            registry: "npm".into(),
            package_name: "left-pad".into(),
            version: version.into(),
            kind: FlagKind::Malware,
            effect,
            severity: None,
            summary: "steals tokens".into(),
            url: None,
            first_seen: Utc::now(),
            updated_at: Utc::now(),
            expires_at: None,
            revoked_at: None,
        }
    }

    #[test]
    fn effects_order_from_inform_to_hard_block() {
        assert!(FlagEffect::HardBlock > FlagEffect::Gate);
        assert!(FlagEffect::Gate > FlagEffect::Warn);
        assert!(FlagEffect::Warn > FlagEffect::Inform);
        for e in FlagEffect::ALL {
            assert_eq!(FlagEffect::parse(e.as_str()), Some(e));
        }
        assert_eq!(FlagEffect::parse("nope"), None);
    }

    #[test]
    fn kind_is_open() {
        assert_eq!(FlagKind::parse("CVE"), FlagKind::Cve);
        assert_eq!(FlagKind::parse("licence"), FlagKind::License);
        assert_eq!(
            FlagKind::parse("eol"),
            FlagKind::Other("eol".into()),
            "an unknown kind is kept, not refused"
        );
        let json = serde_json::to_string(&FlagKind::Other("eol".into())).unwrap();
        assert_eq!(json, "\"eol\"");
        let back: FlagKind = serde_json::from_str("\"malware\"").unwrap();
        assert_eq!(back, FlagKind::Malware);
        let other: FlagKind = serde_json::from_str("\"eol\"").unwrap();
        assert_eq!(other, FlagKind::Other("eol".into()));
    }

    #[test]
    fn a_star_covers_every_version_and_an_exact_one_only_itself() {
        assert!(flag(FlagEffect::Warn, "*").covers("1.0.0"));
        assert!(flag(FlagEffect::Warn, "1.0.0").covers("1.0.0"));
        assert!(!flag(FlagEffect::Warn, "1.0.0").covers("1.0.1"));
    }

    #[test]
    fn liveness_follows_revocation_and_expiry() {
        let now = Utc::now();
        let mut f = flag(FlagEffect::Warn, "*");
        assert!(f.is_live(now));
        f.expires_at = Some(now - chrono::Duration::seconds(1));
        assert!(!f.is_live(now));
        f.expires_at = None;
        f.revoked_at = Some(now);
        assert!(!f.is_live(now));
    }

    #[test]
    fn a_hard_block_is_a_soc_verdict_and_a_gate_keeps_its_severity() {
        let hb = flag(FlagEffect::HardBlock, "*").as_finding();
        assert_eq!(hb.code, ReasonCode::SocVerdict);
        assert_eq!(hb.kind, FindingKind::SocVerdict);
        assert!(hb.code.is_always_denied());
        assert_eq!(hb.reference.as_deref(), Some("soc:CASE-1"));

        let mut g = flag(FlagEffect::Gate, "1.0.0");
        g.severity = Some(Severity::Medium);
        let f = g.as_finding();
        assert_eq!(f.code, ReasonCode::Vulnerability);
        assert_eq!(f.severity, Severity::Medium);

        let w = flag(FlagEffect::Warn, "1.0.0").as_finding();
        assert_eq!(w.severity, Severity::Low);
    }

    #[test]
    fn the_cursor_round_trips_through_its_encoding() {
        let c = ExposureCursor {
            last_pull: Utc::now(),
            consumer: "alice".into(),
            registry: "npm".into(),
            package_name: "@scope/pkg".into(),
            version: "1.0.0".into(),
            flag_id: Uuid::new_v4(),
        };
        assert_eq!(ExposureCursor::decode(&c.encode()), Some(c));
        assert_eq!(ExposureCursor::decode("not base64!"), None);
    }
}
