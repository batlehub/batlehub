//! The supply-chain layer's configuration (RFC 0018 §4.1): a registry's
//! `[registries.security]` profile, the global `[scanners]` table,
//! `[server].roles` and `[worker]`.
//!
//! Every key is optional and absent means "as before": a registry without
//! the section keeps its gates as rules, a process without `roles` is both
//! proxy and worker, and `CURRENT_CONFIG_VERSION` does not move.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use batlehub_core::entities::{
    Escalation, FindingKind, InstallHookMode, ScannerErrorMode, SecurityMode, SecurityPolicy,
    Severity,
};

/// The floor under `min_age_secs`: one hour is the point of the quarantine,
/// and degrading it silently defeats it (RFC 0018 §4.3).
pub const MIN_AGE_FLOOR_SECS: u64 = 3600;

/// `[registries.security]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecurityConfig {
    /// `"block"` (default) or `"warn"`.
    #[serde(default = "default_mode")]
    pub mode: SecurityMode,
    /// Never served below this age. Default one day; floor one hour.
    #[serde(default = "default_day")]
    pub min_age_secs: u64,
    /// Served `warned` while a scan is still pending above this age. Default
    /// one day, which with the default `min_age_secs` leaves the scan-hold
    /// window empty (§4.1 says so). `0` never serves unscanned.
    #[serde(default = "default_day")]
    pub mature_age_secs: u64,
    /// Hold a version the upstream did not date. Default true: the gate's
    /// `deny_missing_timestamp` under the verdict's name, defaulted the
    /// other way.
    #[serde(default = "default_true")]
    pub hold_missing_timestamp: bool,
    /// Names from `[scanners]`. Default `["osv"]`.
    #[serde(default = "default_osv")]
    pub scanners: Vec<String>,
    /// Must all answer before serving. Default `["osv"]`.
    #[serde(default = "default_osv")]
    pub required_scanners: Vec<String>,
    /// Findings at or above this severity deny (`block`) or warn.
    #[serde(default = "default_max_severity")]
    pub max_severity: String,
    #[serde(default)]
    pub require_provenance: bool,
    #[serde(default = "default_install_hooks")]
    pub deny_install_hooks: InstallHookMode,
    #[serde(default = "default_scanner_error")]
    pub scanner_error: ScannerErrorMode,
    #[serde(default)]
    pub rescan: Option<SecurityRescanConfig>,
}

impl SecurityConfig {
    /// The profile as the evaluator reads it, for registry `name`.
    ///
    /// `max_severity` is validated before this is called; an unparseable one
    /// falls to `high` here rather than panicking, which validation makes
    /// unreachable.
    pub fn to_policy(
        &self,
        name: &str,
        scanners: &HashMap<String, ScannerConfig>,
    ) -> SecurityPolicy {
        let mut escalation = HashMap::new();
        for scanner in &self.scanners {
            if let Some(esc) = scanners.get(scanner).and_then(|c| c.escalation()) {
                escalation.insert(scanner.clone(), esc.to_escalation());
            }
        }
        SecurityPolicy {
            mode: self.mode,
            min_age: Duration::from_secs(self.min_age_secs),
            mature_age: Duration::from_secs(self.mature_age_secs),
            hold_missing_timestamp: self.hold_missing_timestamp,
            scanners: self.scanners.clone(),
            required_scanners: self.required_scanners.clone(),
            max_severity: Severity::parse(&self.max_severity).unwrap_or(Severity::High),
            require_provenance: self.require_provenance,
            deny_install_hooks: self.deny_install_hooks,
            scanner_error: self.scanner_error,
            escalation,
            policy_ref: format!("{name}/default"),
        }
    }
}

/// `[registries.security.rescan]` — phase 4's knob, parsed now so the
/// section round-trips; nothing reads it until the rescan worker lands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SecurityRescanConfig {
    #[serde(default)]
    pub interval_secs: u64,
    #[serde(default = "default_true")]
    pub on_webhook: bool,
}

/// `[scanners.<name>.escalation]` (RFC 0018 §4.2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EscalationConfig {
    pub kinds: Vec<FindingKind>,
    pub count: usize,
    pub from: String,
    pub to: String,
}

impl EscalationConfig {
    pub fn to_escalation(&self) -> Escalation {
        Escalation {
            kinds: self.kinds.clone(),
            count: self.count,
            from: Severity::parse(&self.from).unwrap_or(Severity::Medium),
            to: Severity::parse(&self.to).unwrap_or(Severity::High),
        }
    }
}

/// `[scanners.<name>]`, tagged by `type`.
///
/// Every scanner the RFC names parses, so a config written for the full set
/// round-trips; which ones a build can *run* is [`ScannerConfig::available`].
/// Phase 1 ships `osv`; the archive and external scanners arrive with phases
/// 3 and 5, and a registry that lists one before then is refused at startup
/// rather than silently scanned with nothing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ScannerConfig {
    Osv {
        #[serde(default)]
        api_url: Option<String>,
        #[serde(default)]
        escalation: Option<EscalationConfig>,
    },
    Trivy {
        endpoint: String,
        #[serde(default = "default_scanner_timeout")]
        timeout_secs: u64,
        #[serde(default)]
        escalation: Option<EscalationConfig>,
    },
    Postmortem {
        command: String,
        #[serde(default)]
        online: bool,
        #[serde(default = "default_true")]
        timeline: bool,
        #[serde(default)]
        escalation: Option<EscalationConfig>,
    },
    Guarddog {
        command: String,
        #[serde(default)]
        ecosystems: Vec<String>,
        #[serde(default)]
        escalation: Option<EscalationConfig>,
    },
    Mlab {
        #[serde(default)]
        api_key: Option<String>,
        #[serde(default)]
        escalation: Option<EscalationConfig>,
    },
    Sigstore {
        #[serde(default)]
        rekor_url: Option<String>,
        #[serde(default)]
        require_for: Vec<String>,
        #[serde(default)]
        escalation: Option<EscalationConfig>,
    },
    Socket {
        #[serde(default)]
        api_key: Option<String>,
        #[serde(default)]
        escalation: Option<EscalationConfig>,
    },
}

impl ScannerConfig {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Osv { .. } => "osv",
            Self::Trivy { .. } => "trivy",
            Self::Postmortem { .. } => "postmortem",
            Self::Guarddog { .. } => "guarddog",
            Self::Mlab { .. } => "mlab",
            Self::Sigstore { .. } => "sigstore",
            Self::Socket { .. } => "socket",
        }
    }

    pub fn escalation(&self) -> Option<&EscalationConfig> {
        match self {
            Self::Osv { escalation, .. }
            | Self::Trivy { escalation, .. }
            | Self::Postmortem { escalation, .. }
            | Self::Guarddog { escalation, .. }
            | Self::Mlab { escalation, .. }
            | Self::Sigstore { escalation, .. }
            | Self::Socket { escalation, .. } => escalation.as_ref(),
        }
    }

    /// Whether this build can run the scanner (RFC 0018 §12): `osv` in
    /// phase 1; `postmortem`, `trivy`, `sigstore`, `guarddog` in phase 3;
    /// `socket`, `mlab` in phase 5.
    pub fn available(&self) -> bool {
        matches!(
            self,
            Self::Osv { .. }
                | Self::Trivy { .. }
                | Self::Postmortem { .. }
                | Self::Guarddog { .. }
                | Self::Sigstore { .. }
        )
    }

    /// The phase the scanner ships in, for the refusal message.
    pub fn ships_in(&self) -> &'static str {
        match self {
            Self::Osv { .. } => "phase 1",
            Self::Trivy { .. }
            | Self::Postmortem { .. }
            | Self::Guarddog { .. }
            | Self::Sigstore { .. } => "RFC 0018 phase 3",
            Self::Mlab { .. } | Self::Socket { .. } => "RFC 0018 phase 5",
        }
    }

    /// An enrichment scanner never creates findings on its own, so it is
    /// never a sensible member of `required_scanners` (RFC 0018 §6.3).
    pub fn is_enrichment(&self) -> bool {
        matches!(self, Self::Mlab { .. })
    }
}

/// What a process does (`[server].roles`, RFC 0018 §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessRole {
    Proxy,
    Worker,
}

impl std::str::FromStr for ProcessRole {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "proxy" => Ok(Self::Proxy),
            "worker" => Ok(Self::Worker),
            other => Err(format!("unknown role '{other}' (expected proxy or worker)")),
        }
    }
}

/// Both roles — the embedded worker — which is what every existing
/// single-process deployment keeps on upgrade.
pub fn default_roles() -> Vec<ProcessRole> {
    vec![ProcessRole::Proxy, ProcessRole::Worker]
}

/// `[worker]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerConfig {
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    /// Empty means every registry; else only these names.
    #[serde(default)]
    pub registries: Vec<String>,
    #[serde(default = "default_job_timeout")]
    pub job_timeout_secs: u64,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default)]
    pub sandbox: SandboxConfig,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            max_concurrent: default_max_concurrent(),
            registries: Vec::new(),
            job_timeout_secs: default_job_timeout(),
            max_attempts: default_max_attempts(),
            sandbox: SandboxConfig::default(),
        }
    }
}

/// `[worker.sandbox]` — parsed now, enforced by phase 3's subprocess runner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxConfig {
    #[serde(default = "default_runtime")]
    pub runtime: String,
    #[serde(default = "default_memory_limit")]
    pub memory_limit_mb: u64,
    #[serde(default = "default_cpu_seconds")]
    pub cpu_seconds: u64,
    #[serde(default = "default_max_extracted")]
    pub max_extracted_mb: u64,
    #[serde(default = "default_max_entries")]
    pub max_entries: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            runtime: default_runtime(),
            memory_limit_mb: default_memory_limit(),
            cpu_seconds: default_cpu_seconds(),
            max_extracted_mb: default_max_extracted(),
            max_entries: default_max_entries(),
        }
    }
}

fn default_mode() -> SecurityMode {
    SecurityMode::Block
}
fn default_day() -> u64 {
    86_400
}
fn default_true() -> bool {
    true
}
fn default_osv() -> Vec<String> {
    vec!["osv".to_owned()]
}
fn default_max_severity() -> String {
    "high".to_owned()
}
fn default_install_hooks() -> InstallHookMode {
    InstallHookMode::Warn
}
fn default_scanner_error() -> ScannerErrorMode {
    ScannerErrorMode::Quarantine
}
fn default_scanner_timeout() -> u64 {
    60
}
fn default_max_concurrent() -> u32 {
    4
}
fn default_job_timeout() -> u64 {
    600
}
fn default_max_attempts() -> u32 {
    3
}
fn default_runtime() -> String {
    "bwrap".to_owned()
}
fn default_memory_limit() -> u64 {
    2048
}
fn default_cpu_seconds() -> u64 {
    300
}
fn default_max_extracted() -> u64 {
    512
}
fn default_max_entries() -> u64 {
    50_000
}
