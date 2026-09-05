mod builders;
mod explain;
mod grants;
mod hot_config;
mod server_factory;
mod setup;
mod stores;
mod watcher;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use metrics_exporter_prometheus::PrometheusBuilder;

use batlehub_adapters::db::{
    PgArtifactMetaRepository, PgBetaChannelStore, PgConfigChangeRepository, PgGrantRepository,
    PgOwnershipStore, PgPackageRepository, PgStorageAdminRepository, PgTeamNamespaceStore,
    PgVulnerabilityRepository,
};
use batlehub_adapters::local_registry::PostgresLocalRegistry;
use batlehub_adapters::vulnerability::OsvScanner;
use batlehub_core::ports::{BetaChannelPort, UserTokenRepository, VulnerabilityRepository};
use batlehub_core::services::{
    new_hot_lock, AdminService, LocalRegistryService, ProxyMetrics, ProxyService,
    VulnerabilityScanService,
};
use batlehub_web::services::{BannerService, ConfigReloadParams, ConfigReloadService};
use batlehub_web::{new_access_lock, openapi_spec, RateLimitService};

use crate::explain::explain_config;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "batlehub",
    about = "BatleHub — smart artifact hub for package registries"
)]
struct Cli {
    #[arg(short, long)]
    config: Option<String>,

    /// What this process does (RFC 0018 §4.1): `proxy`, `worker`, or
    /// `proxy,worker`. Overrides `[server].roles`; absent means the config's
    /// value, which defaults to both.
    #[arg(long, value_delimiter = ',')]
    roles: Option<Vec<String>>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the OpenAPI spec to stdout and exit (for frontend code generation).
    DumpSpec,
    /// Hash a plain-text token with Argon2id and print the result.
    HashToken {
        /// The plain-text token to hash.
        token: String,
    },
    /// Print the permissions each subject holds on each registry, expanded.
    ///
    /// RFC 0015 §4.2 moves wildcard expansion to config load, so what a `"*"`
    /// covers is a fact about the loaded model rather than something implied at
    /// each decision. This is what makes it visible — an expansion nobody can
    /// print is only half of that property.
    ExplainConfig {
        /// Config file to read. Defaults to the `--config` argument.
        path: Option<String>,
    },
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::DumpSpec) => {
            let spec = openapi_spec();
            println!("{}", spec.to_pretty_json().expect("serialize openapi spec"));
            return Ok(());
        }
        Some(Command::HashToken { token }) => {
            println!("{}", batlehub_adapters::auth::hash_static_token(&token));
            return Ok(());
        }
        Some(Command::ExplainConfig { ref path }) => {
            let path = path
                .clone()
                .or_else(|| cli.config.clone())
                .unwrap_or_else(|| "config.toml".to_owned());
            explain_config(&path)?;
            return Ok(());
        }
        None => {}
    }

    let config_path = cli
        .config
        .or_else(|| std::env::var("BATLEHUB_CONFIG").ok())
        .unwrap_or_else(|| "config.toml".to_string());
    let mut config = batlehub_config::load(&config_path)
        .with_context(|| format!("loading config from '{config_path}'"))?;
    if let Some(roles) = cli.roles {
        let parsed = roles
            .iter()
            .map(|r| r.parse::<batlehub_config::schema::ProcessRole>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(anyhow::Error::msg)
            .context("--roles")?;
        if parsed.is_empty() {
            anyhow::bail!("--roles: a process that is neither proxy nor worker does nothing");
        }
        config.server.roles = parsed;
    }
    let roles = config.server.roles.clone();
    let is_proxy = roles.contains(&batlehub_config::schema::ProcessRole::Proxy);
    let is_worker = roles.contains(&batlehub_config::schema::ProcessRole::Worker);
    tracing::info!(?roles, "process roles");

    // `/metrics` is unauthenticated and was, until RFC 0004, unconditional —
    // it publishes cache hit rates, per-registry pull volumes and upstream
    // latencies to anyone who can reach the port. Consulting config here is
    // what makes `[stats] metrics_enabled = false` mean anything, and what
    // makes the handler's existing "metrics not configured" branch reachable
    // in a real server for the first time rather than only in tests.
    let prometheus_handle = if config.stats.metrics_enabled {
        Some(
            PrometheusBuilder::new()
                .install_recorder()
                .context("installing Prometheus metrics recorder")?,
        )
    } else {
        tracing::info!("[stats] metrics_enabled = false — /metrics will report 503");
        None
    };

    let _tracer_provider = watcher::init_tracing(config.otel.as_ref());
    tracing::info!(config = %config_path, "batlehub starting");

    let repo = Arc::new(
        PgPackageRepository::new(
            &config.database.url,
            batlehub_adapters::db::packages::PoolOptions {
                max_connections: config.database.max_connections,
                min_connections: config.database.min_connections,
                acquire_timeout_secs: config.database.acquire_timeout_secs,
            },
        )
        .await
        .context("connecting to database")?,
    );
    repo.run_migrations().await.context("running migrations")?;
    stores::spawn_db_pool_gauge_sampler(repo.pool());

    let storage = setup::initialize_storage(&config, repo.pool()).await?;
    let setup::AuthSetup {
        providers: mut auth_providers,
        sso_flows: oidc_sso_flows,
        oidc_provider_names,
    } = setup::initialize_auth_providers(&config).await?;
    let token_repo = repo.clone() as Arc<dyn UserTokenRepository>;
    setup::add_user_token_provider(&mut auth_providers, token_repo.clone());

    // Postgres-backed rather than Redis: the server always has a database, a
    // login writes one row and deletes it, and `DELETE … RETURNING` gives the
    // one-time-use guarantee for free across replicas.
    let login_states = repo.clone() as Arc<dyn batlehub_core::ports::LoginStateStore>;
    setup::spawn_login_state_prune(Arc::clone(&login_states));

    let cache = stores::create_cache_store(&config, repo.pool()).await?;
    let cargo_index_map = setup::build_initial_cargo_index_map(&config)?;

    let rate_limit_configs: HashMap<_, _> = config
        .registries
        .iter()
        .filter_map(|r| r.rate_limit.clone().map(|rl| (r.name.clone(), rl)))
        .collect();
    let rate_limit_store = stores::create_rate_limit_store(&config, repo.pool()).await?;
    let rate_limit_svc = Arc::new(RateLimitService::new(&rate_limit_configs, rate_limit_store));

    let registry_names: Vec<String> = config.registries.iter().map(|r| r.name.clone()).collect();
    let proxy_metrics = Arc::new(ProxyMetrics::new(&registry_names));

    // Shared between the hourly rollup writer and the admin read endpoint, so
    // both sides of the series are the same table.
    let stats_history: Arc<dyn batlehub_core::ports::StatsHistoryRepository> = Arc::new(
        batlehub_adapters::db::PgStatsHistoryRepository::new(repo.pool()),
    );
    let artifact_meta = Arc::new(PgArtifactMetaRepository::new(repo.pool()));
    let vuln_repo: Arc<dyn VulnerabilityRepository> =
        Arc::new(PgVulnerabilityRepository::new(repo.pool()));
    let admin_svc = Arc::new(
        AdminService::new(repo.clone() as Arc<dyn batlehub_core::ports::PackageRepository>)
            .with_vulnerability_repo(Arc::clone(&vuln_repo)),
    );
    let local_registry_backend = Arc::new(PostgresLocalRegistry::new(repo.pool()));
    stores::spawn_pending_publish_cleanup(Arc::clone(&local_registry_backend));
    let quota_svc = Arc::new(builders::build_quota_service(
        repo.pool(),
        &config.registries,
    ));
    stores::spawn_quota_gauge_sampler(Arc::clone(&quota_svc));
    let beta_channel_store: Arc<dyn BetaChannelPort> =
        Arc::new(PgBetaChannelStore::new(repo.pool()));
    // RFC 0015 §6.3 — the package and version grant tiers.
    let grant_repo: Option<Arc<dyn batlehub_core::ports::GrantRepository>> =
        Some(Arc::new(PgGrantRepository::new(repo.pool())));
    // RFC 0015 §10 rule 9 — ownership *is* a package-tier grant, so the store
    // that records it writes both. Wrapped here rather than at each caller
    // because ownership changes through five doors and four of them used to
    // write only `package_owners`; see `OwnershipGrants`.
    let ownership_store = {
        let inner = Arc::new(PgOwnershipStore::new(repo.pool()))
            as Arc<dyn batlehub_core::ports::OwnershipPort>;
        match grant_repo.clone() {
            Some(grants) => {
                batlehub_core::services::ownership_grants::OwnershipGrants::wrap(inner, grants)
            }
            None => inner,
        }
    };
    // RFC 0015 §4.2 — the GPG keys a Terraform namespace signs its providers
    // with. Absent, the download response serves the empty list it always did.
    let signing_key_store: Arc<dyn batlehub_core::ports::SigningKeyPort> =
        Arc::new(batlehub_adapters::db::PgSigningKeyStore::new(repo.pool()));
    let team_namespace_store: Arc<dyn batlehub_core::ports::TeamNamespacePort> =
        Arc::new(PgTeamNamespaceStore::new(repo.pool()));
    // RFC 0015 §6.3 — the package and version policy tiers, the twin of
    // `grant_repo` above for the five policies that compose deepest-wins.
    let policy_repo: Arc<dyn batlehub_core::ports::PolicyRepository> =
        Arc::new(batlehub_adapters::db::PgPolicyRepository::new(repo.pool()));

    // Built before the hot bundle because `license_gate` reads the recorded
    // licence through it; `build_sbom_service` below wraps the same repository.
    let sbom_repo: Arc<dyn batlehub_core::ports::SbomRepository> =
        Arc::new(batlehub_adapters::db::PgSbomRepository::new(repo.pool()));
    // RFC 0019 §5.2 — where forge refs are remembered and the rate-limit
    // budget the forge clients share. In the database because it is the one
    // store every deployment has, and because the budget only means something
    // if every process on the token reads the same row.
    let forge_stores = hot_config::ForgeStores {
        ref_resolutions: Some(Arc::new(
            batlehub_adapters::db::PgRefResolutionRepository::new(repo.pool()),
        )),
        rate_limit_budget: Some(Arc::new(batlehub_adapters::db::PgRateLimitBudget::new(
            repo.pool(),
        ))),
        artifact_meta: Some(
            Arc::clone(&artifact_meta) as Arc<dyn batlehub_core::ports::ArtifactCacheMeta>
        ),
    };
    // RFC 0018 §6.3 — verdicts, the leased scan queue and worker heartbeats,
    // all in PostgreSQL: the one store every deployment has, and the only
    // thing the proxy and worker roles share.
    // RFC 0002 (recast): pushed flags and the exposure report's scan state.
    let advisory_repo: Arc<dyn batlehub_core::ports::AdvisoryRepository> = Arc::new(
        batlehub_adapters::db::PgAdvisoryRepository::new(repo.pool()),
    );
    // RFC 0008 §6.3 — the miss log. Always wired: a connected instance
    // records nothing because nothing refuses, and an instance that later
    // turns the mode on has the table already.
    let air_gap_stores = hot_config::AirGapStores {
        miss_recorder: Some(Arc::new(batlehub_adapters::db::PgMissRecorder::new(
            repo.pool(),
        ))),
        bundle_history: Some(Arc::new(batlehub_adapters::db::PgBundleHistory::new(
            repo.pool(),
        ))),
    };
    let security_stores = hot_config::SecurityStores {
        advisories: Some(Arc::clone(&advisory_repo)),
        verdicts: Some(Arc::new(batlehub_adapters::db::PgVerdictRepository::new(
            repo.pool(),
        ))),
        queue: Some(Arc::new(batlehub_adapters::db::PgScanQueue::new(
            repo.pool(),
        ))),
        workers: Some(Arc::new(batlehub_adapters::db::PgWorkerRegistry::new(
            repo.pool(),
        ))),
        // RFC 0014: only when the audit is on. Absent, the presence scanner is
        // not built and the eviction hold holds nothing — the pre-0014 tree.
        upstream_status: config.upstream_audit.enabled.then(|| {
            Arc::new(batlehub_adapters::db::PgUpstreamStatusStore::new(
                repo.pool(),
            )) as Arc<dyn batlehub_core::ports::UpstreamStatusPort>
        }),
    };

    let (
        init_hot,
        init_access,
        registry_map,
        registry_mode_map,
        upstream_map,
        vuln_db_map,
        sumdb_map,
    ) = hot_config::build_hot_bundle(
        &config,
        &beta_channel_store,
        &(repo.clone() as Arc<dyn batlehub_core::ports::PackageRepository>),
        &vuln_repo,
        &sbom_repo,
        &grant_repo,
        &Some(Arc::clone(&policy_repo)),
        &Some(Arc::clone(&signing_key_store)),
        &forge_stores,
        &security_stores,
        &air_gap_stores,
    )?;
    let warming_clients: HashMap<String, Arc<dyn batlehub_core::ports::RegistryClient>> = init_hot
        .registries
        .iter()
        .map(|(k, v)| (k.clone(), Arc::clone(v)))
        .collect();
    let hot = new_hot_lock(init_hot);

    let sbom_svc = stores::build_sbom_service(repo.pool())?;
    // Per-registry README capture is configured in `HotConfig::readme` and
    // defaults to on, so the service is always wired: an absent
    // `[registries.readme]` block means enabled, not disabled (RFC 0007 §4.1).
    // Prose search is opt-in, and the index is a generated column built with one
    // text search configuration. Settling it here — before the repository is
    // built — is what keeps the column and the query agreeing: searching with
    // `french` against a column built with `english` matches almost nothing and
    // reports it as a `200` with an empty list (RFC 0007-bis §5.2). The
    // repository queries with the configuration the column *actually* has, so a
    // reload that turns `[search] readmes` on cannot reintroduce that mismatch,
    // and the settled name is handed to the hot builder so a reload naming a
    // configuration this server does not have is refused rather than deferred to
    // the next restart — see `SettledTextConfig`.
    let settled_text_config = hot_config::settle_text_config(&repo.pool(), &config.search).await?;
    let mut readme_svc = batlehub_core::services::ReadmeService::new(Arc::new(
        batlehub_adapters::db::PgReadmeRepository::new(repo.pool())
            .with_text_config(settled_text_config.in_force()),
    ))
    // The render cache, content-addressed by digest and renderer version
    // (RFC 0007 §5.3). Also where a proxied image's bytes live, keyed by the
    // digest of its URL — one entry for the shields.io badge a thousand READMEs
    // share (RFC 0007-bis §5.1).
    .with_cache(cache.clone());
    // One image client for every registry, because an image host is a third
    // party by construction: it is not the configured upstream, so none of a
    // registry's credentials apply to it. It does honour the **global** proxy
    // settings, which are about this network rather than about any one registry.
    match batlehub_adapters::registry::HttpReadmeImageFetcher::new(
        &batlehub_adapters::registry::http_client::UpstreamHttpOptions {
            proxy_url: config.proxy.as_ref().map(|p| p.url.clone()),
            proxy_username: config.proxy.as_ref().and_then(|p| p.username.clone()),
            proxy_password: config.proxy.as_ref().and_then(|p| p.password.clone()),
            no_proxy: config.proxy.as_ref().and_then(|p| p.no_proxy.clone()),
            ..Default::default()
        },
    ) {
        Ok(fetcher) => readme_svc = readme_svc.with_image_fetcher(Arc::new(fetcher)),
        // Not fatal, and visible: `remote_images = "proxy"` charts images
        // instead, which is what `strip` does and is the answer the panel
        // already knows how to render.
        Err(e) => tracing::warn!(
            error = %e,
            "readme: could not build the image fetcher; images will be charted"
        ),
    }
    let readme_svc = Arc::new(readme_svc);
    let proxy_svc = Arc::new(ProxyService {
        hot: Arc::clone(&hot),
        storage: storage.clone(),
        cache: cache.clone(),
        repo: repo.clone() as Arc<dyn batlehub_core::ports::PackageRepository>,
        artifact_meta: Arc::clone(&artifact_meta)
            as Arc<dyn batlehub_core::ports::ArtifactCacheMeta>,
        metrics: Arc::clone(&proxy_metrics),
        sbom: Some(Arc::clone(&sbom_svc)),
        readme: Some(Arc::clone(&readme_svc)),
        discovery: Default::default(),
    });

    // RFC 0008 §4.5, the third warning: an air-gapped registry with nothing
    // cached answers `503` to everything, and that is worth one line at boot
    // rather than a support ticket about a mirror that "does not work". Said
    // once, here; the Air gap page recomputes it, so it stays true after the
    // first import rather than freezing what was true at startup.
    if config.air_gap.as_ref().is_some_and(|a| a.enabled) {
        let empty = batlehub_web::handlers::air_gap::empty_registries(&proxy_svc).await;
        if !empty.is_empty() {
            tracing::warn!(
                count = empty.len(),
                registries = %empty.join(", "),
                "air gap: these registries hold no cached artifact, so they will answer 503 to                  every request until a bundle is imported"
            );
        }
    }

    let ip_block_store = stores::create_ip_block_store(&config, repo.pool()).await?;
    let user_block_repo = stores::create_user_block_repository(repo.pool());
    let ip_blocking_cfg = config.ip_blocking.clone();
    // `[server].trusted_proxies`, falling back to the deprecated
    // `[ip_blocking].trusted_proxies`. Hot-reloadable, and handed to the reload
    // service below: it decides which peers may influence routing, so it has to
    // move in step with the host-routing table (which is hot-reloadable) or a
    // reload that turns host routing on would run under the startup policy. Each
    // request resolves its verdict once, so no in-flight request straddles two
    // policies.
    let proxy_trust = batlehub_web::ProxyTrust::from_config(config.effective_trusted_proxies());
    let registry_host_map = batlehub_web::RegistryHostMap::from_app_config(&config);
    let local_svc = Arc::new(LocalRegistryService {
        backend: local_registry_backend,
        storage: storage.clone(),
        hot: Arc::clone(&hot),
        quota: Some(Arc::clone(&quota_svc)),
        ownership: Some(ownership_store),
        team_namespace: Some(Arc::clone(&team_namespace_store)),
        sbom: Some(Arc::clone(&sbom_svc)),
        readme: Some(Arc::clone(&readme_svc)),
        explore_cache: Some(Arc::clone(&admin_svc.explore_cache)),
        package_repo: Some(repo.clone() as Arc<dyn batlehub_core::ports::PackageRepository>),
    });

    let warm_coordinator = stores::create_warm_coordinator(&config).await?;
    let warming_map = setup::build_warming_map(
        &config,
        &warming_clients,
        storage.clone(),
        repo.pool(),
        warm_coordinator,
        Arc::clone(&proxy_metrics),
    );
    let eviction_map = setup::build_eviction_map(
        &config,
        storage.clone(),
        repo.pool(),
        repo.clone() as Arc<dyn batlehub_core::ports::PackageRepository>,
        security_stores
            .upstream_status
            .clone()
            .filter(|_| config.upstream_audit.retain_disappeared),
    );
    let access_config = new_access_lock(init_access);
    // Prose search, shared between the app and the reload path so an operator
    // can turn it off without a restart (RFC 0007-bis §4.1). The **index** is
    // not torn down when it goes off — nothing reads it, and rebuilding it on
    // the next flip would be a surprise measured in minutes.
    let search_config = batlehub_web::new_search_lock(config.search.readmes);

    let hot_reload_enabled = std::env::var("BATLEHUB_DISABLE_HOT_RELOAD")
        .map(|v| v != "1" && v.to_lowercase() != "true")
        .unwrap_or(true);
    let banner_store = stores::create_banner_store(&config, repo.pool()).await?;
    let banner_svc = Arc::new(BannerService::new(banner_store));
    let notification_store = stores::create_notification_store(repo.pool());
    let notification_svc =
        stores::build_notification_service(Arc::clone(&notification_store), &config.notifications);

    let hot_builder = hot_config::make_hot_builder(
        Arc::clone(&beta_channel_store),
        repo.clone() as Arc<dyn batlehub_core::ports::PackageRepository>,
        Arc::clone(&vuln_repo),
        Arc::clone(&sbom_repo),
        grant_repo.clone(),
        Some(Arc::clone(&policy_repo)),
        Some(Arc::clone(&signing_key_store)),
        forge_stores.clone(),
        security_stores.clone(),
        air_gap_stores.clone(),
        settled_text_config,
    );
    // Built once here so the same instance is shared with the reload service (for
    // hot-swapping) and registered as actix app_data below.
    let repo_signer_map = builders::build_repo_signer_map(&config)?;
    let config_change_repo: Arc<dyn batlehub_core::ports::ConfigChangeRepository> =
        Arc::new(PgConfigChangeRepository::new(repo.pool()));
    let storage_admin_repo: Arc<dyn batlehub_core::ports::StorageAdminRepository> =
        Arc::new(PgStorageAdminRepository::new(repo.pool()));
    let reload_svc = Arc::new(ConfigReloadService::new(ConfigReloadParams {
        hot: Arc::clone(&hot),
        access: Arc::clone(&access_config),
        search: Arc::clone(&search_config),
        registry_map: registry_map.clone(),
        registry_mode_map: registry_mode_map.clone(),
        upstream_map: upstream_map.clone(),
        cargo_index_map: cargo_index_map.clone(),
        repo_signer_map: repo_signer_map.clone(),
        vuln_db_map: vuln_db_map.clone(),
        sumdb_map: sumdb_map.clone(),
        registry_host_map: registry_host_map.clone(),
        // The same handle wrapped into the host-routing middleware and registered
        // as `app_data` below — clones share a lock, which is what lets a reload
        // reach the policy those two actually read.
        proxy_trust: proxy_trust.clone(),
        config_path: config_path.clone(),
        config_change_repo: Some(Arc::clone(&config_change_repo)),
        hot_reload_enabled,
        builder: hot_builder,
        banner: Some(Arc::clone(&banner_svc)),
    }));

    // Seed the warning store from the config we booted with (this also logs each
    // one). Reloads refresh it themselves.
    reload_svc.set_warnings(config.warnings());

    if hot_reload_enabled {
        watcher::spawn_config_watcher(config_path.clone(), Arc::clone(&reload_svc));
        tracing::info!("hot reload: enabled (watching {})", config_path);
    } else {
        tracing::info!("hot reload: disabled (BATLEHUB_DISABLE_HOT_RELOAD=1)");
    }

    tracing::info!(
        addr = %format!("{}:{}", config.server.host, config.server.port),
        "listening"
    );
    watcher::spawn_startup_warming(&config, &warming_map);

    // Hourly cache-statistics rollup, so the dashboard's trend survives a
    // deploy (RFC 0004 §2.3). `history_enabled = false` restores the previous
    // behaviour: counters since this process started, and nothing older.
    if config.stats.history_enabled {
        let rollup = Arc::new(batlehub_core::services::StatsRollupService::new(
            Arc::clone(&proxy_metrics),
            Arc::clone(&stats_history),
            config.stats.history_retention_days,
        ));
        watcher::spawn_stats_rollup(rollup, Arc::clone(&proxy_svc), registry_names.clone());
        tracing::info!(
            retention_days = config.stats.history_retention_days,
            "stats-rollup: hourly cache-statistics history enabled"
        );
    } else {
        tracing::info!("[stats] history_enabled = false — no rollup recorded");
    }

    // Periodic SBOM re-check against the OSV vulnerability database.
    if let Some(vuln_cfg) = config.vulnerability_scan.as_ref().filter(|v| v.enabled) {
        let osv_client = reqwest::Client::builder()
            .user_agent("batlehub/0.1")
            .connect_timeout(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .context("building OSV HTTP client")?;
        let scanner = Arc::new(OsvScanner::new(osv_client, vuln_cfg.osv_api_url.clone()));
        let scan_svc = Arc::new(
            VulnerabilityScanService::new(
                Arc::clone(&sbom_svc.repo),
                scanner,
                Arc::clone(&vuln_repo),
                vuln_cfg.batch_size as u64,
            )
            .with_advisories(Arc::clone(&advisory_repo)),
        );
        watcher::spawn_periodic_vuln_scan(vuln_cfg.interval_secs, scan_svc);
        tracing::info!(
            interval_secs = vuln_cfg.interval_secs,
            "vuln-scan: periodic SBOM re-check enabled"
        );
    }

    // RFC 0018 §5.4 — the worker role: lease scan jobs, run the scanners,
    // record verdicts. Embedded by default; `--roles worker` runs it alone.
    let quarantined: Vec<String> = config
        .registries
        .iter()
        .filter(|r| r.security.is_some())
        .map(|r| r.name.clone())
        .collect();
    if is_worker {
        let setup::BuiltScanners {
            scanners,
            enrichers,
        } = setup::build_scanners(&config).context("building scanners")?;
        let worker = Arc::new(batlehub_core::services::ScanWorker {
            config: batlehub_core::services::WorkerConfig {
                worker_id: format!("{}-{}", hostname_or("worker"), std::process::id()),
                max_concurrent: config.worker.max_concurrent,
                registries: config.worker.registries.clone(),
                job_timeout: std::time::Duration::from_secs(config.worker.job_timeout_secs),
                max_attempts: config.worker.max_attempts,
                idle_poll: std::time::Duration::from_secs(2),
            },
            queue: Arc::clone(security_stores.queue.as_ref().expect("built above")),
            verdicts: security_stores.service().expect("built above"),
            workers: security_stores.workers.clone(),
            sboms: Some(Arc::clone(&sbom_svc.repo)),
            hot: Arc::clone(&hot),
            scanners,
            enrichers,
            storage: Some(Arc::clone(&storage)),
            max_artifact_bytes: config
                .limits
                .max_artifact_size_bytes
                .unwrap_or(500 * 1024 * 1024),
            // RFC 0018 phase 4: the flip alert and the release announcement
            // go through the same channels a publish does, with the pullers
            // read from the same access log the audit page reads.
            notifier: notification_svc.as_ref().map(|n| {
                Arc::new(batlehub_web::services::NotificationSinkAdapter(Arc::clone(
                    n,
                ))) as Arc<dyn batlehub_core::ports::NotificationSink>
            }),
            events: Some(Arc::clone(&repo) as Arc<dyn batlehub_core::ports::PackageRepository>),
        });
        tokio::spawn(Arc::clone(&worker).run());
        // RFC 0018 phase 4: the rescan timer, one per estate — every worker
        // process ticks, the one holding the advisory lock queues.
        if config.registries.iter().any(|r| {
            r.security
                .as_ref()
                .and_then(|s| s.rescan.as_ref())
                .is_some_and(|x| x.interval_secs > 0)
        }) {
            let scheduler = Arc::new(batlehub_core::services::RescanScheduler {
                verdicts: Arc::clone(security_stores.verdicts.as_ref().expect("built above")),
                queue: Arc::clone(security_stores.queue.as_ref().expect("built above")),
                hot: Arc::clone(&hot),
                cache: Some(Arc::clone(&cache)),
                batch: 500,
            });
            watcher::spawn_rescan_scheduler(scheduler);
            tracing::info!("security: rescan scheduler started");
        }
        tracing::info!(
            max_concurrent = config.worker.max_concurrent,
            registries = ?config.worker.registries,
            "security worker: started"
        );
    } else if !quarantined.is_empty() {
        // A proxy-only process with a quarantine and nobody scanning: the
        // §4.3 warning, from the heartbeat table rather than a guess.
        let live = match &security_stores.workers {
            Some(w) => w.live_count(120).await.unwrap_or(0),
            None => 0,
        };
        metrics::gauge!("batlehub_workers_live").set(live as f64);
        if live == 0 {
            tracing::warn!(
                registries = ?quarantined,
                "security: no worker has sent a heartbeat in the last two minutes; versions of \
                 these registries below mature_age_secs will stay refused with SCAN_PENDING \
                 until one runs (start a process with --roles worker)"
            );
        }
    }
    if !is_proxy {
        tracing::info!("proxy role absent: serving only /livez and /metrics");
        return server_factory::run_worker_only_server(
            format!("{}:{}", config.server.host, config.server.port),
            prometheus_handle,
        )
        .await;
    }

    // RFC 0014: the upstream audit, on the worker role (§13). A proxy-only
    // process with it enabled is told, once, that it is not the one sweeping.
    // The handle is kept for the admin surface (§4.6): `recheck` drives the
    // same probe on demand.
    let mut upstream_audit: Option<Arc<batlehub_core::services::UpstreamAuditService>> = None;
    if config.upstream_audit.enabled {
        if let (true, Some(status)) = (is_worker, security_stores.upstream_status.clone()) {
            let audit = &config.upstream_audit;
            let svc = batlehub_core::services::UpstreamAuditService::new(
                Arc::new(batlehub_adapters::db::PgArtifactMetaRepository::new(
                    repo.pool(),
                )) as Arc<dyn batlehub_core::ports::ArtifactInventory>,
                status,
                Arc::clone(&hot),
                Some(Arc::clone(&cache)),
                security_stores.queue.clone(),
                batlehub_core::services::UpstreamAuditPolicy {
                    confirm_after: audit.confirm_after,
                    confirm_min_age: std::time::Duration::from_secs(audit.confirm_min_age_secs),
                    outage_ratio: audit.outage_ratio,
                    retain_disappeared: audit.retain_disappeared,
                    skip_recently_seen: audit.skip_recently_seen,
                    metadata_pin_ttl: std::time::Duration::from_secs(2 * audit.interval_secs),
                    on_confirmed: audit.on_confirmed.parse().unwrap_or_default(),
                },
                config.worker.max_concurrent as usize,
                config.upstream_audit_registries(),
            );
            // RFC 0014 §4.5: a transition is reported through the same
            // channels a publish is. No notification service (disabled in
            // config) means the sweep records and holds, and tells nobody.
            let svc = match &notification_svc {
                Some(n) => svc.with_notifier(Arc::new(
                    batlehub_web::services::NotificationSinkAdapter(Arc::clone(n)),
                )),
                None => svc,
            };
            // RFC 0014 §4.3, §6.5: under `"block"` the sweep writes through
            // the same service an admin's block goes through, so the block is
            // in shape and in the audit trail exactly theirs. `with_admin` is
            // a no-op under `"audit"`.
            let svc = svc.with_admin(Arc::clone(&admin_svc));
            if svc.blocks() {
                // §4.4: the setting that can break a build is said once, at
                // startup, where an operator reading the log will see it.
                tracing::info!(
                    blocked_by = batlehub_core::services::upstream_audit::SYSTEM_ACTOR,
                    "upstream audit: on_confirmed = \"block\" — a confirmed disappearance is \
                     refused on the wire through the admin block list; a reappearance lifts only \
                     this audit's own blocks"
                );
            }
            let svc = Arc::new(svc);
            upstream_audit = Some(Arc::clone(&svc));
            watcher::spawn_upstream_audit(audit.interval_secs, svc);
            tracing::info!(
                interval_secs = audit.interval_secs,
                confirm_after = audit.confirm_after,
                confirm_min_age_secs = audit.confirm_min_age_secs,
                on_confirmed = %audit.on_confirmed,
                registries = ?config.upstream_audit_registries(),
                "upstream audit: enabled"
            );
        } else if !is_worker {
            tracing::warn!(
                "upstream audit: enabled, but this process has no worker role; the sweep runs \
                 on a worker, and unless another process has that role nothing is audited"
            );
        }
    }

    // Periodic collection of storage blobs nothing references. Off unless asked
    // for: it deletes on a timer with nobody watching, and the on-demand
    // endpoint covers the deployment that would rather look first.
    if let Some(coh_cfg) = config.cache_coherence.as_ref().filter(|c| c.enabled) {
        watcher::spawn_periodic_coherence_sweep(coh_cfg.interval_secs, eviction_map.clone());
        tracing::info!(
            interval_secs = coh_cfg.interval_secs,
            registries = eviction_map.len(),
            "coherence: periodic orphan sweep enabled"
        );
    }

    server_factory::run_actix_server(server_factory::ServerParams {
        bind_addr: format!("{}:{}", config.server.host, config.server.port),
        static_dir: config.server.static_dir.clone(),
        cli_binary_path: config
            .server
            .cli_binary_path
            .as_deref()
            .map(std::path::PathBuf::from),
        cors_allowed_origins: config
            .server
            .cors_allowed_origins
            .clone()
            .unwrap_or_default(),
        db_pool: repo.pool(),
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        search_config: Arc::clone(&search_config),
        registry_map,
        upstream_map,
        vuln_db_map,
        sumdb_map,
        oidc_sso_flows,
        oidc_provider_names,
        login_states,
        warming_map,
        eviction_map,
        proxy_metrics,
        prometheus_handle,
        stats_history,
        sbom_svc,
        notification_svc,
        notification_store,
        notifications_config: config.notifications.clone(),
        upstream_audit,
        artifact_inventory: Arc::clone(&artifact_meta)
            as Arc<dyn batlehub_core::ports::ArtifactInventory>,
        local_svc,
        quota_svc,
        registry_mode_map,
        repo_signer_map,
        ip_block_store,
        user_block_repo,
        beta_channel_store,
        team_namespace_store,
        policy_repo,
        bundle_history: air_gap_stores.bundle_history.clone().expect("built above"),
        advisory_repo: Arc::clone(&advisory_repo),
        flag_svc: Arc::new(batlehub_core::services::FlagService::new(
            Arc::clone(&advisory_repo),
            Arc::clone(&hot),
        )),
        flag_sources: batlehub_web::FlagSources(config.flag_sources.clone()),
        exposure_config: batlehub_web::ExposureConfig {
            sbom_registries: config
                .registries
                .iter()
                .filter(|r| r.sbom.is_some())
                .count() as u64,
        },
        ip_blocking_cfg,
        proxy_trust,
        registry_host_map,
        cargo_index_map,
        rate_limit_svc,
        auth_providers,
        reload_svc,
        banner_svc,
        storage_admin_repo,
    })
    .await
}

/// The host name, for a worker id that says where it ran.
fn hostname_or(fallback: &str) -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_owned())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}
