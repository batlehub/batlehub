//! The credential contract file (RFC 0011 §4.1).
//!
//! `$BATLEHUB_HOME/state/vsx-token.json` is how a process that is **not** the
//! CLI learns which credential to present to a Batlehub registry. The editor
//! patch reads it; the CLI writes it; neither knows anything else about the
//! other. That is the whole of the contract, and it is a file rather than a
//! socket or an environment variable because an editor started by a desktop
//! session, a workspace template or a terminal has no reliable way to inherit
//! either.
//!
//! ```json
//! {
//!   "version": 1,
//!   "registries": {
//!     "https://hub.example.dev": {
//!       "token": "<bearer credential>",
//!       "kind": "oidc",
//!       "expires_at": "2026-08-18T12:00:00Z",
//!       "refresh": { "source": "cli", "owner": "batlehub-cli" }
//!     },
//!     "https://hub.k8s.dev": {
//!       "token": { "from": "file", "path": "/var/run/secrets/batlehub/token" },
//!       "kind": "kubernetes",
//!       "refresh": { "source": "reresolve" }
//!     }
//!   }
//! }
//! ```
//!
//! # What this module implements, and what it deliberately does not
//!
//! RFC 0011 §13 cut the document to the contract file, three CLI verbs and
//! the editor patch. So the sources here are **`inline` and `file`** — the
//! literal the CLI writes after a login, and the projected token something
//! else keeps fresh. `env`, `exchange` and `keychain` are named in the schema
//! as reserved and read as "no credential", which is §4.1.2 rule 4's safe
//! direction: a consumer that guessed at an unknown source is how the wrong
//! thing gets sent.
//!
//! # The rules that are rules
//!
//! * **Unknown fields are preserved.** A consumer that dropped what it did
//!   not understand would silently undo the writer that added it, and
//!   `version` only moves when a field changes meaning (§4.1).
//! * **Failure to resolve is "no credential", never an error** (§4.1.2 rule
//!   3). A missing file and a malformed document mean the same thing to the
//!   editor: no header. The reason is reported once, here, with the source
//!   named and the value absent.
//! * **A resolved secret is never written back** (rule 5). The point of the
//!   `file` source is that the credential is not at rest in this file.
//! * **A secret is never printed.** [`EntrySummary`] is the type the status
//!   table renders, and it has no field that can hold one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The version this CLI writes. A reader refuses a document from the future
/// rather than guessing at it.
pub const CONTRACT_VERSION: u32 = 1;

/// The most a `file` source may hold. A credential is a few kilobytes; a
/// bigger file is a mistake, and reading it into memory to find that out is
/// the mistake made twice.
const MAX_TOKEN_FILE_BYTES: u64 = 64 * 1024;

/// The name the CLI writes into `refresh.owner`, and the only owner it will
/// redeem for (§4.1.1 rule 2).
pub const CLI_OWNER: &str = "batlehub-cli";

/// `$BATLEHUB_HOME`, defaulting to `$HOME/.batlehub`.
///
/// Deliberately *not* `dirs::config_dir()`, which is where the CLI's own
/// profile store lives: the two files have different jobs and different
/// readers, and an editor patch that had to know about XDG on Linux and
/// `Application Support` on macOS would be a second contract.
pub fn batlehub_home() -> PathBuf {
    if let Ok(home) = std::env::var("BATLEHUB_HOME") {
        if !home.is_empty() {
            return PathBuf::from(home);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".batlehub")
}

/// The default contract path. `VSX_REGISTRY_AUTH_TOKEN_FILE` overrides it —
/// the same variable the editor patch reads first, so pointing one at a
/// different file points both (§4.1.3).
pub fn contract_path() -> PathBuf {
    if let Ok(p) = std::env::var("VSX_REGISTRY_AUTH_TOKEN_FILE") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    batlehub_home().join("state").join("vsx-token.json")
}

// ── the document ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractFile {
    pub version: u32,
    #[serde(default)]
    pub registries: BTreeMap<String, Entry>,
    /// Everything this version does not know about, kept so a rewrite does
    /// not undo a writer that knew more.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl Default for ContractFile {
    fn default() -> Self {
        Self {
            version: CONTRACT_VERSION,
            registries: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub token: TokenSource,
    pub kind: Kind,
    /// Absent means "treat as non-expiring" — a PAT, or a credential whose
    /// lifetime this writer did not know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<Refresh>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

/// What kind of credential the entry holds. It decides nothing here — the
/// consumer puts the value in a header either way — and exists so a human
/// reading the file, or `auth status`, can tell a PAT from a login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Oidc,
    Pat,
    Kubernetes,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Oidc => "oidc",
            Self::Pat => "pat",
            Self::Kubernetes => "kubernetes",
        }
    }
}

/// Where the credential comes from (§4.1.2).
///
/// Untagged, because the plain string is the shorthand every hand-written
/// file uses and the shape the editor patch understands: a bare `"token":
/// "abc"` is exactly `{"from": "inline", "value": "abc"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TokenSource {
    /// The literal credential. What the CLI writes after a login, and the
    /// only shape the editor patch reads.
    Literal(String),
    Object(SourceObject),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "lowercase")]
pub enum SourceObject {
    Inline {
        value: String,
    },
    File {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pointer: Option<String>,
    },
    /// `env`, `exchange`, `keychain` — named in the schema, not implemented
    /// in this cut. Read as "no credential" and warned about once, which is
    /// §4.1.2 rule 4: adding a source later must not require a `version`
    /// bump, and guessing at one must never send the wrong thing.
    #[serde(other)]
    Unsupported,
}

/// How a fresh credential appears when this one expires (§4.1.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Refresh {
    pub source: RefreshSource,
    /// The one process allowed to redeem. Exactly one refresher per entry:
    /// rotation plus two racing refreshers is not a lost update, it is the
    /// IDP seeing a replayed refresh token and revoking the chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefreshSource {
    /// The refresh material is in the CLI's own profile store.
    Cli,
    /// There is no refresh token: resolving `token` again yields a fresh one,
    /// because something else keeps it fresh. A kubelet rotating a projected
    /// token is the reference case.
    Reresolve,
    /// The refresh token is in this file. The fallback for a consumer with no
    /// secret store, never what the CLI writes.
    Inline,
    /// Nothing to refresh. On expiry the answer is a new login.
    None,
}

impl RefreshSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Reresolve => "reresolve",
            Self::Inline => "inline",
            Self::None => "none",
        }
    }
}

// ── what a resolution produced ───────────────────────────────────────────────

/// The outcome of resolving one entry, as `auth status` reports it.
///
/// `Refused` and `Unreachable` are never constructed here, and that is
/// deliberate: they belong to the `exchange` source, which RFC 0011 §13 cut
/// from this pass. They stay in the vocabulary because the state names are
/// the user-visible contract — an operator reading `unset` rather than
/// `refused` is choosing between opposite fixes — and a follow-up that had to
/// rename them would break every runbook written against this one.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Resolved, and not past `expires_at`.
    Ok,
    /// Resolved, but `expires_at` has passed. **Not deleted**: the owner can
    /// still refresh it, and a consumer that removed it would log the user
    /// out of every registry because one process noticed first.
    Expired,
    /// The variable or the file is not there, or held nothing.
    Unset,
    /// The document said something this build does not implement.
    Unsupported,
    /// An endpoint outside the configured trusted set. Reserved for
    /// `exchange`.
    Refused,
    /// The STS did not answer. Reserved for `exchange`.
    Unreachable,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Expired => "expired",
            Self::Unset => "unset",
            Self::Unsupported => "unsupported",
            Self::Refused => "refused",
            Self::Unreachable => "unreachable",
        }
    }
}

/// One entry, as a status table renders it.
///
/// **There is no field here that can hold a credential.** Redaction is a
/// property of the type rather than of each call site, so a log line added
/// later cannot leak what this one never carried (§4.6).
#[derive(Debug, Clone, Serialize)]
pub struct EntrySummary {
    pub registry: String,
    pub kind: &'static str,
    /// `inline (written by cli)`, `file /var/run/…/token` — the source, never
    /// its value.
    pub source: String,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// `4m12s`, or `—` when the entry does not expire.
    pub expires_in: String,
    pub refresh: String,
    /// Why the state is what it is, when that is not obvious from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A resolved credential. Kept apart from [`EntrySummary`] so the two cannot
/// be confused at a call site: this one is passed to a header, that one to a
/// terminal.
pub struct Resolved {
    pub state: State,
    pub token: Option<String>,
    pub detail: Option<String>,
}

impl Resolved {
    fn none(state: State, detail: impl Into<String>) -> Self {
        Self {
            state,
            token: None,
            detail: Some(detail.into()),
        }
    }
}

// ── reading, resolving, writing ──────────────────────────────────────────────

impl ContractFile {
    /// Read the contract, or an empty one when there is nothing to read.
    ///
    /// A document that does not parse is **not** an error: it is "no
    /// credential", reported once. It must never break an anonymous gallery
    /// (§4.3, *unparseable file*).
    pub fn load(path: &Path) -> Self {
        match Self::try_load(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "Warning: {} is not a usable contract file: {e}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// [`Self::load`], but saying why. For the writer, which must not
    /// overwrite a file it could not read — that would silently discard
    /// another registry's entry.
    pub fn try_load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let doc: Self = serde_json::from_str(&text).context("the document is not valid JSON")?;
        if doc.version > CONTRACT_VERSION {
            bail!(
                "it is version {}, and this CLI understands {CONTRACT_VERSION}",
                doc.version
            );
        }
        Ok(doc)
    }

    /// Write the whole document atomically: temp file beside the target,
    /// `0600`, then rename. Parent directories are `0700`.
    ///
    /// Rename rather than truncate-and-write because a reader is an editor
    /// that may look at any moment, and half a JSON document is a credential
    /// that vanished.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
            restrict_dir(parent);
        }
        let body = serde_json::to_string_pretty(self).context("serializing the contract")?;
        let tmp = path.with_extension(format!("json.tmp{}", std::process::id()));
        std::fs::write(&tmp, format!("{body}\n"))
            .with_context(|| format!("writing {}", tmp.display()))?;
        restrict_file(&tmp);
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    /// Insert or replace one registry's entry, preserving every other entry
    /// and every unknown field (§4.2, *read-modify-write*).
    pub fn set_entry(&mut self, registry: &str, entry: Entry) {
        self.registries.insert(normalize_origin(registry), entry);
    }

    pub fn entry(&self, registry: &str) -> Option<&Entry> {
        self.registries.get(&normalize_origin(registry))
    }
}

/// The key an entry is filed under: the origin, without a trailing slash.
///
/// The consumer matches the *configured gallery origin* against these keys,
/// so `https://hub.example.dev` and `https://hub.example.dev/` have to be one
/// entry or a login writes a key nothing reads.
pub fn normalize_origin(registry: &str) -> String {
    registry.trim_end_matches('/').to_owned()
}

impl Entry {
    /// A literal entry, which is what the CLI writes after a login.
    pub fn literal(
        token: impl Into<String>,
        kind: Kind,
        expires_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            token: TokenSource::Literal(token.into()),
            kind,
            expires_at,
            refresh: Some(Refresh {
                // What the CLI can actually do: the refresh material is in
                // its own profile store, and it names itself the owner.
                // Never `inline` — it has somewhere better to put it
                // (§4.1.1 rule 1).
                source: match kind {
                    Kind::Oidc => RefreshSource::Cli,
                    Kind::Kubernetes => RefreshSource::Reresolve,
                    Kind::Pat => RefreshSource::None,
                },
                owner: matches!(kind, Kind::Oidc).then(|| CLI_OWNER.to_owned()),
                extra: BTreeMap::new(),
            }),
            extra: BTreeMap::new(),
        }
    }

    /// A `file` entry: the credential stays where it is, and something else
    /// keeps it fresh.
    pub fn from_file(path: impl Into<String>, kind: Kind) -> Self {
        Self {
            token: TokenSource::Object(SourceObject::File {
                path: path.into(),
                format: None,
                pointer: None,
            }),
            kind,
            expires_at: None,
            refresh: Some(Refresh {
                source: RefreshSource::Reresolve,
                owner: None,
                extra: BTreeMap::new(),
            }),
            extra: BTreeMap::new(),
        }
    }

    /// What this entry is, in the words the status table uses. Never its
    /// value.
    pub fn source_label(&self) -> String {
        match &self.token {
            TokenSource::Literal(_) => match self.refresh.as_ref().map(|r| r.source) {
                Some(RefreshSource::Cli) => "inline (written by cli)".to_owned(),
                _ => "inline".to_owned(),
            },
            TokenSource::Object(SourceObject::Inline { .. }) => "inline".to_owned(),
            TokenSource::Object(SourceObject::File { path, .. }) => format!("file {path}"),
            TokenSource::Object(SourceObject::Unsupported) => "unsupported source".to_owned(),
        }
    }

    pub fn refresh_label(&self) -> String {
        match &self.refresh {
            Some(r) => match &r.owner {
                Some(o) => format!("{} ({o})", r.source.as_str()),
                None => r.source.as_str().to_owned(),
            },
            None => "none".to_owned(),
        }
    }

    /// Resolve the credential **now**, which is the only thing worth
    /// reporting: a cached "ok" from before the token file rotated is the
    /// failure being debugged (§4.6).
    pub fn resolve(&self) -> Resolved {
        let value = match &self.token {
            TokenSource::Literal(v) | TokenSource::Object(SourceObject::Inline { value: v }) => {
                let v = v.trim();
                if v.is_empty() {
                    return Resolved::none(State::Unset, "the entry holds an empty credential");
                }
                v.to_owned()
            }
            TokenSource::Object(SourceObject::File {
                path,
                format,
                pointer,
            }) => match read_token_file(path, format.as_deref(), pointer.as_deref()) {
                Ok(v) => v,
                // `{:#}` rather than `{}`: the outermost context is "reading
                // /var/run/…/token", and the cause is the half that says
                // whether it is absent, a directory, or unreadable — which is
                // the half an operator acts on.
                Err(e) => return Resolved::none(State::Unset, format!("{e:#}")),
            },
            TokenSource::Object(SourceObject::Unsupported) => {
                return Resolved::none(
                    State::Unsupported,
                    "this build implements the `inline` and `file` sources only",
                )
            }
        };

        // Expiry is reported, never enforced by withholding: an expired entry
        // is still the entry, and the owner can refresh it.
        match self.expires_at {
            Some(exp) if Utc::now() >= exp => Resolved {
                state: State::Expired,
                token: Some(value),
                detail: None,
            },
            _ => Resolved {
                state: State::Ok,
                token: Some(value),
                detail: None,
            },
        }
    }

    pub fn summary(&self, registry: &str) -> EntrySummary {
        let r = self.resolve();
        EntrySummary {
            registry: registry.to_owned(),
            kind: self.kind.as_str(),
            source: self.source_label(),
            state: r.state.as_str(),
            expires_at: self.expires_at,
            expires_in: match self.expires_at {
                Some(exp) => humanize(exp - Utc::now()),
                None => "—".to_owned(),
            },
            refresh: self.refresh_label(),
            detail: r.detail,
        }
    }
}

/// Read a `file` source, with the rules §4.1.2 rule 5 attaches to it.
fn read_token_file(path: &str, format: Option<&str>, pointer: Option<&str>) -> Result<String> {
    let p = Path::new(path);
    // Absolute, because a relative path resolves against whatever directory
    // the *consumer* happened to start in — which for an editor is not a
    // thing the file's author can know.
    if !p.is_absolute() {
        bail!("`{path}` is not an absolute path");
    }
    let meta = std::fs::metadata(p).with_context(|| format!("reading {path}"))?;
    if !meta.is_file() {
        bail!("`{path}` is not a regular file");
    }
    if meta.len() > MAX_TOKEN_FILE_BYTES {
        bail!(
            "`{path}` is {} bytes; a credential is not (cap {MAX_TOKEN_FILE_BYTES})",
            meta.len()
        );
    }
    warn_if_readable_by_others(p, &meta);

    let text = std::fs::read_to_string(p).with_context(|| format!("reading {path}"))?;
    let value = match format {
        Some("json") => {
            let doc: Value = serde_json::from_str(&text)
                .with_context(|| format!("{path} is not JSON, but `format` says it is"))?;
            let at = pointer.unwrap_or("/token");
            doc.pointer(at)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .with_context(|| format!("{path} has no string at `{at}`"))?
        }
        // `raw`, absent, or anything else: the file is the credential.
        _ => text,
    };
    let value = value.trim().to_owned();
    if value.is_empty() {
        bail!("`{path}` is empty");
    }
    Ok(value)
}

/// A file that is a credential should be told so.
#[cfg(unix)]
fn warn_if_readable_by_others(path: &Path, meta: &std::fs::Metadata) {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        eprintln!(
            "Warning: {} is mode {:04o}; it holds a credential, and other local users can read it",
            path.display(),
            mode
        );
    }
}

#[cfg(not(unix))]
fn warn_if_readable_by_others(_path: &Path, _meta: &std::fs::Metadata) {}

#[cfg(unix)]
fn restrict_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(unix)]
fn restrict_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) {}

#[cfg(not(unix))]
fn restrict_dir(_path: &Path) {}

/// `4m12s`, `58m`, `2d3h`, or `expired`.
fn humanize(d: chrono::Duration) -> String {
    let secs = d.num_seconds();
    if secs <= 0 {
        return "expired".to_owned();
    }
    let (days, hours, mins, s) = (
        secs / 86_400,
        (secs % 86_400) / 3600,
        (secs % 3600) / 60,
        secs % 60,
    );
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{mins}m")
    } else if mins > 0 {
        format!("{mins}m{s}s")
    } else {
        format!("{s}s")
    }
}

// ── validation the writer applies before a write ─────────────────────────────

/// Refuse an entry a reader could not act on (§4.3, the client table).
///
/// Applied at *write* time on purpose: the alternative is discovering it at
/// read time, in an editor, as extensions that are quietly missing.
pub fn validate_entry(entry: &Entry) -> Result<()> {
    if let TokenSource::Object(SourceObject::File { path, format, .. }) = &entry.token {
        if !Path::new(path).is_absolute() {
            bail!("a `file` token source needs an absolute path, and `{path}` is not one");
        }
        if let Some(f) = format {
            if f != "raw" && f != "json" {
                bail!("a `file` token source's format is `raw` or `json`, not `{f}`");
            }
        }
    }
    let Some(refresh) = &entry.refresh else {
        return Ok(());
    };
    if matches!(refresh.source, RefreshSource::Reresolve)
        && matches!(
            entry.token,
            TokenSource::Literal(_) | TokenSource::Object(SourceObject::Inline { .. })
        )
    {
        // Re-resolving a literal yields the same literal, so the entry would
        // claim a refresh path it does not have and silently never refresh.
        bail!("`refresh.source = \"reresolve\"` on a literal token can never produce a new one");
    }
    if matches!(entry.kind, Kind::Pat) && !matches!(refresh.source, RefreshSource::None) {
        // A warning rather than a refusal: the entry still works, and the
        // block is a writer bug worth surfacing rather than a reason to
        // refuse a usable credential.
        eprintln!(
            "Warning: a PAT has nothing to redeem, so `refresh.source = \"{}\"` will be ignored",
            refresh.source.as_str()
        );
    }
    if matches!(refresh.source, RefreshSource::Inline) {
        eprintln!(
            "Warning: `refresh.source = \"inline\"` puts a refresh token in a file an editor \
             reads. It is the fallback for a consumer with no secret store, and this CLI has one."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
