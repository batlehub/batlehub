use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use batlehub_config::schema::ConfigWarning;
use batlehub_core::ports::ConfigChangeRepository;
use batlehub_core::services::{HotConfig, HotConfigLock};

use crate::{
    middleware::ProxyTrust, services::banner::BannerService, AccessConfig, AccessConfigLock,
    CargoIndexMap, RegistryHostMap, RegistryMap, RegistryModeMap, RepoSignerMap, SumDbMap,
    UpstreamMap, VulnDbMap,
};

pub(super) const PENDING_TTL_SECS: i64 = 600; // 10 minutes

pub mod applier;
mod config_file;
mod drift;
pub mod validator;

use config_file::ConfigFile;

pub use applier::{ConfigChangeRow, ReloadApplyError};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct ChangedRegistry {
    pub name: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct ReloadDiff {
    pub added_registries: Vec<String>,
    pub removed_registries: Vec<String>,
    pub changed_registries: Vec<ChangedRegistry>,
    pub access_config_changed: bool,
    pub limits_changed: bool,
    /// Keys this candidate changes that every reader sees once it is applied
    /// (RFC 0037 §4.3, tier *hot*), against the config in force.
    #[serde(default)]
    pub applied: Vec<String>,
    /// Keys this candidate changes that every request sees once it is
    /// applied, while a component built at startup — warming, release
    /// imports, the scan worker — keeps the old value until a restart.
    #[serde(default)]
    pub partially_applied: Vec<String>,
    /// Every restart-only key the running process does not have, against the
    /// config it **started** on: a key an earlier reload applied stays listed
    /// until the restart that makes it true. `["*"]` when the documents could
    /// not be compared, with the reason in `restart_reason` — any
    /// `[[...]]` entry whose `name` or `type` is a placeholder does that.
    #[serde(default)]
    pub restart_required: Vec<String>,
    /// Why `restart_required` is `["*"]`: the config uses a placeholder where
    /// `batlehub_config::merge_layers` cannot keep it (RFC 0037 §4.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_reason: Option<String>,
    /// Whether this candidate changes a restart-only key *against the config in
    /// force*. `restart_required` compares against startup instead, so it is
    /// the wrong question for [`Self::is_noop`].
    #[serde(skip)]
    restart_changed: bool,
}

impl ReloadDiff {
    /// True when re-parsing the config produced no detectable change. Used to avoid
    /// surfacing a "pending reload" for filesystem events (touch, atomic-save
    /// rewrite) that don't actually change the config content.
    pub fn is_noop(&self) -> bool {
        self.added_registries.is_empty()
            && self.removed_registries.is_empty()
            && self.changed_registries.is_empty()
            && !self.access_config_changed
            && !self.limits_changed
            && self.applied.is_empty()
            && self.partially_applied.is_empty()
            && !self.restart_changed
    }

    /// Fill the RFC 0037 lists: `applied` and `partially_applied` from
    /// `in_force → candidate`, `restart_required` from `startup → candidate`.
    ///
    /// A side `merge_layers` refuses leaves that comparison unknown, and
    /// unknown is a restart; the other comparison still stands, and the reload
    /// itself goes on exactly as before, since the server runs configurations
    /// `merge_layers` cannot render. `startup` comes merged already: it never
    /// changes, so [`ConfigReloadService::new`] merges it once.
    fn classify(
        &mut self,
        startup: &Result<String, String>,
        in_force: &[String],
        candidate: &[String],
    ) {
        use batlehub_config::{merge_layers, reload_keys::classify};
        let unknown = |side: &str, e: &dyn std::fmt::Display| format!("{side}: {e:#}");
        let after = match merge_layers(candidate) {
            Ok(after) => after,
            Err(e) => {
                self.restart_changed = true;
                self.restart_required = vec!["*".to_owned()];
                self.restart_reason = Some(unknown("the candidate config", &e));
                return;
            }
        };
        let changed = merge_layers(in_force)
            .map_err(|e| unknown("the config in force", &e))
            .and_then(|before| classify(&before, &after).map_err(|e| format!("{e:#}")));
        let owed = startup
            .as_ref()
            .map_err(|e| format!("the startup config: {e}"))
            .and_then(|before| classify(before, &after).map_err(|e| format!("{e:#}")));
        match owed {
            Ok(owed) => self.restart_required = owed.restart,
            Err(reason) => {
                self.restart_required = vec!["*".to_owned()];
                self.restart_reason = Some(reason);
            }
        }
        let changed = match changed {
            Ok(changed) => changed,
            Err(reason) => {
                self.restart_changed = true;
                self.restart_required = vec!["*".to_owned()];
                self.restart_reason = Some(reason);
                return;
            }
        };
        let mut by_name: Vec<ChangedRegistry> = Vec::new();
        for path in changed
            .hot
            .iter()
            .chain(&changed.partial)
            .chain(&changed.restart)
        {
            let Some((name, field)) = path
                .strip_prefix("registries[")
                .and_then(|rest| rest.split_once("]."))
            else {
                continue; // not a registry, or one added or removed whole
            };
            if self.added_registries.iter().any(|a| a == name) {
                continue; // the restart-only keys an added registry carries
            }
            match by_name.iter_mut().find(|c| c.name == name) {
                Some(c) => c.fields.push(field.to_owned()),
                None => by_name.push(ChangedRegistry {
                    name: name.to_owned(),
                    fields: vec![field.to_owned()],
                }),
            }
        }
        by_name.sort_by(|a, b| a.name.cmp(&b.name));
        for c in &mut by_name {
            c.fields.sort();
        }
        self.changed_registries = by_name;
        self.restart_changed = !changed.restart.is_empty();
        self.applied = changed.hot;
        self.partially_applied = changed.partial;
    }

    /// Set `search.text_config` in `restart_required` from what the column
    /// was built with rather than from the startup document.
    pub(super) fn settle_text_config(&mut self, owed: bool) {
        const KEY: &str = "search.text_config";
        if self.restart_required == ["*"] {
            return;
        }
        self.restart_required.retain(|k| k != KEY);
        if owed {
            self.restart_required.push(KEY.to_owned());
            self.restart_required.sort();
        }
    }
}

/// A [`ReloadDiff`] plus the [`ConfigWarning`]s the candidate config produces.
///
/// Returned by the two dry-run-ish entry points (`validate_content` and
/// `load_pending_from_content`) so an admin sees the warnings *before* applying
/// a pending reload rather than after. The applied config's warnings are read
/// from [`ConfigWarnings`] instead, since they outlive the request that set them.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct ReloadOutcome {
    pub diff: ReloadDiff,
    pub warnings: Vec<ConfigWarning>,
    /// Whether this call left a pending reload staged for approval.
    ///
    /// `false` is not a failure — it is the answer for a dry run
    /// (`validate_content` stages nothing by contract) and for content that is
    /// byte-identical to the last load attempt, where there is nothing to
    /// rebuild. Without it a caller cannot tell "staged, go apply it" from
    /// "nothing to stage", since both return `200` with an empty diff, and would
    /// only find out at apply time via [`ReloadApplyError::NoPendingReload`].
    pub pending_created: bool,
}

/// The [`ConfigWarning`]s of the config currently in force, shared between the
/// reload service (which refreshes them on every apply) and the admin endpoint
/// that serves them.
#[derive(Clone, Default)]
pub struct ConfigWarnings(Arc<std::sync::RwLock<Vec<ConfigWarning>>>);

impl ConfigWarnings {
    pub fn get(&self) -> Vec<ConfigWarning> {
        self.0
            .read()
            .expect("config warnings lock poisoned")
            .clone()
    }

    /// Replace the stored warnings and log each one. Called at startup and after
    /// every applied reload, so the log and the endpoint never disagree.
    pub fn replace(&self, warnings: Vec<ConfigWarning>) {
        for w in &warnings {
            tracing::warn!(code = %w.code, path = %w.path, "config: {}", w.message);
        }
        *self.0.write().expect("config warnings lock poisoned") = warnings;
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReloadSource {
    FileWatcher,
    AdminRequest,
}

impl ReloadSource {
    /// Who a rejected candidate is attributed to in `config_changes`: the
    /// candidate is refused before anyone applies it, so there is no actor yet.
    pub fn actor_label(&self) -> &'static str {
        match self {
            Self::FileWatcher => "file-watcher",
            Self::AdminRequest => "config-editor",
        }
    }
}

pub struct PendingReload {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub source: ReloadSource,
    pub diff: ReloadDiff,
    /// Raw TOML submitted via the config-editor API. Present only when the pending
    /// reload originated from `load_pending_from_content`; absent for file-watcher
    /// reloads (the file is already on disk). `apply()` writes this back to disk so
    /// editor changes survive a server restart.
    pub content: Option<String>,
    /// The raw layers of the candidate, which become
    /// [`ConfigReloadService::applied_layers`] when it is applied.
    pub layers: Vec<String>,
    /// Pre-built, ready to swap in — no async work required during apply.
    pub new_hot: HotConfig,
    pub new_access: AccessConfig,
    pub new_search_readmes: bool,
    pub new_registry_map: RegistryMap,
    pub new_registry_mode_map: RegistryModeMap,
    pub new_upstream_map: UpstreamMap,
    pub new_cargo_index_map: CargoIndexMap,
    pub new_repo_signer_map: RepoSignerMap,
    pub new_apk_signer_map: crate::ApkSignerMap,
    pub new_vuln_db_map: VulnDbMap,
    pub new_sumdb_map: SumDbMap,
    pub new_registry_host_map: RegistryHostMap,
    /// The candidate's proxy-trust policy. Derived from the `AppConfig` directly
    /// rather than from the builder, since it needs no adapter wiring — see
    /// `build_pending`.
    pub new_proxy_trust: ProxyTrust,
    /// Warnings the candidate config produces, promoted to the live
    /// [`ConfigWarnings`] store when this pending reload is applied.
    pub warnings: Vec<ConfigWarning>,
}

/// Snapshot of a pending reload returned to the GET /pending endpoint.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct PendingReloadSnapshot {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub source: ReloadSource,
    pub diff: ReloadDiff,
    /// Warnings the *candidate* config produces (RFC 0004-bis A4).
    ///
    /// `PendingReload` has held these since it was written — `apply()` promotes
    /// them to the live store — and the snapshot dropped them, so the one
    /// surface that exists to review a change before applying it could not show
    /// what the change would warn about. The console worked around it by
    /// remembering the warnings from the `validate` call that staged the
    /// pending reload, which is lost on any reload of the page.
    pub warnings: Vec<ConfigWarning>,
}

/// Named result of a [`HotConfigBuilder`] invocation, replacing an untyped
/// 8-element tuple so adding/removing a field is a compile error at each
/// construction/destructuring site instead of a silent positional shift.
pub struct BuiltHotState {
    pub hot: HotConfig,
    pub access: AccessConfig,
    pub registry_map: RegistryMap,
    pub registry_mode_map: RegistryModeMap,
    pub upstream_map: UpstreamMap,
    pub cargo_index_map: CargoIndexMap,
    pub repo_signer_map: RepoSignerMap,
    /// The RSA index signers of `apk` registries, rebuilt so a rotated key takes
    /// effect on reload rather than on restart (RFC 0026 §4.1).
    pub apk_signer_map: crate::ApkSignerMap,
    pub vuln_db_map: VulnDbMap,
    /// Go checksum database URLs (RFC 0009 §7.4).
    pub sumdb_map: SumDbMap,
    pub registry_host_map: RegistryHostMap,
    /// `[search] readmes` (RFC 0007-bis §4.1).
    ///
    /// Carried through the reload path rather than read once at startup because
    /// it governs an index read on the catalogue's busiest endpoint, and
    /// "restart the server to stop searching prose" is not an answer an operator
    /// should have to accept.
    pub search_readmes: bool,
    /// The candidate's `[search] text_config` is not the one the README
    /// full-text column was built with, so it waits for the rebuild a restart
    /// does. The column, not the startup document, is what is in force: the
    /// two differ when the process booted on a config the column did not match.
    pub text_config_owed: bool,
}

/// Builds a new hot-reloadable bundle from an `AppConfig`.
///
/// Created in `server/src/main.rs` as a closure capturing `beta_channel_store`,
/// `repo`, etc. Passed to `ConfigReloadService` so the service can rebuild state
/// on reload without depending on adapter types directly.
pub type HotConfigBuilder =
    Arc<dyn Fn(&batlehub_config::schema::AppConfig) -> anyhow::Result<BuiltHotState> + Send + Sync>;

/// Service responsible for hot-reloading configuration at runtime.
pub struct ConfigReloadService {
    pub(super) hot: HotConfigLock,
    pub(super) access: AccessConfigLock,
    pub(super) search: crate::SearchConfigLock,
    pub(super) registry_map: RegistryMap,
    pub(super) registry_mode_map: RegistryModeMap,
    pub(super) upstream_map: UpstreamMap,
    pub(super) cargo_index_map: CargoIndexMap,
    pub(super) repo_signer_map: RepoSignerMap,
    pub(super) apk_signer_map: crate::ApkSignerMap,
    pub(super) vuln_db_map: VulnDbMap,
    pub(super) sumdb_map: SumDbMap,
    pub(super) registry_host_map: RegistryHostMap,
    /// The live proxy-trust policy, swapped on apply just before
    /// `registry_host_map` — routing reads a header, so the table and the policy
    /// that says who may set it must never drift apart. Supplied by
    /// [`ConfigReloadParams::proxy_trust`], which see for why it is a mandatory
    /// field rather than something defaulted here.
    pub(super) proxy_trust: ProxyTrust,
    /// See [`ConfigReloadParams::discovery`].
    pub(super) discovery: Arc<batlehub_core::services::UpstreamDetailCoordinator>,
    /// The config file this process was started with. A [`ConfigFile`], not a
    /// `String`, so that nothing can put a path here that did not come from
    /// process arguments — see that type for what the distinction buys and what
    /// it does not.
    /// Private, not `pub(super)` like its neighbours: `applier` and `validator`
    /// are descendants of this module and reach it anyway, and nothing outside
    /// has any business holding the path this service opens.
    config_file: ConfigFile,
    /// The layers merged *over* [`Self::config_file`], in order, when the
    /// process was started with more than one `--config`. Re-read on every
    /// reload, exactly like the primary, so a change to a credentials file is
    /// picked up by the same watcher event that a change to the main file is.
    ///
    /// Deliberately absent from the editor path: `config_content` serves the
    /// primary and `persist_config_to_disk` writes the primary, so a layer
    /// holding credentials is never sent to a browser and never rewritten from
    /// one. It is still merged before validation, so the diff and the warnings
    /// an admin sees describe the config that would actually be in force.
    config_overlays: Vec<ConfigFile>,
    pub(super) config_change_repo: Option<Arc<dyn ConfigChangeRepository>>,
    pub hot_reload_enabled: bool,
    /// Intentionally panics (`.expect(...)`) rather than recovers on poison, unlike
    /// `notification::dispatch`'s `Mutex`, which favors availability and recovers via
    /// `unwrap_or_else(|e| e.into_inner())`. A panic here means a reload panicked while
    /// holding this lock, which can only mean a bug already corrupted in-flight config
    /// state; serving a possibly-torn config swap after that is worse than crashing.
    pub(super) pending: Mutex<Option<PendingReload>>,
    pub(super) builder: HotConfigBuilder,
    pub(super) banner: Option<Arc<BannerService>>,
    /// Warnings for the config currently in force. Populated at startup via
    /// [`ConfigReloadService::set_warnings`] and refreshed by `apply`.
    pub(super) config_warnings: ConfigWarnings,
    /// Raw content of the last config load attempt (file-watcher or editor), whether
    /// or not it ended up applied. Lets `load_pending`/`load_pending_from_content`
    /// suppress redundant rebuilds triggered by byte-identical rewrites (touch,
    /// atomic-save) without relying on `ReloadDiff`, which can't detect a change to
    /// an existing registry's fields (see `compute_diff`) and would otherwise treat
    /// such a change as a no-op too.
    pub(super) last_content: Mutex<Option<String>>,
    /// The raw layers of the config **in force**: those the process started on,
    /// then those of each applied reload. Not [`Self::last_content`], which is
    /// the last load *attempt*. What `applied` and `partially_applied` are
    /// diffed against (RFC 0037 §6.5).
    pub(super) applied_layers: Mutex<Vec<String>>,
    /// The layers the process started on, through `merge_layers` once: for a
    /// restart-only key that is what is in force, whatever was applied since,
    /// and what every `restart_required` is diffed against. The error as text
    /// when `merge_layers` refuses them.
    pub(super) startup_document: Result<String, String>,
    /// Where this replica reports its configuration, and under which name —
    /// unset on a single server. See `drift.rs`.
    pub(super) cluster:
        std::sync::OnceLock<(Arc<dyn batlehub_core::ports::ClusterStatePort>, String)>,
}

/// Everything [`ConfigReloadService::new`] needs, as named fields.
///
/// A struct rather than 15 positional parameters because most of them are
/// interchangeable at the type level — six registry-keyed maps, two `Option`s,
/// two booleans-adjacent values — so a transposed pair would compile and only
/// show up as a reload that swaps the wrong table.
///
/// It is also what makes [`proxy_trust`](Self::proxy_trust) safe. Every field of
/// a struct literal is mandatory, so a construction site that does not pass the
/// app's live handle is a *compile* error. The alternative shapes could not give
/// that: a defaulted field or a `with_proxy_trust` setter both let a caller
/// silently end up with a detached handle, and no runtime check can catch it —
/// a never-wired policy and a legitimately unconfigured one are both just
/// `is_configured() == false`.
pub struct ConfigReloadParams {
    pub hot: HotConfigLock,
    pub access: AccessConfigLock,
    /// The app's live `[search] readmes` handle — the same one registered as
    /// `app_data`, not a fresh one, for the reason `proxy_trust` gives below.
    pub search: crate::SearchConfigLock,
    pub registry_map: RegistryMap,
    pub registry_mode_map: RegistryModeMap,
    pub upstream_map: UpstreamMap,
    pub cargo_index_map: CargoIndexMap,
    pub repo_signer_map: RepoSignerMap,
    pub apk_signer_map: crate::ApkSignerMap,
    pub vuln_db_map: VulnDbMap,
    pub sumdb_map: SumDbMap,
    pub registry_host_map: RegistryHostMap,
    /// The app's live [`ProxyTrust`] handle — the same one wrapped into the
    /// host-routing middleware and registered as `app_data`, not a fresh one.
    /// All clones share a lock, which is what lets `apply` update the policy the
    /// middleware actually reads; a `ProxyTrust::default()` here would be a
    /// detached handle and every reload of it a silent no-op.
    pub proxy_trust: ProxyTrust,
    /// The app's live discovery coordinator — the one held by `ProxyService`.
    /// `apply` forgets its remembered upstream absences: a reload that adds an
    /// upstream or repairs its credentials changes what "absent" means, and a
    /// fresh handle here would leave the operator's fix invisible until restart.
    pub discovery: Arc<batlehub_core::services::UpstreamDetailCoordinator>,
    /// The primary config file, as the process was started with it: the first
    /// `--config`, or the first `BATLEHUB_CONFIG` segment, or `"config.toml"`.
    ///
    /// A `String` here and a `ConfigFile` on the service is deliberate — this
    /// struct is the argv-shaped boundary, and `ConfigReloadService::new` is the
    /// one crossing. Nothing but process arguments belongs in it.
    pub config_path: String,
    /// Extra config files merged over `config_path`, in order, and from the same
    /// source. Empty for the single-file case, which is every deployment that
    /// has not asked for layering.
    pub config_overlays: Vec<String>,
    /// The text of `config_path` then each overlay, as the process loaded them
    /// at startup — the baseline `restart_required` is computed against.
    pub startup_layers: Vec<String>,
    pub config_change_repo: Option<Arc<dyn ConfigChangeRepository>>,
    pub hot_reload_enabled: bool,
    pub builder: HotConfigBuilder,
    pub banner: Option<Arc<BannerService>>,
}

impl ConfigReloadService {
    pub fn new(params: ConfigReloadParams) -> Self {
        let ConfigReloadParams {
            hot,
            access,
            search,
            registry_map,
            registry_mode_map,
            upstream_map,
            cargo_index_map,
            repo_signer_map,
            apk_signer_map,
            vuln_db_map,
            sumdb_map,
            registry_host_map,
            proxy_trust,
            discovery,
            config_path,
            config_overlays,
            startup_layers,
            config_change_repo,
            hot_reload_enabled,
            builder,
            banner,
        } = params;
        Self {
            hot,
            access,
            search,
            registry_map,
            registry_mode_map,
            upstream_map,
            cargo_index_map,
            repo_signer_map,
            apk_signer_map,
            vuln_db_map,
            sumdb_map,
            registry_host_map,
            proxy_trust,
            discovery,
            // The one place a `String` becomes a path this service will open.
            // `ConfigReloadParams` still carries the operator's own arguments,
            // which is what `server/src/main.rs` and the test fixtures have to
            // hand; the conversion is here so that boundary is a single line.
            config_file: ConfigFile::from_process_argument(&config_path),
            config_overlays: config_overlays
                .iter()
                .map(|p| ConfigFile::from_process_argument(p))
                .collect(),
            config_change_repo,
            hot_reload_enabled,
            pending: Mutex::new(None),
            builder,
            banner,
            config_warnings: ConfigWarnings::default(),
            last_content: Mutex::new(None),
            applied_layers: Mutex::new(startup_layers.clone()),
            startup_document: batlehub_config::merge_layers(&startup_layers)
                .map_err(|e| format!("{e:#}")),
            cluster: std::sync::OnceLock::new(),
        }
    }

    /// The warnings of the config currently in force.
    pub fn warnings(&self) -> Vec<ConfigWarning> {
        self.config_warnings.get()
    }

    /// Seed the warning store from the config loaded at startup. Reloads refresh
    /// it themselves, so this is only needed once.
    pub fn set_warnings(&self, warnings: Vec<ConfigWarning>) {
        self.config_warnings.replace(warnings);
    }

    /// Discards the pending reload without applying. Returns `true` if one existed.
    pub fn discard_pending(&self) -> bool {
        self.pending
            .lock()
            .expect("pending reload lock poisoned")
            .take()
            .is_some()
    }

    /// Returns a non-sensitive snapshot of the pending reload for the GET endpoint.
    pub fn pending_snapshot(&self) -> Option<PendingReloadSnapshot> {
        self.pending
            .lock()
            .expect("pending reload lock poisoned")
            .as_ref()
            .map(|p| PendingReloadSnapshot {
                id: p.id,
                created_at: p.created_at,
                expires_at: p.expires_at,
                source: p.source.clone(),
                diff: p.diff.clone(),
                warnings: p.warnings.clone(),
            })
    }

    /// Drops the pending reload if it has passed its expiry time.
    pub fn expire_pending_if_stale(&self) {
        let mut guard = self.pending.lock().expect("pending reload lock poisoned");
        if let Some(ref p) = *guard {
            if Utc::now() > p.expires_at {
                *guard = None;
                tracing::info!("pending reload expired and was discarded");
            }
        }
    }

    /// Convenience: `load_pending` + `apply` atomically (for the immediate reload endpoint).
    ///
    /// Answers with the diff `apply` returns, the one of the reload it applied:
    /// `load_pending`'s is empty when the watcher already staged these bytes.
    pub async fn reload_immediate(&self, triggered_by: &str) -> Result<ReloadDiff, anyhow::Error> {
        self.load_pending(ReloadSource::AdminRequest).await?;
        self.apply(triggered_by).await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(super) mod tests;
