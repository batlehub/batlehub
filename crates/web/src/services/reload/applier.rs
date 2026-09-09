use chrono::{DateTime, Utc};
use uuid::Uuid;

use batlehub_config::{load_from_str, load_layered_from_str};
use batlehub_core::entities::{BannerLevel, GlobalBanner};

use super::{
    ConfigReloadService, PendingReload, ReloadDiff, ReloadOutcome, ReloadSource, PENDING_TTL_SECS,
};

/// The 3 distinguishable failure modes of [`ConfigReloadService::apply`],
/// wrapped into the `anyhow::Error` it returns so callers that need to tell
/// them apart (e.g. `apply_pending_reload`'s HTTP status mapping) can
/// `downcast_ref` instead of substring-matching the error's `Display` text.
#[derive(Debug, thiserror::Error)]
pub enum ReloadApplyError {
    #[error("hot reload is disabled (BATLEHUB_DISABLE_HOT_RELOAD=1)")]
    Disabled,
    #[error("no pending reload")]
    NoPendingReload,
    #[error("pending reload expired")]
    Expired,
}

/// A row from the `config_changes` audit table.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ConfigChangeRow {
    pub id: Uuid,
    pub triggered_by: String,
    pub triggered_at: DateTime<Utc>,
    pub status: String,
    pub diff: serde_json::Value,
    pub summary: String,
    pub error_msg: Option<String>,
}

impl ConfigReloadService {
    /// Parse `primary` as the first layer, with the configured overlay files
    /// read fresh and merged over it.
    ///
    /// Every path that turns config text into an `AppConfig` goes through here,
    /// so the file watcher, the editor's validate button and the editor's save
    /// all see the same merged document. Reading the overlays here rather than
    /// caching them is what makes a credentials-only edit reload: the watcher
    /// fires on the overlay, the primary's bytes are unchanged, and the merge
    /// still produces a new config.
    ///
    /// With no overlays this is `load_from_str`, span-carrying error messages
    /// included.
    async fn load_layers(
        &self,
        primary: &str,
    ) -> Result<batlehub_config::AppConfig, anyhow::Error> {
        if self.config_overlays.is_empty() {
            return load_from_str(primary);
        }
        let mut layers = Vec::with_capacity(self.config_overlays.len() + 1);
        layers.push(primary.to_owned());
        for path in &self.config_overlays {
            layers.push(
                tokio::fs::read_to_string(path)
                    .await
                    .map_err(|e| anyhow::anyhow!("reading config overlay '{path}': {e}"))?,
            );
        }
        load_layered_from_str(&layers)
    }

    /// A fingerprint of every layer, for the byte-identical-rewrite dedup.
    ///
    /// The dedup exists because `touch` and atomic saves fire the watcher with
    /// unchanged bytes. With layers the question is whether *any* of them
    /// changed, so the primary's text alone would miss a credentials rotation
    /// and skip the rebuild that should have happened. The separator cannot
    /// appear in a TOML file, so two different splits cannot fingerprint alike.
    async fn layered_fingerprint(&self, primary: &str) -> String {
        let mut out = primary.to_owned();
        for path in &self.config_overlays {
            out.push('\0');
            match tokio::fs::read_to_string(path).await {
                Ok(content) => out.push_str(&content),
                // An unreadable overlay must not fingerprint like an empty one.
                // The comparison happens *before* `load_layers` runs, so a
                // deleted overlay that used to be empty would otherwise match
                // the previous fingerprint, return early as "nothing changed",
                // and never reach the error that should have been reported.
                Err(e) => {
                    out.push_str("\u{1}unreadable: ");
                    out.push_str(&e.to_string());
                }
            }
        }
        out
    }

    /// Re-reads the config file, validates, and builds a new HotConfig + AccessConfig.
    /// Stores the result as a pending reload (does NOT apply).
    /// Replaces any existing pending reload.
    pub async fn load_pending(&self, source: ReloadSource) -> Result<ReloadDiff, anyhow::Error> {
        if !self.hot_reload_enabled {
            anyhow::bail!("hot reload is disabled (BATLEHUB_DISABLE_HOT_RELOAD=1)");
        }
        let content = tokio::fs::read_to_string(&self.config_path).await?;
        if self.mark_seen_and_check_unchanged(&self.layered_fingerprint(&content).await) {
            // File-watcher fired (touch/atomic-save rewrite) but no layer's bytes
            // differ from the last load attempt — nothing to rebuild.
            return Ok(ReloadDiff::default());
        }
        let new_config = self.load_layers(&content).await?;
        self.build_pending(new_config, source).await
    }

    /// Validates a config TOML string without storing a pending reload.
    /// Returns the diff that would result from applying the new config, plus the
    /// warnings it would raise — so an admin sees them before committing to it.
    pub async fn validate_content(&self, content: &str) -> Result<ReloadOutcome, anyhow::Error> {
        if !self.hot_reload_enabled {
            anyhow::bail!("hot reload is disabled (BATLEHUB_DISABLE_HOT_RELOAD=1)");
        }
        let new_config = self.load_layers(content).await?;
        let built = (self.builder)(&new_config)?;
        Ok(ReloadOutcome {
            diff: self.compute_diff(&built.hot, &built.access).await,
            warnings: new_config.warnings(),
            // A dry run by contract: this endpoint answers "would this config be
            // accepted", and never stages anything to apply.
            pending_created: false,
        })
    }

    /// Same as `load_pending` but parses the config from a supplied TOML string
    /// instead of re-reading the file. Used by the config-editor endpoint.
    pub async fn load_pending_from_content(
        &self,
        content: &str,
        source: ReloadSource,
    ) -> Result<ReloadOutcome, anyhow::Error> {
        if !self.hot_reload_enabled {
            anyhow::bail!("hot reload is disabled (BATLEHUB_DISABLE_HOT_RELOAD=1)");
        }
        if self.mark_seen_and_check_unchanged(&self.layered_fingerprint(content).await) {
            // Byte-identical to the last load *attempt* — which is not necessarily
            // the config in force: the file watcher may have loaded this content
            // without it being applied yet. Skip the rebuild, but report the
            // warnings of the content submitted, not of the config it would
            // replace, or the editor shows an empty warning panel for a candidate
            // that has warnings.
            //
            // `pending_created: false` is the whole point of the flag: this call
            // looks like a success and returns an empty diff, but there is nothing
            // for the caller to apply afterwards. The dedup itself is aimed at the
            // file watcher, which really is fired repeatedly with identical bytes
            // by `touch` and atomic saves; the editor inherits it, where a human
            // pressing a button is never a spurious event and deserves to be told.
            return Ok(ReloadOutcome {
                diff: ReloadDiff::default(),
                warnings: self.load_layers(content).await?.warnings(),
                pending_created: false,
            });
        }
        let new_config = self.load_layers(content).await?;
        let warnings = new_config.warnings();
        let diff = self.build_pending(new_config, source).await?;
        // Store the raw content so apply() can persist it to disk.
        if let Some(ref mut p) = *self.pending.lock().expect("pending reload lock poisoned") {
            p.content = Some(content.to_owned());
        }
        Ok(ReloadOutcome {
            diff,
            warnings,
            pending_created: true,
        })
    }

    /// Read the current on-disk config content without parsing or validating it.
    /// Non-blocking: uses `tokio::fs` so it does not stall a tokio worker thread.
    pub async fn config_content(&self) -> Result<String, std::io::Error> {
        tokio::fs::read_to_string(&self.config_path).await
    }

    /// Records `content` as the last-seen raw config text and reports whether it is
    /// identical to the previous call's content. `false` on the very first call (there
    /// is nothing yet to compare against), so the first load after startup always goes
    /// through `build_pending` — matching the state the caller already has on disk.
    fn mark_seen_and_check_unchanged(&self, content: &str) -> bool {
        let mut last = self
            .last_content
            .lock()
            .expect("last_content lock poisoned");
        let unchanged = last.as_deref() == Some(content);
        *last = Some(content.to_owned());
        unchanged
    }

    /// Internal helper shared by `load_pending` and `load_pending_from_content`.
    async fn build_pending(
        &self,
        new_config: batlehub_config::AppConfig,
        source: ReloadSource,
    ) -> Result<ReloadDiff, anyhow::Error> {
        let built = (self.builder)(&new_config)?;
        let diff = self.compute_diff(&built.hot, &built.access).await;
        let warnings = new_config.warnings();
        let now = Utc::now();
        let pending = PendingReload {
            id: Uuid::new_v4(),
            created_at: now,
            expires_at: now + chrono::Duration::seconds(PENDING_TTL_SECS),
            source,
            diff: diff.clone(),
            content: None, // set by load_pending_from_content when originating from the editor
            new_hot: built.hot,
            new_access: built.access,
            new_search_readmes: built.search_readmes,
            new_registry_map: built.registry_map,
            new_registry_mode_map: built.registry_mode_map,
            new_upstream_map: built.upstream_map,
            new_cargo_index_map: built.cargo_index_map,
            new_repo_signer_map: built.repo_signer_map,
            new_vuln_db_map: built.vuln_db_map,
            new_sumdb_map: built.sumdb_map,
            new_registry_host_map: built.registry_host_map,
            new_proxy_trust: crate::middleware::ProxyTrust::from_config(
                new_config.effective_trusted_proxies(),
            ),
            warnings,
        };
        *self.pending.lock().expect("pending reload lock poisoned") = Some(pending);
        Ok(diff)
    }

    /// Applies the current pending reload: swaps hot config + access config, persists audit row,
    /// clears the pending state. Returns the diff that was applied.
    pub async fn apply(&self, triggered_by: &str) -> Result<ReloadDiff, anyhow::Error> {
        if !self.hot_reload_enabled {
            return Err(ReloadApplyError::Disabled.into());
        }
        let pending = self
            .pending
            .lock()
            .expect("pending reload lock poisoned")
            .take()
            .ok_or(ReloadApplyError::NoPendingReload)?;

        if Utc::now() > pending.expires_at {
            return Err(ReloadApplyError::Expired.into());
        }

        // Set "reload in progress" banner while swapping.
        if let Some(ref banner) = self.banner {
            let _ = banner
                .set(GlobalBanner {
                    message: "Configuration reload in progress…".to_owned(),
                    level: BannerLevel::Info,
                    set_at: Utc::now(),
                    set_by: "system".to_owned(),
                })
                .await;
        }

        // Replace HotConfig, AccessConfig, and all registry metadata maps.
        //
        // Each map below is swapped under its own lock, one after another — this is
        // *not* a single atomic transition. A request handled concurrently with this
        // method can observe the new HotConfig but an old registry_map (or any other
        // in-between combination), for the brief window between these blocks. Every
        // map converges to the new config by the time this function returns; the
        // gap is a request-scoped skew, not a lost update. If a handler ever needs
        // cross-map consistency within a single request, snapshot the specific maps
        // it depends on once at the top of the request instead of assuming reload
        // applies them all in one step.
        {
            let mut hot = self.hot.write().await;
            *hot = pending.new_hot;
        }
        {
            let mut access = self.access.write().await;
            *access = pending.new_access;
        }
        {
            // The **index** is deliberately not torn down when this goes off:
            // nothing reads it, and rebuilding it on the next flip would be a
            // surprise measured in minutes (RFC 0007-bis §9).
            let mut search = self.search.write().await;
            *search = pending.new_search_readmes;
        }
        // Swap the shared registry/mode/upstream maps in-place so all actix workers
        // immediately see the new registries without a process restart.
        //
        // Each `replace_from` call takes its own lock pair (read `pending`'s map,
        // write this one) — deliberate, matching `ConfigReloadService::pending`'s
        // policy (see its doc comment in `mod.rs`): a poisoned lock here means a
        // reload already panicked mid-swap, so crashing is preferred over serving
        // a torn config.
        self.registry_map.replace_from(&pending.new_registry_map);
        self.registry_mode_map
            .replace_from(&pending.new_registry_mode_map);
        self.upstream_map.replace_from(&pending.new_upstream_map);
        self.cargo_index_map
            .replace_from(&pending.new_cargo_index_map);
        // Swap the deb/rpm repo signing keys so a reload that adds/changes/removes
        // `[registries.repo_signing]` takes effect without a process restart.
        self.repo_signer_map
            .replace_from(&pending.new_repo_signer_map);
        // Swap the Go vuln DB URL map; reuse the existing HTTP client.
        self.vuln_db_map.replace_from(&pending.new_vuln_db_map);
        self.sumdb_map.replace_from(&pending.new_sumdb_map);
        // Proxy trust *before* the routing table it guards. These two swaps are not
        // atomic together (see this function's doc comment), and the order decides
        // which way the gap fails: trust first means a request landing mid-swap
        // sees the new, stricter policy against the old table — at worst a
        // forwarded host is ignored for a few microseconds. Table first would mean
        // the opposite: the new host bindings live under the old legacy-permissive
        // verdict, which is precisely the header-spoofing window
        // `validate_host_routing` exists to make unreachable.
        self.proxy_trust.replace_from(&pending.new_proxy_trust);
        // Swap the host -> registry routing table so a reload that adds, moves or
        // removes a `hosts` entry takes effect without a process restart.
        self.registry_host_map
            .replace_from(&pending.new_registry_host_map);
        // Refresh (and re-log) the config warnings so the admin endpoint describes
        // the config that is now in force, not the one it replaced.
        self.config_warnings.replace(pending.warnings.clone());

        // Clear the in-progress banner on success.
        if let Some(ref banner) = self.banner {
            let _ = banner
                .clear()
                .await
                .inspect_err(|e| tracing::warn!(error = %e, "failed to clear reload banner"));
        }

        // Write editor-submitted content back to disk so the change survives a restart.
        // File-watcher reloads set content = None (the file is already correct on disk).
        if let Some(ref text) = pending.content {
            if let Err(e) = self.persist_config_to_disk(text, pending.id).await {
                tracing::warn!(
                    path = %self.config_path,
                    error = %e,
                    "failed to persist editor config to disk; change is live in memory but will be lost on restart"
                );
            }
        }

        // Persist audit row (best-effort — do not fail the reload if DB is unavailable).
        self.persist_audit(&pending.diff, triggered_by, "applied", None)
            .await;

        Ok(pending.diff)
    }

    /// Replace the config file with `text`, atomically.
    ///
    /// The obvious `tokio::fs::write` truncates the file in place, so a process
    /// killed between the truncate and the last byte — a pod eviction during a
    /// config-editor save — leaves a half-written `config.toml` behind. The
    /// in-memory config dies with the process and the file that was supposed to
    /// carry it forward no longer parses, so the *next* boot fails. Writing a
    /// complete temp file and `rename`-ing it over the target removes that
    /// window: `rename(2)` within a filesystem is atomic, so a concurrent reader
    /// (the file watcher) and the next boot see either the whole old file or the
    /// whole new one, never a prefix.
    ///
    /// The temp file is created *beside* the target rather than in `/tmp`, because
    /// a rename across a mount point fails with `EXDEV` — and `/etc/batlehub`
    /// being its own mount is the normal container case, not an exotic one. It is
    /// named from the pending reload's id so two applies cannot collide on it, and
    /// it is removed on every failure path rather than left as litter next to the
    /// config.
    ///
    /// The directory is deliberately not fsynced. That would buy durability of the
    /// rename itself across a power cut, which is a different failure from the one
    /// this guards, and opening a directory as a file is not portable off Unix.
    async fn persist_config_to_disk(&self, text: &str, id: Uuid) -> std::io::Result<()> {
        let target = std::path::Path::new(&self.config_path);
        // `parent()` is `Some("")` for a bare relative name like `config.toml`;
        // joining onto that keeps the temp file in the current directory, which is
        // exactly where the target lives too.
        let dir = target.parent().unwrap_or_else(|| std::path::Path::new(""));
        let name = target
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("config.toml"));
        let tmp = dir.join(format!(".{}.{id}.tmp", name.to_string_lossy()));

        let staged = async {
            Self::write_and_sync(&tmp, text).await?;
            // Carry the target's mode onto the replacement. `rename` swaps in a
            // whole new inode, so without this the 0644 that `File::create`
            // produces would silently widen a `config.toml` an operator
            // deliberately keeps at 0600 — and this file holds `database.url`,
            // credentials included. The old in-place write had no such problem: it
            // truncated the existing inode and kept its mode. Missing metadata
            // means there is no prior file to inherit from (first write), which is
            // not an error; a *failed* `set_permissions` is, because proceeding
            // would publish the widened mode.
            if let Ok(meta) = tokio::fs::metadata(target).await {
                tokio::fs::set_permissions(&tmp, meta.permissions()).await?;
            }
            tokio::fs::rename(&tmp, target).await
        }
        .await;

        if staged.is_err() {
            // Best-effort: the write may have failed before the file existed.
            let _ = tokio::fs::remove_file(&tmp).await;
        }
        staged
    }

    /// Write `text` to `path` and flush it to the device before returning.
    ///
    /// The `sync_all` is what makes the rename in [`Self::persist_config_to_disk`]
    /// meaningful: without it the rename can publish the new inode while its data
    /// is still only in the page cache, which on a crash yields precisely the
    /// truncated file the atomic write exists to prevent.
    async fn write_and_sync(path: &std::path::Path, text: &str) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt as _;
        let mut file = tokio::fs::File::create(path).await?;
        file.write_all(text.as_bytes()).await?;
        file.sync_all().await
    }

    /// Returns the history of applied reloads from `config_changes`.
    pub async fn list_changes(
        &self,
        page: u64,
        per_page: u64,
    ) -> Result<Vec<ConfigChangeRow>, anyhow::Error> {
        let repo = self
            .config_change_repo
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("database not configured"))?;
        let records = repo.list(page, per_page).await?;
        Ok(records.into_iter().map(ConfigChangeRow::from).collect())
    }

    /// Total count of `config_changes` rows, ignoring pagination — backs
    /// `ConfigChangesResponse.total`.
    pub async fn count_changes(&self) -> Result<u64, anyhow::Error> {
        let repo = self
            .config_change_repo
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("database not configured"))?;
        Ok(repo.count().await?)
    }
}

impl From<batlehub_core::ports::ConfigChangeRecord> for ConfigChangeRow {
    fn from(r: batlehub_core::ports::ConfigChangeRecord) -> Self {
        ConfigChangeRow {
            id: r.id,
            triggered_by: r.triggered_by,
            triggered_at: r.triggered_at,
            status: r.status,
            diff: r.diff,
            summary: r.summary,
            error_msg: r.error_msg,
        }
    }
}
