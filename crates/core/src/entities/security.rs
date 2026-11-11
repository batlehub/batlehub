//! The verdict model (RFC 0018 §4.2, §6.1): what is known about an artifact
//! before it is served, in a form a persisted row, a header and a CLI can all
//! carry.
//!
//! One decision replaces a chain of independent gates that each fail open on
//! their own error and cannot tell "not scanned" from "clean". A verdict is
//! computed from the *full* finding set — age, pending scans, scanner errors
//! and content findings alike — so the absence of a scan is a persisted
//! `SCAN_PENDING` hold rather than an empty list that reads as an allow.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{PackageId, RegistryKind, Severity};

/// What the proxy does with an artifact under this verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum VerdictState {
    /// Every required scanner returned, nothing at or above the threshold.
    Allowed,
    /// Served, with the reasons on the response: findings under `mode =
    /// "warn"`, or a scan still pending on a version past `mature_age_secs`.
    Warned,
    /// Held. Time-bound (`MIN_AGE_NOT_MET`, `SCAN_PENDING`, `SCANNER_ERROR`)
    /// lifts on its own and carries `available_at`; open-ended
    /// (`TIMESTAMP_MISSING`) does not.
    Quarantined,
    /// Terminal until a rescan or an administrator acts.
    Denied,
}

impl VerdictState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Warned => "warned",
            Self::Quarantined => "quarantined",
            Self::Denied => "denied",
        }
    }

    /// Whether an artifact under this state is streamed.
    pub fn is_served(&self) -> bool {
        matches!(self, Self::Allowed | Self::Warned)
    }

    /// Precedence: `denied` > `quarantined` > `warned` > `allowed`.
    fn rank(&self) -> u8 {
        match self {
            Self::Allowed => 0,
            Self::Warned => 1,
            Self::Quarantined => 2,
            Self::Denied => 3,
        }
    }

    fn worse(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

impl std::str::FromStr for VerdictState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "allowed" => Ok(Self::Allowed),
            "warned" => Ok(Self::Warned),
            "quarantined" => Ok(Self::Quarantined),
            "denied" => Ok(Self::Denied),
            other => Err(format!("unknown verdict state '{other}'")),
        }
    }
}

impl std::fmt::Display for VerdictState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The closed set of reasons a verdict can carry (RFC 0018 §4.2 — this list
/// is the master; RFC 0019 adds its ref-level codes here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReasonCode {
    MinAgeNotMet,
    TimestampMissing,
    ScanPending,
    ScannerError,
    ScannerUnsupported,
    Vulnerability,
    MalwareSignal,
    InstallHook,
    TyposquatSuspect,
    PublisherChanged,
    InstallHookAdded,
    RepositoryMoved,
    DormantRelease,
    UnpublishedUpstream,
    ProvenanceMissing,
    ProvenanceInvalid,
    ProvenanceUnverifiable,
    ProvenanceRemoved,
    SignatureMissing,
    LicenseDenied,
    UntrustedPublisher,
    BlockList,
    SocVerdict,
    AdminOverride,
    MutableRef,
    TagMoved,
    AssetReplaced,
    PinnedRefRequired,
    RawScript,
    /// An advisory database names the version as malicious (an OSV `MAL-`
    /// record). Always denied. RFC 0041 decision 33: release N reads it and
    /// never writes it, so the release that does is readable by the one
    /// before it.
    KnownMalicious,
    /// A stored code this release does not know — written by a newer one.
    /// Always denied, never emitted, and outside [`ReasonCode::ALL`]: a read
    /// that met one answers a refusal instead of an error, and the row is
    /// left for the release that wrote it (RFC 0041 decision 33).
    Unrecognised,
}

impl ReasonCode {
    /// Every code, for the drift tests and the wire-string table.
    pub const ALL: &[ReasonCode] = &[
        Self::MinAgeNotMet,
        Self::TimestampMissing,
        Self::ScanPending,
        Self::ScannerError,
        Self::ScannerUnsupported,
        Self::Vulnerability,
        Self::MalwareSignal,
        Self::InstallHook,
        Self::TyposquatSuspect,
        Self::PublisherChanged,
        Self::InstallHookAdded,
        Self::RepositoryMoved,
        Self::DormantRelease,
        Self::UnpublishedUpstream,
        Self::ProvenanceMissing,
        Self::ProvenanceInvalid,
        Self::ProvenanceUnverifiable,
        Self::ProvenanceRemoved,
        Self::SignatureMissing,
        Self::LicenseDenied,
        Self::UntrustedPublisher,
        Self::BlockList,
        Self::SocVerdict,
        Self::AdminOverride,
        Self::MutableRef,
        Self::TagMoved,
        Self::AssetReplaced,
        Self::PinnedRefRequired,
        Self::RawScript,
        Self::KnownMalicious,
    ];

    /// The SCREAMING_SNAKE wire form.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MinAgeNotMet => "MIN_AGE_NOT_MET",
            Self::TimestampMissing => "TIMESTAMP_MISSING",
            Self::ScanPending => "SCAN_PENDING",
            Self::ScannerError => "SCANNER_ERROR",
            Self::ScannerUnsupported => "SCANNER_UNSUPPORTED",
            Self::Vulnerability => "VULNERABILITY",
            Self::MalwareSignal => "MALWARE_SIGNAL",
            Self::InstallHook => "INSTALL_HOOK",
            Self::TyposquatSuspect => "TYPOSQUAT_SUSPECT",
            Self::PublisherChanged => "PUBLISHER_CHANGED",
            Self::InstallHookAdded => "INSTALL_HOOK_ADDED",
            Self::RepositoryMoved => "REPOSITORY_MOVED",
            Self::DormantRelease => "DORMANT_RELEASE",
            Self::UnpublishedUpstream => "UNPUBLISHED_UPSTREAM",
            Self::ProvenanceMissing => "PROVENANCE_MISSING",
            Self::ProvenanceInvalid => "PROVENANCE_INVALID",
            Self::ProvenanceUnverifiable => "PROVENANCE_UNVERIFIABLE",
            Self::ProvenanceRemoved => "PROVENANCE_REMOVED",
            Self::SignatureMissing => "SIGNATURE_MISSING",
            Self::LicenseDenied => "LICENSE_DENIED",
            Self::UntrustedPublisher => "UNTRUSTED_PUBLISHER",
            Self::BlockList => "BLOCK_LIST",
            Self::SocVerdict => "SOC_VERDICT",
            Self::AdminOverride => "ADMIN_OVERRIDE",
            Self::MutableRef => "MUTABLE_REF",
            Self::TagMoved => "TAG_MOVED",
            Self::AssetReplaced => "ASSET_REPLACED",
            Self::PinnedRefRequired => "PINNED_REF_REQUIRED",
            Self::RawScript => "RAW_SCRIPT",
            Self::KnownMalicious => "KNOWN_MALICIOUS",
            Self::Unrecognised => "UNRECOGNISED",
        }
    }

    /// A hold that lifts on its own: it carries an `available_at` (age) or
    /// ends when a scanner answers (pending, error).
    pub fn is_time_bound(&self) -> bool {
        matches!(
            self,
            Self::MinAgeNotMet | Self::ScanPending | Self::ScannerError
        )
    }

    /// `denied` regardless of `mode`: an administrator's or the SOC's word.
    pub fn is_always_denied(&self) -> bool {
        matches!(
            self,
            Self::BlockList | Self::SocVerdict | Self::KnownMalicious | Self::Unrecognised
        )
    }

    /// A stored wire form, read leniently: a code this release does not know
    /// is [`ReasonCode::Unrecognised`] with a `WARN`, never a read error — an
    /// error would fail every download and every listing of the version, and
    /// nothing would repair the row (RFC 0041 decision 33).
    pub fn parse_stored(s: &str) -> Self {
        s.parse().unwrap_or_else(|e| {
            tracing::warn!(code = s, error = %e, "security: stored reason code not known to this release; read as UNRECOGNISED");
            Self::Unrecognised
        })
    }

    /// The codes the maturity bypass may downgrade to `warned`: a version
    /// older than `mature_age_secs` is served while these are all it carries.
    pub fn is_maturity_bypassable(&self) -> bool {
        matches!(self, Self::ScanPending | Self::ScannerError)
    }
}

/// The page that explains every reason code to the person whose install was
/// refused. The CLI prints a link per code; the console links each badge.
pub const REASON_CODES_DOCS: &str = "https://batlehub.dev/use/refused-downloads";

/// The link for one wire-form code: the page, anchored at the code's section.
/// Takes the wire string rather than a [`ReasonCode`] so a client older than
/// the server still links a code it cannot parse — to the right page, at
/// worst to the top of it. The anchors are gated by
/// `every_reason_code_has_a_section_on_the_refusals_page`.
pub fn reason_code_docs_url(wire: &str) -> String {
    format!(
        "{REASON_CODES_DOCS}#{}",
        wire.to_ascii_lowercase().replace('_', "-")
    )
}

impl std::str::FromStr for ReasonCode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .find(|c| c.as_str() == s)
            .copied()
            .ok_or_else(|| format!("unknown reason code '{s}'"))
    }
}

impl std::fmt::Display for ReasonCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What kind of thing a finding is — the axis escalation and the console
/// group on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    Vulnerability,
    MalwareSignal,
    InstallHook,
    Provenance,
    Signature,
    License,
    Publisher,
    Typosquat,
    Transition,
    Age,
    /// A required scanner has not answered yet. Not in the RFC's list, which
    /// folded it into `ScannerError`; separate because "nobody looked yet"
    /// and "somebody looked and crashed" are different holds with different
    /// metrics.
    Pending,
    SocVerdict,
    ScannerError,
    /// A `ScannerError` from a scanner that is an advisory database (RFC 0041
    /// decision 44): the maturity bypass does not lift it on a version the
    /// database never answered for.
    AdvisoryDbError,
    BlockList,
    Ref,
}

/// The name the block-list gate emits findings under.
///
/// Two writers put a `BlockList` finding on a verdict — `BlockListScanner` on
/// every scan, and `AdminService::block_package` immediately when the operator
/// blocks — and each has to recognise the other's finding to replace it rather
/// than duplicate it. One constant is what keeps them agreeing.
pub const BLOCK_LIST_SCANNER: &str = "block_list";

impl FindingKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Vulnerability => "vulnerability",
            Self::MalwareSignal => "malware_signal",
            Self::InstallHook => "install_hook",
            Self::Provenance => "provenance",
            Self::Signature => "signature",
            Self::License => "license",
            Self::Publisher => "publisher",
            Self::Typosquat => "typosquat",
            Self::Transition => "transition",
            Self::Age => "age",
            Self::Pending => "pending",
            Self::SocVerdict => "soc_verdict",
            Self::ScannerError => "scanner_error",
            Self::AdvisoryDbError => "advisory_db_error",
            Self::BlockList => "block_list",
            Self::Ref => "ref",
        }
    }
}

impl FindingKind {
    /// The severity an adapter gives a finding of this kind when the
    /// scanner's output states none (RFC 0041 §4.5). Exhaustive, so a new
    /// kind cannot be added without deciding it. A scanner that states a
    /// severity is never overridden by this.
    pub fn default_severity(&self) -> Severity {
        match self {
            Self::MalwareSignal | Self::InstallHook => Severity::High,
            Self::Typosquat | Self::Transition => Severity::Medium,
            Self::Vulnerability
            | Self::Provenance
            | Self::Signature
            | Self::License
            | Self::Publisher
            | Self::Age
            | Self::Pending
            | Self::SocVerdict
            | Self::ScannerError
            | Self::AdvisoryDbError
            | Self::BlockList
            | Self::Ref => Severity::Unknown,
        }
    }
}

impl std::str::FromStr for FindingKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "vulnerability" => Self::Vulnerability,
            "malware_signal" => Self::MalwareSignal,
            "install_hook" => Self::InstallHook,
            "provenance" => Self::Provenance,
            "signature" => Self::Signature,
            "license" => Self::License,
            "publisher" => Self::Publisher,
            "typosquat" => Self::Typosquat,
            "transition" => Self::Transition,
            "age" => Self::Age,
            "pending" => Self::Pending,
            "soc_verdict" => Self::SocVerdict,
            "scanner_error" => Self::ScannerError,
            "advisory_db_error" => Self::AdvisoryDbError,
            "block_list" => Self::BlockList,
            "ref" => Self::Ref,
            other => return Err(format!("unknown finding kind '{other}'")),
        })
    }
}

/// One thing one scanner said about one artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Finding {
    /// The scanner's name (`osv`, `age`, `block_list`, `cve_gate`, …).
    pub scanner: String,
    pub kind: FindingKind,
    pub code: ReasonCode,
    pub severity: Severity,
    /// A CVE id, an OSV id, a SOC case id — whatever the scanner cites.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<u8>,
    /// For a time-bound hold: when it lifts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_at: Option<DateTime<Utc>>,
    /// The scanner's own output, untouched. Hostile data: never interpolated.
    #[serde(default)]
    pub raw: serde_json::Value,
    /// The level an unrated finding was judged at, under the registry's
    /// `unknown_severity` (RFC 0041 §4.5). Recomputed by every evaluation,
    /// never stored: `severity` keeps what the scanner said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judged_as: Option<Severity>,
}

/// The prefix of an OSV malicious-package record.
const MALICIOUS_PREFIX: &str = "MAL-";

impl Finding {
    /// A finding with only the fields every one has.
    pub fn new(
        scanner: impl Into<String>,
        kind: FindingKind,
        code: ReasonCode,
        severity: Severity,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            scanner: scanner.into(),
            kind,
            code,
            severity,
            reference: None,
            summary: summary.into(),
            confidence: None,
            available_at: None,
            raw: serde_json::Value::Null,
            judged_as: None,
        }
    }

    /// Whether this finding cites a known-malicious record (OSV `MAL-`), by
    /// its reference or by an id or alias in its raw record. The one place
    /// `MAL-` is recognised (RFC 0041 §6.8): a finding stored before the
    /// worker emitted `KNOWN_MALICIOUS` is refused on read by this.
    pub fn names_malicious(&self) -> bool {
        let mal = |s: &str| s.starts_with(MALICIOUS_PREFIX);
        if self.reference.as_deref().is_some_and(mal) {
            return true;
        }
        let raw_names = |v: &serde_json::Value| {
            v.get("id").and_then(|i| i.as_str()).is_some_and(mal)
                || v.get("aliases")
                    .and_then(|a| a.as_array())
                    .is_some_and(|a| a.iter().filter_map(|x| x.as_str()).any(mal))
        };
        raw_names(&self.raw)
    }

    pub fn with_reference(mut self, reference: impl Into<String>) -> Self {
        self.reference = Some(reference.into());
        self
    }

    pub fn with_raw(mut self, raw: serde_json::Value) -> Self {
        self.raw = raw;
        self
    }
}

/// The persisted decision about one version (RFC 0018 §6.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Verdict {
    pub package: PackageId,
    pub state: VerdictState,
    pub reason_codes: Vec<ReasonCode>,
    pub findings: Vec<Finding>,
    /// `<registry>/<profile>` — which policy judged it.
    pub policy_ref: String,
    /// When a time-bound hold lifts, if this verdict is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_at: Option<DateTime<Utc>>,
    pub evaluated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_scanned_at: Option<DateTime<Utc>>,
    /// The scanners that have answered for this version, so "pending" is
    /// derived rather than guessed on every read.
    #[serde(default)]
    pub scanners_done: Vec<String>,
}

impl Verdict {
    pub fn is_served(&self) -> bool {
        self.state.is_served()
    }

    /// The one-line message every native error body carries (RFC 0018 §4.2):
    /// `<registry>/<name>@<version> is <state> (<CODES>[, available <RFC3339>]).
    /// Run `batlehub-cli why <coord>` for details.` — the coordinate in the
    /// form the CLI reads (RFC 0040 §4.1), and the binary's real name.
    pub fn short_message(&self) -> String {
        let codes = self
            .reason_codes
            .iter()
            .map(ReasonCode::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let coord = format!(
            "{}/{}@{}",
            self.package.registry, self.package.name, self.package.version
        );
        let available = self
            .available_at
            .map(|at| {
                format!(
                    ", available {}",
                    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                )
            })
            .unwrap_or_default();
        format!(
            "{coord} is {} ({codes}{available}). Run `batlehub-cli why {coord}` for details.",
            self.state
        )
    }

    /// Whether this verdict keeps its version out of the listings a client
    /// resolves against (RFC 0018 §4.2 *Listings*): a `denied` or
    /// `quarantined` version is hidden by the registry's own block mechanism,
    /// except a time-bound hold whose clock has already run out — the next
    /// read re-derives that one as served, and a listing must not lag it.
    pub fn hides_from_listings(&self, now: DateTime<Utc>) -> bool {
        self.hides_from_listings_under(now, SecurityMode::Block)
    }

    /// [`Self::hides_from_listings`] judged under the registry's *current*
    /// `mode`, which the stored row may predate.
    ///
    /// A verdict is re-judged on the artifact path on every read
    /// (`VerdictService::current`), so a `denied` judged under `block` is
    /// served `warned` the moment the registry flips to `warn` — but the
    /// listing filter reads the stored rows, and a stored `denied` hid the
    /// version from every fresh resolve, which therefore never reached the
    /// artifact path that would have re-judged it. Measured by
    /// `tests/heavy/quarantine.sh` step 5: after the flip, `npm install`
    /// answered ETARGET forever. Under `warn` the only `denied` the evaluator
    /// can produce is one carrying an always-denied code (`BLOCK_LIST`,
    /// `SOC_VERDICT`); any other stored `denied` is stale and served.
    ///
    /// The other direction — a stored `warned` under a registry now in
    /// `block` — is left to the artifact path: the listing names the
    /// version, the gate refuses it with the finding, and the refusal
    /// re-judges the row so the next listing agrees. That lag is one request
    /// long and errs on the side the gate corrects.
    pub fn hides_from_listings_under(&self, now: DateTime<Utc>, mode: SecurityMode) -> bool {
        match self.state {
            VerdictState::Allowed | VerdictState::Warned => false,
            VerdictState::Denied => match mode {
                SecurityMode::Block => true,
                SecurityMode::Warn => self.reason_codes.iter().any(ReasonCode::is_always_denied),
            },
            VerdictState::Quarantined => {
                let lifted = !self.reason_codes.is_empty()
                    && self.reason_codes.iter().all(ReasonCode::is_time_bound)
                    && self.available_at.is_some_and(|at| at <= now);
                !lifted
            }
        }
    }

    /// `Retry-After` in seconds, when waiting can help: a hold whose every
    /// code is time-bound and which names an `available_at`.
    pub fn retry_after_secs(&self, now: DateTime<Utc>) -> Option<u64> {
        if self.state != VerdictState::Quarantined
            || !self.reason_codes.iter().all(ReasonCode::is_time_bound)
        {
            return None;
        }
        let at = self.available_at?;
        Some((at - now).num_seconds().max(1) as u64)
    }
}

/// `mode` — what findings at or above the threshold do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecurityMode {
    Block,
    Warn,
}

/// `scanner_error` — what a scanner that crashed or timed out does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScannerErrorMode {
    Quarantine,
    Warn,
    Ignore,
}

/// `deny_install_hooks` — what an install hook finding does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallHookMode {
    Deny,
    Warn,
    Ignore,
}

/// A scanner's escalation block (RFC 0018 §4.2): `count` findings of `kinds`
/// at or above `from`, from the same scanner, are raised to `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escalation {
    pub kinds: Vec<FindingKind>,
    pub count: usize,
    pub from: Severity,
    pub to: Severity,
}

/// One registry's `[registries.security]` profile, as the evaluator reads it.
#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    pub mode: SecurityMode,
    /// Never below one hour; validation enforces the floor.
    pub min_age: Duration,
    /// Zero means "never serve unscanned".
    pub mature_age: Duration,
    pub hold_missing_timestamp: bool,
    pub scanners: Vec<String>,
    pub required_scanners: Vec<String>,
    pub max_severity: Severity,
    /// What an unrated (`unknown`) finding is judged at against
    /// `max_severity` (RFC 0041 §4.5). Default `high`; `unknown` restores
    /// the old "an unrated finding never acts".
    pub unknown_severity: Severity,
    pub require_provenance: bool,
    pub deny_install_hooks: InstallHookMode,
    pub scanner_error: ScannerErrorMode,
    /// Per scanner, by name.
    pub escalation: HashMap<String, Escalation>,
    /// What the verdict cites as its policy: `<registry>/default`.
    pub policy_ref: String,
    /// `[registries.security.rescan] interval_secs` (RFC 0018 phase 4):
    /// a served verdict older than this is scanned again. `None` never
    /// rescans on a clock — a webhook or an admin still can.
    pub rescan_interval: Option<Duration>,
    /// How far back the flip alert and the `pullers` report look
    /// (`pullers_window_days`, decision 23). Default thirty days.
    pub pullers_window: Duration,
}

impl SecurityPolicy {
    /// The profile `[registries.security]` with only `mode` written gives
    /// (RFC 0018 §4.1): a day of age, OSV required, everything else default.
    pub fn defaults_for(registry: &str) -> Self {
        Self {
            mode: SecurityMode::Block,
            min_age: Duration::from_secs(86_400),
            mature_age: Duration::from_secs(86_400),
            hold_missing_timestamp: true,
            scanners: vec!["osv".to_owned()],
            required_scanners: vec!["osv".to_owned()],
            max_severity: Severity::High,
            unknown_severity: Severity::High,
            require_provenance: false,
            deny_install_hooks: InstallHookMode::Warn,
            scanner_error: ScannerErrorMode::Quarantine,
            escalation: HashMap::new(),
            policy_ref: format!("{registry}/default"),
            rescan_interval: None,
            pullers_window: Duration::from_secs(30 * 86_400),
        }
    }
}

/// Why a scan job exists, in priority order (RFC 0018 §4.2 *Queue
/// priority*): a user is waiting on `FirstSeen`, nobody on `Backfill`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanTrigger {
    FirstSeen,
    Webhook,
    Rescan,
    Backfill,
}

impl ScanTrigger {
    /// Lower is dequeued first.
    pub fn priority(&self) -> i16 {
        match self {
            Self::FirstSeen => 0,
            Self::Webhook => 1,
            Self::Rescan => 2,
            Self::Backfill => 3,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FirstSeen => "first_seen",
            Self::Webhook => "webhook",
            Self::Rescan => "rescan",
            Self::Backfill => "backfill",
        }
    }
}

impl std::str::FromStr for ScanTrigger {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "first_seen" => Ok(Self::FirstSeen),
            "webhook" => Ok(Self::Webhook),
            "rescan" => Ok(Self::Rescan),
            "backfill" => Ok(Self::Backfill),
            other => Err(format!("unknown scan trigger '{other}'")),
        }
    }
}

/// One unit of scan work (RFC 0018 §6.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanJob {
    pub id: uuid::Uuid,
    pub package: PackageId,
    /// The upstream's publish date at enqueue time, so the worker can judge
    /// age without re-resolving metadata.
    pub published_at: Option<DateTime<Utc>>,
    pub artifact_sha256: Option<String>,
    pub trigger: ScanTrigger,
    pub attempts: u32,
    pub leased_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// When the job was first deferred on an advisory database's transient
    /// error (RFC 0041 decision 44): the anchor of the 24-hour budget,
    /// surviving a worker restart.
    #[serde(default)]
    pub deferred_since: Option<DateTime<Utc>>,
    /// The worker holding the lease. `None` on a job never leased, released
    /// by a failure, or deferred — which is how a deferral is told from a
    /// running scan whose `leased_until` is also in the future (RFC 0044 §4.2
    /// rule 2).
    #[serde(default)]
    pub leased_by: Option<String>,
    /// The column as stored: an attempt's error, the `deferred since ` anchor,
    /// or, on a closed row, [`EXHAUSTED_PREFIX`] and the last error.
    #[serde(default)]
    pub last_error: Option<String>,
    /// When the row was closed; `None` while it is open.
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
}

/// Which rows the Scans page lists (RFC 0044 §4.4 `/admin/scans?state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanJobFilter {
    /// Every open job: queued and scanning.
    Open,
    Queued,
    Scanning,
    /// A coordinate whose latest closed job ran out of attempts
    /// ([`EXHAUSTED_PREFIX`]) and that has no open job (§4.2 rule 5).
    Failed,
}

/// What `/admin/findings` reads (RFC 0044 §6.1). `registries` is a ceiling
/// the caller's grants computed: the store never reads outside it, and an
/// empty one lists nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindingsFilter {
    pub registries: Vec<String>,
    /// At or above this severity, by rank (critical → unknown), not by the
    /// text of the column.
    pub min_severity: Option<Severity>,
    pub scanner: Option<String>,
    /// A prefix of `reference`, matched literally (`%` and `_` are not
    /// wildcards).
    pub reference_prefix: Option<String>,
    pub verdict: Option<VerdictState>,
    /// Order by reference first, so one advisory's versions sit together.
    pub by_reference: bool,
}

/// One finding with its coordinate and the verdict it fed.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingRow {
    pub package: PackageId,
    pub finding: Finding,
    pub verdict: VerdictState,
}

/// What `close_exhausted` writes before the last error of a job whose
/// attempts ran out (RFC 0044 decision 34). The web process reads it to tell a
/// failed scan from a finished one: an exhausted `rescan` writes no
/// `SCANNER_ERROR`, and `max_attempts` is a worker setting it may not share.
pub const EXHAUSTED_PREFIX: &str = "exhausted: ";

/// Where one version stands in scanning (RFC 0044 §4.2), derived by
/// `services::scan_status::derive` and nowhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScanState {
    NotScanned,
    Queued,
    Scanning,
    Failed,
    Scanned,
}

/// The `findings:read` tier of a scan status: when, how many attempts, and
/// the error. Never the worker id, which is `system:read`'s (§9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ScanDetail {
    pub last_scanned_at: Option<DateTime<Utc>>,
    /// The open job's attempts, else the latest closed one's.
    pub attempts: Option<u32>,
    /// The open job's `created_at`.
    pub queued_since: Option<DateTime<Utc>>,
    pub deferred_since: Option<DateTime<Utc>>,
    /// Raw as stored; the web layer redacts and truncates it before it leaves.
    /// Never the `deferred since ` anchor, which `deferred_since` carries.
    pub last_error: Option<String>,
}

/// One version's scan status before the visibility tier is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanStatus {
    pub state: ScanState,
    /// Set only when `state` is `Scanned`.
    pub verdict: Option<VerdictState>,
    pub detail: ScanDetail,
}

/// The package URL a coordinate is looked up under, per registry kind.
///
/// The same table `services::sbom` uses for the SBOM's main component, kept
/// in step by the test beside it: two spellings of one PURL would be two
/// different OSV answers for one artifact.
pub fn coordinate_purl(kind: RegistryKind, name: &str, version: &str) -> String {
    match kind {
        RegistryKind::Cargo => format!("pkg:cargo/{name}@{version}"),
        RegistryKind::Npm => format!("pkg:npm/{name}@{version}"),
        RegistryKind::Maven => format!("pkg:maven/{}@{version}", name.replacen(':', "/", 1)),
        RegistryKind::Pypi => format!("pkg:pypi/{name}@{version}"),
        RegistryKind::Rubygems => format!("pkg:gem/{name}@{version}"),
        RegistryKind::Goproxy => format!("pkg:golang/{name}@{version}"),
        RegistryKind::Composer => format!("pkg:composer/{name}@{version}"),
        RegistryKind::Conda => format!("pkg:conda/{name}@{version}"),
        RegistryKind::Nuget => format!("pkg:nuget/{name}@{version}"),
        RegistryKind::Github | RegistryKind::Forgejo | RegistryKind::Gitlab => {
            format!("pkg:{}/{name}@{version}", kind.as_str())
        }
        _ => format!("pkg:generic/{name}@{version}"),
    }
}

/// The worst of two states.
pub fn worse_state(a: VerdictState, b: VerdictState) -> VerdictState {
    a.worse(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_order_as_the_precedence_rule_says() {
        use VerdictState::*;
        assert_eq!(worse_state(Allowed, Warned), Warned);
        assert_eq!(worse_state(Warned, Quarantined), Quarantined);
        assert_eq!(worse_state(Denied, Quarantined), Denied);
        assert_eq!(worse_state(Allowed, Allowed), Allowed);
        assert!(Warned.is_served() && Allowed.is_served());
        assert!(!Quarantined.is_served() && !Denied.is_served());
    }

    #[test]
    fn every_reason_code_round_trips_its_wire_form() {
        for code in ReasonCode::ALL {
            assert_eq!(code.as_str().parse::<ReasonCode>().unwrap(), *code);
            assert_eq!(
                serde_json::to_string(code).unwrap(),
                format!("\"{}\"", code.as_str())
            );
        }
        assert_eq!(ReasonCode::ALL.len(), 30);
        assert!(!ReasonCode::ALL.contains(&ReasonCode::Unrecognised));
    }

    #[test]
    fn a_stored_code_this_release_does_not_know_is_unrecognised_and_denied() {
        assert_eq!(
            ReasonCode::parse_stored("KNOWN_MALICIOUS"),
            ReasonCode::KnownMalicious
        );
        let unknown = ReasonCode::parse_stored("SOMETHING_NEWER");
        assert_eq!(unknown, ReasonCode::Unrecognised);
        assert!(unknown.is_always_denied() && ReasonCode::KnownMalicious.is_always_denied());
        assert_eq!(serde_json::to_string(&unknown).unwrap(), "\"UNRECOGNISED\"");
    }

    /// Every code the CLI and the console link to has its section, in both
    /// languages: a code added without one is a link that lands at the top of
    /// a page that does not mention it.
    #[test]
    fn every_reason_code_has_a_section_on_the_refusals_page() {
        let pages = [
            (
                "en",
                include_str!("../../../../docs/use/refused-downloads.md"),
            ),
            (
                "fr",
                include_str!("../../../../docs/fr/use/refused-downloads.md"),
            ),
        ];
        for (lang, page) in pages {
            // `UNRECOGNISED` is outside `ALL` (nothing emits it) but a reader
            // can still be refused with it, so it has its section too.
            for code in ReasonCode::ALL.iter().chain([&ReasonCode::Unrecognised]) {
                let url = reason_code_docs_url(code.as_str());
                let anchor = url.rsplit('#').next().unwrap();
                assert!(
                    page.contains(&format!("{{#{anchor}}}")),
                    "{lang}: docs/use/refused-downloads.md has no `{{#{anchor}}}` section for {code}"
                );
            }
        }
        assert_eq!(
            reason_code_docs_url("MIN_AGE_NOT_MET"),
            "https://batlehub.dev/use/refused-downloads#min-age-not-met"
        );
    }

    #[test]
    fn the_short_message_is_the_one_line_every_client_prints() {
        let v = Verdict {
            package: PackageId::new("npm-public", "left-pad", "1.3.1"),
            state: VerdictState::Quarantined,
            reason_codes: vec![ReasonCode::MinAgeNotMet],
            findings: vec![],
            policy_ref: "npm-public/default".into(),
            available_at: Some("2026-09-05T14:12:00Z".parse().unwrap()),
            evaluated_at: Utc::now(),
            last_scanned_at: None,
            scanners_done: vec![],
        };
        assert_eq!(
            v.short_message(),
            "npm-public/left-pad@1.3.1 is quarantined (MIN_AGE_NOT_MET, available \
             2026-09-05T14:12:00Z). Run `batlehub-cli why npm-public/left-pad@1.3.1` for details."
        );
        let now: DateTime<Utc> = "2026-09-05T13:12:00Z".parse().unwrap();
        assert_eq!(v.retry_after_secs(now), Some(3600));
    }

    #[test]
    fn retry_after_is_only_for_holds_that_waiting_lifts() {
        let mut v = Verdict {
            package: PackageId::new("r", "p", "1"),
            state: VerdictState::Denied,
            reason_codes: vec![ReasonCode::BlockList],
            findings: vec![],
            policy_ref: "r/default".into(),
            available_at: None,
            evaluated_at: Utc::now(),
            last_scanned_at: None,
            scanners_done: vec![],
        };
        assert_eq!(v.retry_after_secs(Utc::now()), None);
        v.state = VerdictState::Quarantined;
        v.reason_codes = vec![ReasonCode::TimestampMissing];
        assert_eq!(v.retry_after_secs(Utc::now()), None, "open-ended: no clock");
    }

    #[test]
    fn purls_match_the_sbom_generator_and_maven_uses_a_slash() {
        assert_eq!(
            coordinate_purl(RegistryKind::Npm, "left-pad", "1.3.1"),
            "pkg:npm/left-pad@1.3.1"
        );
        assert_eq!(
            coordinate_purl(RegistryKind::Rubygems, "rails", "7.0.0"),
            "pkg:gem/rails@7.0.0"
        );
        assert_eq!(
            coordinate_purl(RegistryKind::Maven, "com.acme:lib", "1.0"),
            "pkg:maven/com.acme/lib@1.0"
        );
        assert_eq!(
            coordinate_purl(RegistryKind::Generic, "x", "1"),
            "pkg:generic/x@1"
        );
    }
}
