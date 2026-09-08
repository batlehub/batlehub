use std::sync::Arc;
use std::time::Duration;

use actix_cors::Cors;
use actix_web::http;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{trace as sdktrace, Resource};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use batlehub_config::schema::{AppConfig, OtelConfig};
use batlehub_web::handlers::back_office::ops::warming::WarmingServiceMap;
use batlehub_web::services::ConfigReloadService;

// ── CORS ──────────────────────────────────────────────────────────────────────

/// The explicit opt-out for "any origin may read responses from this server".
pub(super) const CORS_WILDCARD: &str = "*";

/// Build the CORS policy from `[server].cors_allowed_origins`.
///
/// Three cases, and the first one changed in 1.1.0:
///
/// | `cors_allowed_origins` | Policy                                    |
/// |------------------------|-------------------------------------------|
/// | empty / unset          | same-origin only — **no** CORS headers    |
/// | `["*"]`                | any origin (explicit opt-out)             |
/// | `["https://ui.…", …]`  | exactly those origins                     |
///
/// **Breaking change.** An empty list used to mean `allow_any_origin()`, so any
/// website a user happened to visit could issue cross-origin requests to this
/// server and read the responses. Credentials are not allowed, so this was never
/// a token-theft path — but for a registry proxy sitting inside a private network
/// it meant a public page could enumerate internal package metadata using the
/// victim's browser as the network position. Defaulting to closed and requiring
/// `["*"]` to reopen it makes that a decision someone writes down.
///
/// Deployments serving the SPA from the same origin as the API — the default
/// layout, since the server hosts `ui/dist` itself — are unaffected either way:
/// same-origin requests never consult CORS.
pub(super) fn build_cors(allowed_origins: &[String]) -> Cors {
    let base = Cors::default()
        .allowed_methods(vec!["GET", "POST", "PUT", "HEAD", "OPTIONS", "DELETE"])
        .allowed_headers(vec![
            http::header::AUTHORIZATION,
            http::header::CONTENT_TYPE,
            http::header::ACCEPT,
        ])
        .max_age(3600);

    if allowed_origins.iter().any(|o| o == CORS_WILDCARD) {
        return base.allow_any_origin();
    }
    allowed_origins
        .iter()
        .fold(base, |c, origin| c.allowed_origin(origin))
}

// ── Startup warming ───────────────────────────────────────────────────────────

pub(super) fn spawn_startup_warming(config: &AppConfig, warming_map: &WarmingServiceMap) {
    for reg in &config.registries {
        if reg.cache.warm_packages.is_empty() && reg.cache.warm_paths.is_empty() {
            continue;
        }
        if let Some(svc) = warming_map.get(&reg.name) {
            let svc = Arc::clone(svc);
            let packages = reg.cache.warm_packages.clone();
            let paths = reg.cache.warm_paths.clone();
            let name = reg.name.clone();
            tokio::spawn(async move {
                tracing::info!(registry = %name, "warming: startup warming started");
                let mut report = svc.warm_all(&packages).await;
                report += svc.warm_all_paths(&paths).await;
                tracing::info!(
                    registry = %name,
                    warmed = report.warmed,
                    skipped = report.skipped,
                    errors = report.errors,
                    "warming: startup warming complete"
                );
            });
        }
    }
}

// ── Periodic release import ─────────────────────────────────────────────────

/// One task per `[[release_imports]]` that declares an `interval_secs`
/// (RFC 0021 §6.4).
///
/// The first run is immediate: an operator who has just written the block and
/// restarted wants the extension to be there, not to be there in an hour. Every
/// run after that is free when nothing has been released — a version the target
/// already holds is skipped, and for a kind whose coordinate is in the asset
/// name nothing is even downloaded.
///
/// Detached, like the startup warming above: an import that cannot reach its
/// forge must not stop the server from serving what it already holds.
pub(super) fn spawn_release_imports(
    config: &AppConfig,
    imports: &batlehub_web::handlers::back_office::ops::release_import::ReleaseImportMap,
) {
    for imp in &config.release_imports {
        let Some(secs) = imp.interval_secs.filter(|s| *s > 0) else {
            continue;
        };
        let Some(svc) = imports
            .get(&imp.into)
            .and_then(|v| v.iter().find(|s| s.repo == imp.repo))
            .cloned()
        else {
            continue;
        };
        let (into, repo) = (imp.into.clone(), imp.repo.clone());
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let report = svc.import().await;
                // Logged at info even when nothing happened: "the import ran
                // and found nothing new" and "the import has not run" are the
                // two states an operator needs to tell apart, and only one of
                // them is a problem.
                tracing::info!(
                    registry = %into, repo = %repo,
                    imported = report.imported,
                    skipped = report.skipped,
                    errors = report.errors,
                    "release import: scheduled run complete"
                );
                for failure in &report.failures {
                    tracing::warn!(
                        registry = %into, repo = %repo,
                        tag = %failure.tag, asset = %failure.asset, error = %failure.error,
                        "release import: asset did not import"
                    );
                }
            }
        });
    }
}

// ── Periodic vulnerability scan ─────────────────────────────────────────────────

/// Spawn a background task that re-checks all cached SBOMs against the OSV
/// vulnerability database: once shortly after startup, then every
/// `interval_secs`. Mirrors `spawn_startup_warming` — a detached `tokio::spawn`
/// that logs a summary per run.
pub(super) fn spawn_periodic_vuln_scan(
    interval_secs: u64,
    scan_svc: Arc<batlehub_core::services::VulnerabilityScanService>,
) {
    let period = Duration::from_secs(interval_secs.max(1));
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        loop {
            ticker.tick().await;
            tracing::info!("vuln-scan: starting periodic SBOM re-check");
            match scan_svc.scan_all().await {
                Ok(report) => tracing::info!(
                    scanned = report.scanned,
                    findings = report.findings,
                    errors = report.errors,
                    "vuln-scan: periodic re-check complete"
                ),
                Err(e) => tracing::warn!(error = %e, "vuln-scan: periodic re-check failed"),
            }
        }
    });
}

// ── Periodic cache-coherence sweep ──────────────────────────────────────────────

/// Spawn a background task that collects storage blobs no artifact-meta row
/// points at: once `interval_secs` after startup, then every `interval_secs`.
///
/// # Why the first tick is skipped
///
/// `tokio::time::interval` fires immediately, which is right for the vulnerability
/// scan above — a scan that reports is a read. This one deletes, and a sweep at
/// second zero of a process start is the worst possible moment for it: a restart
/// is exactly when half-finished cache writes exist, and the pending set that
/// makes the two-pass grace safe is empty in a fresh process. So the first tick
/// is consumed before the loop, and the first sweep an instance runs is one
/// interval in — by which time anything that was mid-write has its row.
///
/// The grace itself still holds regardless: the sweep carries a blob forward on
/// its first sighting and deletes it on the second, so nothing this task
/// collects has been orphaned for less than one interval.
///
/// # It shares the services the endpoint uses
///
/// The same `Arc<EvictionService>` values as `POST /registries/{r}/coherence`,
/// deliberately: `coherence_pending` is the state the grace is built on, and a
/// scheduler with services of its own would keep a second, private set — a
/// manual sweep and a scheduled one would each be permanently on their own
/// first pass, and neither would ever delete anything.
pub(super) fn spawn_periodic_coherence_sweep(
    interval_secs: u64,
    eviction_map: batlehub_web::handlers::back_office::ops::eviction::EvictionServiceMap,
) {
    let period = Duration::from_secs(interval_secs.max(1));
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        // See above: never at second zero.
        ticker.tick().await;
        // A sweep that overran its interval must not start again the instant it
        // finishes — `Delay` skips the missed ticks instead of bursting.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            for (registry, svc) in &eviction_map {
                // `Identity::system()` is what puts `user_id = "system"` on the
                // audit row, and it is the only thing that separates a
                // scheduled sweep from an operator who ran one.
                match svc
                    .run_coherence_check(false, &batlehub_core::entities::Identity::system())
                    .await
                {
                    Ok(report) => tracing::info!(
                        registry,
                        storage_keys = report.storage_keys,
                        meta_rows = report.meta_rows,
                        deleted = report.orphaned_deleted,
                        first_seen = report.first_seen_orphaned,
                        "coherence: periodic sweep complete"
                    ),
                    // One registry's storage being unreachable must not stop the
                    // others, and must not kill the task: the next tick tries
                    // again.
                    Err(e) => {
                        tracing::warn!(registry, error = %e, "coherence: periodic sweep failed")
                    }
                }
            }
        }
    });
}

/// Spawn the rescan scheduler (RFC 0018 phase 4): one tick a minute on
/// every worker process; the tick does nothing unless this process holds
/// the estate's advisory lock. First tick after one period, so a restart
/// storm does not become a queue storm.
pub(super) fn spawn_rescan_scheduler(scheduler: Arc<batlehub_core::services::RescanScheduler>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(batlehub_core::services::RESCAN_TICK);
        ticker.tick().await;
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match scheduler.run_once(chrono::Utc::now()).await {
                Ok(r) if r.leader && r.queued > 0 => {
                    metrics::counter!("batlehub_rescans_queued_total").increment(r.queued as u64);
                    tracing::info!(queued = r.queued, due = ?r.due, "security: rescans queued");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "security: rescan tick failed"),
            }
        }
    });
}

/// Spawn the upstream audit (RFC 0014 §6.10): one sweep per interval, on
/// the worker role. Copies `spawn_periodic_coherence_sweep`'s first tick —
/// never at second zero, so `skip_recently_seen` has a picture to compare
/// against and a restart storm does not become a probe storm.
pub(super) fn spawn_upstream_audit(
    interval_secs: u64,
    svc: Arc<batlehub_core::services::UpstreamAuditService>,
) {
    let period = Duration::from_secs(interval_secs.max(1));
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        ticker.tick().await;
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let report = svc.run_sweep().await;
            for r in &report.registries {
                record_sweep(&svc, r).await;
            }
        }
    });
}

/// One registry's sweep: its counters, its line in the log, and a line per
/// transition.
async fn record_sweep(
    svc: &Arc<batlehub_core::services::UpstreamAuditService>,
    r: &batlehub_core::services::upstream_audit::RegistryReport,
) {
    let outcome = if r.void { "void" } else { "ok" };
    metrics::counter!(
        "batlehub_upstream_audit_sweeps_total",
        "registry" => r.registry.clone(),
        "outcome" => outcome
    )
    .increment(1);
    metrics::histogram!(
        "batlehub_upstream_audit_duration_seconds",
        "registry" => r.registry.clone()
    )
    .record(r.duration.as_secs_f64());
    if let Ok((missing, disappeared)) = svc.counts(&r.registry).await {
        metrics::gauge!("batlehub_upstream_missing_total", "registry" => r.registry.clone())
            .set(missing as f64);
        metrics::gauge!("batlehub_upstream_disappeared_total", "registry" => r.registry.clone())
            .set(disappeared as f64);
    }
    if r.void {
        // A registry that is void every cycle is itself an alert, not a silent
        // gap in coverage.
        tracing::warn!(
            registry = %r.registry,
            probed = r.probed,
            missing = r.missing,
            "upstream audit: sweep VOID — too many misses to be unpublishes; nothing recorded"
        );
    } else {
        tracing::info!(
            registry = %r.registry,
            probed = r.probed,
            missing = r.missing,
            inconclusive = r.inconclusive,
            skipped_recent = r.skipped_recent,
            confirmed = r.confirmed(),
            reappeared = r.reappeared(),
            capped = r.capped_packages,
            "upstream audit: sweep complete"
        );
    }
    for t in &r.transitions {
        log_transition(t);
    }
}

/// A confirmed disappearance, or a reappearance that lifts the hold.
fn log_transition(t: &batlehub_core::services::Transition) {
    match t {
        batlehub_core::services::Transition::Confirmed(row, versions) => {
            tracing::warn!(
                registry = %row.registry,
                package = %row.package_name,
                version = ?row.version,
                versions = versions.len(),
                misses = row.consecutive_misses,
                first_missed_at = %row.first_missed_at,
                "upstream audit: DISAPPEARED upstream — held from eviction"
            );
        }
        batlehub_core::services::Transition::Reappeared(row, _) => {
            tracing::info!(
                registry = %row.registry,
                package = %row.package_name,
                version = ?row.version,
                "upstream audit: reappeared upstream — hold released"
            );
        }
    }
}

/// Spawn the cache-statistics rollup (RFC 0004 §6.4, R9).
///
/// The stored **resolution** is fixed at one hour rather than configured: it is
/// the granularity the data is *kept* at, and a deployment wanting daily figures
/// aggregates on read — daily figures can always be derived from hourly ones,
/// never recovered from them.
///
/// The **write cadence** is deliberately shorter than that resolution. Each tick
/// writes the counter delta since the previous tick and stamps it with
/// `hour_start(now)`, so the delta and the stamp only describe the same interval
/// when the tick is short relative to the hour. Ticking hourly from process
/// start meant an instance booted at 09:05 filed all of 09:05–10:05 under the
/// 10:00 bucket, permanently reporting every hour's traffic an hour late; at
/// five minutes the attribution error is bounded by the tick, and a restart
/// costs at most that much rather than a whole window.
///
/// Correctness here depends on `StatsHistoryRepository::append` **accumulating**
/// on `(registry, window_start)` — twelve ticks land in each hourly bucket, and
/// a replacing upsert would keep only the last one.
///
/// The `cached_bytes` **measurement** keeps the hourly cadence, though, and does
/// not follow the write cadence down. `stat_by_prefix` enumerates every cached
/// object of a registry — a paginated S3 `ListObjectsV2` over the whole prefix,
/// or a full directory walk with a `stat` per file — and it is a level, not a
/// delta, so measuring it twelve times an hour would multiply that scan by
/// twelve to store the same number. Between measurements the last one is
/// re-sent rather than dropped: `StatsRollupService::tick` reads the map with
/// `unwrap_or(0)` and `cached_bytes` is *replaced* on conflict, so an absent
/// entry would write a zero over a good level and show the cache as empty.
///
/// The first tick fires immediately (tokio's `interval` does), which is what
/// makes a freshly started instance record its startup window rather than
/// nothing at all.
pub(super) fn spawn_stats_rollup(
    rollup: Arc<batlehub_core::services::StatsRollupService>,
    proxy_svc: Arc<batlehub_core::services::ProxyService>,
    registries: Vec<String>,
) {
    /// How often the counters are read and a delta written.
    const TICK: Duration = Duration::from_secs(300);
    /// How many ticks pass between two storage measurements — one hour's worth,
    /// the resolution `cached_bytes` is stored at.
    const TICKS_PER_MEASUREMENT: u32 = 12;

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        // `cached_bytes` is a level read from storage, not something the
        // counters know; the rollup takes it as input rather than reaching
        // into a storage backend from `core`.
        let mut cached: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        // `TICKS_PER_MEASUREMENT` so the first tick measures rather than
        // reporting an empty cache for the startup window.
        let mut ticks_since_measurement = TICKS_PER_MEASUREMENT;

        loop {
            ticker.tick().await;

            if ticks_since_measurement >= TICKS_PER_MEASUREMENT {
                ticks_since_measurement = 0;
                for registry in &registries {
                    let prefix = format!("artifact:{registry}/");
                    match proxy_svc.storage.stat_by_prefix(&prefix).await {
                        Ok((_, bytes)) => {
                            cached.insert(registry.clone(), bytes);
                        }
                        // Keep the previous measurement rather than dropping the
                        // entry: a missing key records 0, and a replacing upsert
                        // makes one failed listing read back as "cache emptied".
                        Err(e) => tracing::warn!(
                            registry = %registry,
                            error = %e,
                            "stats-rollup: cached-size measurement failed, reusing the last one"
                        ),
                    }
                }
            }
            ticks_since_measurement += 1;

            match rollup.tick_now(&cached).await {
                Ok(n) => tracing::debug!(rows = n, "stats-rollup: window recorded"),
                // A failed rollup costs one tick of chart, never a request:
                // this is a detached task and nothing on the request path
                // waits for it.
                Err(e) => tracing::warn!(error = %e, "stats-rollup: window failed"),
            }
        }
    });
}

// ── Config file watcher ───────────────────────────────────────────────────────

/// OS-thread body: owns the blocking `notify` watcher and forwards change events
/// to the async side via `event_tx`. Exits when `event_tx` is closed.
/// The file names a directory event has to mention to be worth a reload.
///
/// Names rather than whole paths, because the event for a Kubernetes mount
/// names the symlink target's directory rather than the path this process was
/// given, and because every watched directory is a config directory — a file
/// called `config.toml` in one of them is a config file by construction.
fn watched_names(config_paths: &[String]) -> std::collections::HashSet<std::ffi::OsString> {
    let mut names: std::collections::HashSet<std::ffi::OsString> = config_paths
        .iter()
        .filter_map(|p| {
            std::path::Path::new(p)
                .file_name()
                .map(std::ffi::OsStr::to_os_string)
        })
        .collect();
    // The atomically-swapped symlink a ConfigMap or Secret projection updates
    // through. The per-key symlinks beside it are not touched by an update, so
    // this is the only name an event carries when the mounted content changes.
    names.insert(std::ffi::OsString::from("..data"));
    names
}

/// Whether a watcher event mentions a file worth reloading for.
///
/// An `Err` event is treated as relevant: the watcher lost track of something,
/// and re-reading the config is cheap next to running on a version of it that
/// may have moved on. The reload path's own dedup drops it again if nothing
/// actually changed.
fn is_relevant(
    event: &notify::Result<notify::Event>,
    names: &std::collections::HashSet<std::ffi::OsString>,
) -> bool {
    match event {
        Ok(event) => event
            .paths
            .iter()
            .filter_map(|p| p.file_name())
            .any(|name| names.contains(name)),
        Err(_) => true,
    }
}

fn run_watcher_thread(config_paths: Vec<String>, event_tx: tokio::sync::mpsc::UnboundedSender<()>) {
    use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc::channel;

    let (notify_tx, notify_rx) = channel();
    let mut watcher = match RecommendedWatcher::new(
        notify_tx,
        NotifyConfig::default().with_poll_interval(Duration::from_secs(2)),
    ) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = %e, "config file watcher init failed");
            return;
        }
    };
    // Every layer, not just the primary: a credentials rotation touches only the
    // overlay, and a watcher blind to it would leave the process running with
    // the old secret until something else happened to rewrite the main file.
    //
    // Each layer's *directory* is watched, not the file. inotify resolves a
    // path to an inode at watch time, and the two ways a config file actually
    // changes in production both replace that inode rather than writing through
    // it: an atomic save renames a new file over the old one (this process does
    // it itself, in `persist_config_to_disk`), and a Kubernetes ConfigMap or
    // Secret mount swaps the `..data` symlink the file points through. Watching
    // the file means the watch is silently attached to an inode nothing writes
    // to again — the first rewrite is missed, and every one after it, which is
    // the difference between hot reload working under the Helm chart and only
    // appearing to.
    //
    // Directories are deduplicated: two layers in `/etc/batlehub` are one watch,
    // and one event, rather than two reloads of the same change.
    //
    // A directory that cannot be watched is logged and skipped rather than
    // fatal. Returning here would take the watcher down for the *other* layers
    // too, so one unwatchable path would silently cost hot reload on all of
    // them; the reload path re-reads every layer anyway, so a change to a
    // watched file still picks up whatever the unwatched one now says.
    let mut watched = 0usize;
    let mut seen_dirs = std::collections::HashSet::new();
    for path in &config_paths {
        // `parent()` is `Some("")` for a bare relative name like `config.toml`,
        // which is not a directory anything can watch — the file is in the
        // current one.
        let dir = match std::path::Path::new(path).parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => std::path::PathBuf::from("."),
        };
        if !seen_dirs.insert(dir.clone()) {
            watched += 1;
            continue;
        }
        match watcher.watch(&dir, RecursiveMode::NonRecursive) {
            Ok(()) => watched += 1,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    dir = %dir.display(),
                    "config file watcher: failed to watch the directory of {path}"
                );
            }
        }
    }
    if watched == 0 {
        tracing::error!("config file watcher: no config file could be watched");
        return;
    }
    tracing::info!(paths = %config_paths.join(", "), "config file watcher started");

    // The watch is on directories, so most of what arrives is about files this
    // process does not care about — its own atomic-save temp file, and in a
    // development checkout (where the config sits in the working directory)
    // every artefact a build writes. Without this filter, one `cargo build`
    // beside a `config.toml` is a reload storm.
    let names = watched_names(&config_paths);

    loop {
        match notify_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(first) => {
                // Drain the burst before deciding: an atomic save arrives as
                // several events and a ConfigMap update as a dozen, and they
                // are one change. Deciding on the first alone would send a
                // reload for a temp-file creation and then swallow the rename
                // that actually mattered.
                let mut relevant = is_relevant(&first, &names);
                while let Ok(event) = notify_rx.try_recv() {
                    relevant |= is_relevant(&event, &names);
                }
                if relevant && event_tx.send(()).is_err() {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if event_tx.is_closed() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    tracing::info!("config file watcher stopped");
}

/// Circuit breaker for `run_reload_task`: trips when too many file-change events
/// land within a sliding window (e.g. a broken sync tool or NFS mount hammering
/// mtime), so a noisy filesystem can't turn into a reload-parsing busy loop.
// ponytail: fixed threshold/window, make configurable if a real deployment needs tuning.
struct ReloadEventLimiter {
    max_events: u32,
    window: Duration,
    window_start: std::time::Instant,
    count: u32,
}

impl ReloadEventLimiter {
    fn new(max_events: u32, window: Duration) -> Self {
        Self {
            max_events,
            window,
            window_start: std::time::Instant::now(),
            count: 0,
        }
    }

    /// Records one event at `now`. Returns `true` once this event pushes the
    /// window's count past `max_events` (i.e. the caller should stop watching).
    fn record_and_check_tripped(&mut self, now: std::time::Instant) -> bool {
        if now.duration_since(self.window_start) > self.window {
            self.window_start = now;
            self.count = 0;
        }
        self.count += 1;
        self.count > self.max_events
    }
}

/// Async task body: receives file-change notifications and triggers config reloads.
async fn run_reload_task(
    reload_svc: Arc<ConfigReloadService>,
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
) {
    use batlehub_web::services::ReloadSource;

    let mut limiter = ReloadEventLimiter::new(5, Duration::from_secs(30));

    while let Some(()) = event_rx.recv().await {
        if limiter.record_and_check_tripped(std::time::Instant::now()) {
            tracing::error!(
                max_events = limiter.max_events,
                window_secs = limiter.window.as_secs(),
                "config file watcher: too many reload events in a short time, \
                 disabling automatic reload detection (restart the server to re-enable; \
                 use POST /api/v1/admin/config/reload to reload manually)"
            );
            break;
        }

        tracing::info!("config file changed, loading pending reload");
        match reload_svc.load_pending(ReloadSource::FileWatcher).await {
            Ok(diff) if diff.is_noop() => {
                tracing::debug!("config file event produced no change, nothing pending")
            }
            Ok(diff) => tracing::info!(
                added = diff.added_registries.len(),
                removed = diff.removed_registries.len(),
                "pending reload ready — confirm at POST /api/v1/admin/config/pending/apply"
            ),
            Err(e) => tracing::warn!(error = %e, "config file reload validation failed"),
        }
    }
    reload_svc.expire_pending_if_stale();
    tracing::debug!("config reload task exiting");
}

pub(super) fn spawn_config_watcher(
    config_paths: Vec<String>,
    reload_svc: Arc<ConfigReloadService>,
) {
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    std::thread::Builder::new()
        .name("config-watcher".to_owned())
        .spawn(move || run_watcher_thread(config_paths, event_tx))
        .expect("failed to spawn config-watcher thread");

    tokio::spawn(run_reload_task(reload_svc, event_rx));
}

// ── Tracing ───────────────────────────────────────────────────────────────────

/// Initialise tracing. Returns the `TracerProvider` when OTLP is configured
/// so the caller can keep it alive for the process lifetime and flush on exit.
pub(super) fn init_tracing(otel_cfg: Option<&OtelConfig>) -> Option<sdktrace::SdkTracerProvider> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let (otel_layer, provider) = match otel_cfg {
        Some(cfg) => match build_otlp_provider(cfg) {
            Ok(p) => {
                use opentelemetry::trace::TracerProvider as _;
                let tracer = p.tracer(cfg.service_name.clone());
                let layer = tracing_opentelemetry::layer().with_tracer(tracer);
                (Some(layer), Some(p))
            }
            Err(e) => {
                eprintln!("WARN: failed to build OTLP exporter: {e}");
                (None, None)
            }
        },
        None => (None, None),
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer())
        .with(otel_layer)
        .init();

    provider
}

fn build_otlp_provider(cfg: &OtelConfig) -> anyhow::Result<sdktrace::SdkTracerProvider> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&cfg.endpoint)
        .build()?;

    let resource = Resource::builder_empty()
        .with_service_name(cfg.service_name.clone())
        .build();

    Ok(sdktrace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;
    // `actix_web::test` is both a module and an attribute macro, so importing it
    // unqualified shadows the built-in `#[test]` attribute for this whole module
    // and the plain sync tests below stop compiling. Alias it.
    use actix_web::test as actix_test;
    use actix_web::{dev::Service, http::header, web, App, HttpResponse};

    // ── Directory watching and its event filter ───────────────────────────────
    //
    // The watch is on directories rather than files, because both ways a config
    // file actually changes replace its inode: an atomic rename, and the
    // `..data` symlink swap a Kubernetes ConfigMap or Secret mount uses. That
    // makes the filter load-bearing — a directory fires on everything in it,
    // and in a development checkout the config sits in the working directory.

    fn event_for(paths: &[&str]) -> notify::Result<notify::Event> {
        Ok(notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: paths.iter().map(std::path::PathBuf::from).collect(),
            attrs: Default::default(),
        })
    }

    #[test]
    fn an_event_naming_a_config_file_is_relevant() {
        let names = watched_names(&["/etc/batlehub/config.toml".to_owned()]);
        assert!(is_relevant(
            &event_for(&["/etc/batlehub/config.toml"]),
            &names
        ));
    }

    /// The overlay is the file a credentials rotation touches, and it is the
    /// one a file-name filter written for the primary alone would drop.
    #[test]
    fn an_event_naming_an_overlay_is_relevant() {
        let names = watched_names(&[
            "/etc/batlehub/config.toml".to_owned(),
            "/etc/batlehub/credentials/credentials.toml".to_owned(),
        ]);
        assert!(is_relevant(
            &event_for(&["/etc/batlehub/credentials/credentials.toml"]),
            &names
        ));
    }

    /// What a ConfigMap or Secret projection actually reports when its content
    /// changes: the per-key symlinks are untouched, only `..data` is swapped.
    /// Without this the chart's hot reload would be silently dead.
    #[test]
    fn the_kubernetes_data_symlink_swap_is_relevant() {
        let names = watched_names(&["/etc/batlehub/config.toml".to_owned()]);
        assert!(is_relevant(&event_for(&["/etc/batlehub/..data"]), &names));
    }

    /// The reason the filter exists. A directory watch on a development
    /// checkout sees every build artefact, and each one used to be a reload.
    #[test]
    fn an_unrelated_file_in_a_watched_directory_is_ignored() {
        let names = watched_names(&["config.toml".to_owned()]);
        assert!(!is_relevant(&event_for(&["./Cargo.lock"]), &names));
        assert!(!is_relevant(&event_for(&["./notes.txt"]), &names));
    }

    /// This process writes `.config.toml.<uuid>.tmp` beside the target and
    /// renames it over. The temp file must not trigger anything; the rename to
    /// `config.toml` is what does.
    #[test]
    fn the_atomic_save_temp_file_is_ignored_but_the_rename_is_not() {
        let names = watched_names(&["/etc/batlehub/config.toml".to_owned()]);
        let id = "0b57a1a2-0000-4000-8000-000000000000";
        assert!(!is_relevant(
            &event_for(&[&format!("/etc/batlehub/.config.toml.{id}.tmp")]),
            &names
        ));
        assert!(is_relevant(
            &event_for(&["/etc/batlehub/config.toml"]),
            &names
        ));
    }

    /// A burst that mentions the config among other files is one relevant
    /// change, not none.
    #[test]
    fn a_mixed_event_is_relevant() {
        let names = watched_names(&["/etc/batlehub/config.toml".to_owned()]);
        assert!(is_relevant(
            &event_for(&["/etc/batlehub/unrelated", "/etc/batlehub/config.toml"]),
            &names
        ));
    }

    /// A watcher error means it lost track of something. Re-reading is cheap
    /// next to running on a config that may have moved on, and the reload
    /// path's own dedup drops it again if nothing changed.
    #[test]
    fn a_watcher_error_is_treated_as_relevant() {
        let names = watched_names(&["/etc/batlehub/config.toml".to_owned()]);
        let err = Err(notify::Error::generic("watch lost"));
        assert!(is_relevant(&err, &names));
    }

    /// Send a cross-origin GET and report the `Access-Control-Allow-Origin` the
    /// policy produced, if any. That header is what actually decides whether a
    /// browser hands the response body to the calling page.
    async fn allow_origin_for(allowed: &[String], request_origin: &str) -> Option<String> {
        let app = actix_test::init_service(
            App::new()
                .wrap(build_cors(allowed))
                .route("/", web::get().to(|| async { HttpResponse::Ok().finish() })),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/")
            .insert_header((header::ORIGIN, request_origin))
            .to_request();

        app.call(req).await.ok().and_then(|resp| {
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        })
    }

    /// The 1.1.0 behaviour change: an empty list is same-origin only. Before, it
    /// meant `allow_any_origin()`.
    #[actix_web::test]
    async fn empty_list_allows_no_cross_origin_reader() {
        assert_eq!(allow_origin_for(&[], "https://evil.example").await, None);
    }

    /// `allow_any_origin()` echoes the requesting origin rather than emitting a
    /// literal `*` — either form tells the browser the response is readable, so
    /// the assertion is "the caller's own origin came back", not "we saw a star".
    #[actix_web::test]
    async fn wildcard_is_the_explicit_opt_out() {
        assert_eq!(
            allow_origin_for(&[CORS_WILDCARD.to_owned()], "https://anywhere.example").await,
            Some("https://anywhere.example".to_owned()),
        );
    }

    #[actix_web::test]
    async fn listed_origin_is_allowed_and_others_are_not() {
        let allowed = vec!["https://ui.example".to_owned()];
        assert_eq!(
            allow_origin_for(&allowed, "https://ui.example").await,
            Some("https://ui.example".to_owned()),
        );
        assert_eq!(
            allow_origin_for(&allowed, "https://evil.example").await,
            None
        );
    }

    /// A wildcard mixed into a list of real origins still opens everything, so it
    /// must be detected wherever it appears — otherwise the config warning and
    /// the actual policy would disagree.
    #[actix_web::test]
    async fn wildcard_anywhere_in_the_list_wins() {
        let allowed = vec!["https://ui.example".to_owned(), CORS_WILDCARD.to_owned()];
        assert_eq!(
            allow_origin_for(&allowed, "https://evil.example").await,
            Some("https://evil.example".to_owned()),
            "a wildcard mixed into the list must still open the policy, or the \
             config warning and the real behaviour would disagree",
        );
    }

    #[test]
    fn limiter_trips_after_max_events_within_window() {
        let mut limiter = ReloadEventLimiter::new(5, Duration::from_secs(30));
        let t0 = std::time::Instant::now();

        for _ in 0..5 {
            assert!(!limiter.record_and_check_tripped(t0));
        }
        assert!(limiter.record_and_check_tripped(t0));
    }

    #[test]
    fn limiter_resets_count_once_window_elapses() {
        let mut limiter = ReloadEventLimiter::new(5, Duration::from_secs(30));
        let t0 = std::time::Instant::now();

        for _ in 0..5 {
            assert!(!limiter.record_and_check_tripped(t0));
        }
        let after_window = t0 + Duration::from_secs(31);
        assert!(!limiter.record_and_check_tripped(after_window));
    }
}
