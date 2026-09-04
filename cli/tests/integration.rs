/// Integration tests for `batlehub-cli`.
///
/// Each test starts a genuine in-memory batlehub server on a random port, then
/// invokes the compiled `batlehub-cli` binary via `std::process::Command` and
/// asserts on exit status and JSON output.
///
/// ## Running
///
/// ```
/// cargo test -p batlehub-cli --test integration
/// ```
use std::collections::HashMap;
use std::io::Write as _;
use std::net::TcpStream;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use actix_web::{web, App, HttpServer};
use utoipa_actix_web::AppExt;

use batlehub_adapters::{
    auth::StaticTokenAuthProvider,
    cache::{InMemoryBannerStore, InMemoryCacheStore},
    db::InMemoryUserBlockRepository,
    in_memory::{
        InMemoryPackageRepository, InMemoryQuotaRepository, InMemoryStorageBackend,
        InMemoryTeamNamespaceStore, NoopArtifactMetaRepository, NullUserTokenRepository,
    },
    local_registry::InMemoryLocalRegistry,
    notification::InMemoryNotificationStore,
    rate_limit::InMemoryIpBlockStore,
};
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{AccessEvent, PackageId, Role},
    ports::{
        AuthProvider, BannerPort, CacheStore, IpBlockStore, PackageRepository, RegistryClient,
        TeamNamespacePort, UserBlockRepository,
    },
    rules::{BlockListRule, RbacRule},
    services::{
        new_hot_lock, AdminService, HotConfig, LocalRegistryService, ProxyMetrics, ProxyService,
        QuotaService, RegistryPolicy,
    },
};
use batlehub_web::{
    configure_app, new_access_lock,
    services::{BannerService, ConfigReloadParams, ConfigReloadService, HotConfigBuilder},
    AccessConfig, AuthMiddlewareFactory, RegistryMap, RegistryModeMap, UpstreamMap,
};

const AUTH_TOKEN: &str = "test-cli-token";
const REGISTRY: &str = "test-nuget";

// ── In-memory batlehub server ─────────────────────────────────────────────────

struct TestServer {
    port: u16,
    /// Exposed so tests can seed packages into the package repository directly.
    /// This is necessary because in-memory local-registry and admin-service
    /// backends are separate stores (in Postgres they share the same tables).
    repo: Arc<InMemoryPackageRepository>,
    /// Exposed for the same reason `repo` is: `package readme` reads the README
    /// store, and nothing a CLI test can do through the HTTP surface puts a row
    /// in it — the capture paths are a proxied resolve and a publish, neither of
    /// which this harness runs.
    readme_repo: Arc<batlehub_adapters::in_memory::InMemoryReadmeRepository>,
    _runtime: tokio::runtime::Runtime,
}

impl TestServer {
    fn start() -> Self {
        Self::start_with(&[(REGISTRY, "nuget")])
    }

    /// Like `start()`, but with an arbitrary set of `(name, registry_type)`
    /// local-mode registries instead of the single hardcoded nuget one.
    fn start_with(registries: &[(&str, &str)]) -> Self {
        Self::start_with_hosts(registries, &[])
    }

    /// Like `start_with()`, plus `(registry, public_url)` host bindings — what
    /// `[subdomain_routing]` produces, and what `GET /api/v1/registries`
    /// advertises as `public_url`.
    fn start_with_hosts(registries: &[(&str, &str)], public_urls: &[(&str, &str)]) -> Self {
        let host_map = batlehub_web::RegistryHostMap::new(
            public_urls
                .iter()
                .map(|(reg, url)| {
                    (
                        url.trim_start_matches("https://")
                            .trim_start_matches("http://")
                            .to_string(),
                        reg.to_string(),
                    )
                })
                .collect(),
            public_urls
                .iter()
                .map(|(reg, url)| (reg.to_string(), url.to_string()))
                .collect(),
            HashMap::new(),
        );
        let registry_map = RegistryMap::from(
            registries
                .iter()
                .map(|(n, t)| (n.to_string(), t.to_string()))
                .collect::<HashMap<String, String>>(),
        );
        let registry_names: Vec<String> = registry_map.keys();

        let repo = InMemoryPackageRepository::new();
        let storage = InMemoryStorageBackend::new();
        let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

        let policies: HashMap<String, Arc<RegistryPolicy>> = registry_names
            .iter()
            .map(|name| {
                let perms = HashMap::from([
                    (Role::Anonymous, vec!["*".to_owned()]),
                    (Role::User, vec!["*".to_owned()]),
                    (Role::Admin, vec!["*".to_owned()]),
                ]);
                (
                    name.clone(),
                    Arc::new(RegistryPolicy {
                        metadata_ttl: None,
                        firewall_only: false,
                        serve_stale_metadata: false,
                        artifact_ttl: None,
                        rules: vec![
                            Box::new(
                                RbacRule::from_patterns(perms)
                                    .expect("fixture rbac patterns are valid"),
                            ),
                            Box::new(BlockListRule::new(repo.clone())),
                        ],
                    }),
                )
            })
            .collect();

        let readme_repo = batlehub_adapters::in_memory::InMemoryReadmeRepository::new();
        let readme_svc = Arc::new(batlehub_core::services::ReadmeService::new(Arc::clone(
            &readme_repo,
        )
            as Arc<dyn batlehub_core::ports::ReadmeRepository>));

        // RFC 0015 §4.1's grant hierarchy, granting everything to everyone.
        //
        // Present rather than absent because `authz explain` answers `404
        // registry not configured` for a registry with no entry, which a test
        // asserting a verdict would read as a broken command rather than a
        // missing fixture.
        //
        // **Permissive rather than `RegistryGrants::empty()`**, and the
        // difference is the whole of §4.3: an *empty* hierarchy grants nobody
        // anything, because grants only widen and a union of nothing is nothing.
        // Wiring one closed this server to its own tests — publish, version and
        // access-check all started refusing. This suite exercises CLI plumbing
        // rather than authorization (`crates/web/tests/authz_matrix.rs` is where
        // that is measured), so what it needs from the model is that it never
        // gets in the way.
        let grants: HashMap<String, Arc<batlehub_core::entities::RegistryGrants>> = {
            use batlehub_core::entities::{
                Action, GrantMap, Node, RegistryGrants, RegistryKind, SubjectMatcher, Tier,
            };
            [(
                REGISTRY.to_owned(),
                Arc::new(RegistryGrants {
                    kind: RegistryKind::Nuget,
                    registry: Node::new(
                        Tier::Registry,
                        format!("registry:{REGISTRY}"),
                        Some(GrantMap::new().grant(SubjectMatcher::Anyone, Action::ALL.to_vec())),
                    ),
                    namespaces: Vec::new(),
                }),
            )]
            .into()
        };
        let local_svc = Arc::new(LocalRegistryService {
            backend: Arc::new(InMemoryLocalRegistry::new()),
            storage: storage.clone(),
            hot: new_hot_lock(HotConfig {
                registries: HashMap::new(),
                policies: HashMap::new(),
                // RFC 0017's editor reads both of these off **this** lock:
                // `GrantAdminService` is built from `local_svc.hot`, the way
                // `server_factory` builds it. Production has one `HotConfig`
                // behind both services and this fixture has two, so the
                // hierarchy is shared explicitly rather than by construction.
                //
                // Without `grant_repo` the routes answer `503 no grant
                // storage`; without `grants` the service answers `404 registry
                // not configured`, because a grant on a registry this server
                // does not serve is a typo whose only symptom would be a row
                // nothing reads. Either way these tests would assert an error
                // path and never reach the command.
                grants: grants.clone(),
                grant_repo: Some(batlehub_adapters::in_memory::InMemoryGrantRepository::new()
                    as Arc<dyn batlehub_core::ports::GrantRepository>),
                ..Default::default()
            }),
            quota: None,
            ownership: None,
            team_namespace: None,
            sbom: None,
            explore_cache: None,
            package_repo: None,
            readme: Some(Arc::clone(&readme_svc)),
        });

        let registries: HashMap<String, Arc<dyn RegistryClient>> = HashMap::new();
        let proxy_svc = Arc::new(ProxyService {
            hot: new_hot_lock(HotConfig {
                registries,
                policies,
                grants,
                // RFC 0015 §4.2's instance tier. Wired for the same reason the
                // registry hierarchy above is permissive rather than empty: the
                // control-surface verbs live here now, and a fixture with no
                // instance node grants nobody `config:read` — so every admin
                // command in this suite would `403` on a server that is not the
                // one production runs. `instance_node` is rule 5's own
                // translation, so what the fixture grants is what a real
                // deployment grants.
                instance: Some(Arc::new(
                    batlehub_core::services::authz::translate::instance_node(None),
                )),
                // RFC 0017's editor writes here. Absent, the three `admin
                // grants` routes answer `503 no grant storage` — a *deployment*
                // fact rather than a bad request, so a CLI test without it would
                // assert the command's error path and never reach the command.
                grant_repo: Some(batlehub_adapters::in_memory::InMemoryGrantRepository::new()
                    as Arc<dyn batlehub_core::ports::GrantRepository>),
                ..Default::default()
            }),
            storage: storage.clone(),
            cache,
            repo: repo.clone(),
            artifact_meta: NoopArtifactMetaRepository::arc(),
            metrics: Arc::new(ProxyMetrics::new(&[])),
            sbom: None,
            readme: Some(Arc::clone(&readme_svc)),
            discovery: Default::default(),
        });

        let admin_svc = Arc::new(AdminService::new(repo.clone()));
        let token_repo = NullUserTokenRepository::arc();

        // Explore follows proxy access, as `build_access_config` makes it in an
        // unconfigured server: `rbac.explore.*` all default to `true`. Leaving
        // these three empty modelled a server where nothing is browsable by
        // anyone, which is not what the CLI's catalogue commands run against —
        // `package readme` reads the set directly and would get the `404` an
        // operator gets for a registry they took out of the console.
        let access_config = new_access_lock(AccessConfig {
            anonymous: registry_names.iter().cloned().collect(),
            user: registry_names.iter().cloned().collect(),
            admin: registry_names.iter().cloned().collect(),
            groups: HashMap::new(),
            explore_anonymous: registry_names.iter().cloned().collect(),
            explore_user: registry_names.iter().cloned().collect(),
            explore_admin: registry_names.iter().cloned().collect(),
        });

        let auth_providers: Vec<Arc<dyn AuthProvider>> =
            vec![Arc::new(StaticTokenAuthProvider::new([(
                AUTH_TOKEN.to_owned(),
                Some("test-user".to_owned()),
                Role::Admin,
            )]))];

        let mode_map = RegistryModeMap::from(
            registry_names
                .iter()
                .map(|n| (n.clone(), RegistryMode::Local))
                .collect::<HashMap<_, _>>(),
        );

        let quota_svc = Arc::new(QuotaService::new(
            InMemoryQuotaRepository::new(),
            HashMap::new(),
        ));
        let ip_block_store: Arc<dyn IpBlockStore> = Arc::new(InMemoryIpBlockStore::new());
        let banner_store: Arc<dyn BannerPort> = Arc::new(InMemoryBannerStore::new());
        let banner_svc = Arc::new(BannerService::new(banner_store));
        let user_block_repo: Arc<dyn UserBlockRepository> =
            Arc::new(InMemoryUserBlockRepository::new());
        let team_namespace_store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
        let reload_builder: HotConfigBuilder = Arc::new(|_| anyhow::bail!("not used in tests"));
        let reload_svc = Arc::new(ConfigReloadService::new(ConfigReloadParams {
            hot: proxy_svc.hot.clone(),
            access: access_config.clone(),
            search: batlehub_web::new_search_lock(false),
            registry_map: registry_map.clone(),
            registry_mode_map: mode_map.clone(),
            upstream_map: UpstreamMap::default(),
            cargo_index_map: batlehub_web::CargoIndexMap::default(),
            repo_signer_map: batlehub_web::RepoSignerMap::default(),
            vuln_db_map: batlehub_web::VulnDbMap::default(),
            sumdb_map: batlehub_web::SumDbMap::default(),
            registry_host_map: batlehub_web::RegistryHostMap::default(),
            proxy_trust: batlehub_web::ProxyTrust::default(),
            config_path: "config.toml".to_owned(),
            config_change_repo: None,
            hot_reload_enabled: false,
            builder: reload_builder,
            banner: Some(Arc::clone(&banner_svc)),
        }));

        let configure = configure_app(
            proxy_svc,
            admin_svc,
            token_repo,
            None,
            access_config,
            registry_map,
            UpstreamMap::default(),
            vec![],
            batlehub_web::OidcProviderNames::default(),
            batlehub_adapters::in_memory::InMemoryLoginStateStore::arc(),
            HashMap::new(), // warming_map
            // One eviction service, so `admin cache evict` and `admin cache
            // coherence` have something to reach. `keep_latest_n` is set
            // because `/evict` answers `404` for a registry with no strategy
            // configured — the coherence sweep deliberately does not, which is
            // why it needs no strategy of its own here.
            [(
                REGISTRY.to_owned(),
                Arc::new(
                    batlehub_core::services::EvictionService::new(
                        batlehub_adapters::in_memory::NoopArtifactMetaRepository::arc(),
                        storage.clone(),
                        batlehub_core::services::EvictionConfig {
                            registry: REGISTRY.to_owned(),
                            keep_latest_n: Some(1),
                            ..Default::default()
                        },
                    )
                    .with_audit(repo.clone()),
                ),
            )]
            .into_iter()
            .collect(), // eviction_map
            Arc::new(ProxyMetrics::new(&[])),
            None,
            None,
            None,
            Arc::new(InMemoryNotificationStore::new()),
            None,
            None, // storage_admin_repo,
            // Prose search off, matching the shipped default.
            batlehub_web::new_search_lock(false),
        );

        let rt = tokio::runtime::Runtime::new().unwrap();
        let port = rt.block_on(async {
            let local_svc = local_svc.clone();
            let mode_map = mode_map.clone();
            let configure = configure.clone();
            let auth_providers = auth_providers.clone();
            let cargo_indexes = batlehub_web::CargoIndexMap::default();
            let quota_svc = quota_svc.clone();
            let ip_block_store = ip_block_store.clone();
            let banner_svc = banner_svc.clone();
            let reload_svc = reload_svc.clone();
            let user_block_repo = user_block_repo.clone();
            let team_namespace_store = team_namespace_store.clone();
            let host_map = host_map.clone();

            let server = HttpServer::new(move || {
                let (app, _) = App::new()
                    .into_utoipa_app()
                    .configure(configure.clone())
                    .split_for_parts();
                app.app_data(web::Data::new(cargo_indexes.clone()))
                    .app_data(web::Data::new(local_svc.clone()))
                    .app_data(web::Data::new(Arc::new(
                        batlehub_core::services::GrantAdminService::new(
                            local_svc.hot.clone(),
                            Some(
                                Arc::new(batlehub_core::services::BackendVersions(Arc::clone(
                                    &local_svc.backend,
                                )))
                                    as Arc<dyn batlehub_core::services::VersionLookup>,
                            ),
                            local_svc.ownership.clone(),
                        ),
                    )))
                    .app_data(web::Data::new(mode_map.clone()))
                    .app_data(web::Data::new(quota_svc.clone()))
                    .app_data(web::Data::new(ip_block_store.clone()))
                    .app_data(web::Data::new(banner_svc.clone()))
                    .app_data(web::Data::new(reload_svc.clone()))
                    .app_data(web::Data::new(user_block_repo.clone()))
                    .app_data(web::Data::new(team_namespace_store.clone()))
                    .app_data(web::Data::new(host_map.clone()))
                    .wrap(AuthMiddlewareFactory::new(auth_providers.clone()))
            })
            .bind("127.0.0.1:0")
            .expect("bind to random port");

            let port = server.addrs()[0].port();
            tokio::spawn(server.run());
            port
        });

        assert!(
            wait_for_port(port, Duration::from_secs(10)),
            "test server did not start on port {port}"
        );

        Self {
            port,
            repo,
            readme_repo,
            _runtime: rt,
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Seed a package entry directly into the `PackageRepository` that the admin
    /// service queries. Required because the in-memory local-registry backend and
    /// the package repository are separate stores (unlike PostgreSQL, where they
    /// share the same tables).
    fn seed_package(&self, name: &str, version: &str) {
        let pkg = PackageId {
            registry: REGISTRY.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
            artifact: None,
        };
        let event = AccessEvent::allowed_download(pkg, Some("test-user".to_owned()), Role::Admin);
        let repo = self.repo.clone();
        // Run in a fresh single-threaded runtime so we don't nest runtimes.
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async move { repo.record_access(event).await.unwrap() });
    }

    /// Seed a README directly into the store, for the same reason
    /// [`Self::seed_package`] exists: the capture paths are a proxied resolve
    /// and a publish, and this harness runs neither.
    fn seed_readme(&self, name: &str, version: &str, body: &str) {
        use batlehub_core::entities::{readme_digest, PackageReadme, ReadmeFormat, ReadmeSource};

        let readme = PackageReadme {
            registry: REGISTRY.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
            content: body.to_owned(),
            format: ReadmeFormat::Markdown,
            source: ReadmeSource::LocalPublish,
            digest: readme_digest(body),
            truncated: false,
            package_level: false,
            extracted_at: chrono::Utc::now(),
        };
        let repo = Arc::clone(&self.readme_repo);
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async move {
                use batlehub_core::ports::ReadmeRepository;
                repo.upsert(readme).await.unwrap()
            });
    }
}

fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(300));
    }
    false
}

// ── Test helpers ──────────────────────────────────────────────────────────────

/// A `HOME` for one CLI subprocess that nothing else can see.
///
/// **`mktemp`, not `/tmp`.** Every test here runs the real binary, and the
/// binary reads its `HOME`: `~/.netrc`, and `~/.config` whenever
/// `XDG_CONFIG_HOME` does not decide the question. Handing them all the literal
/// `/tmp` made that one directory shared state between processes that know
/// nothing about each other — not only the tests in this file but every other
/// suite `cargo test --workspace` runs at the same time, plus whatever else is
/// already in `/tmp` on the machine. It held: two `setup detect` tests failed
/// under a loaded parallel run and passed in isolation, which is the shape of a
/// flake nobody can reproduce.
///
/// The returned handle must stay alive for the whole call — dropping it deletes
/// the directory — which is why every caller binds it before spawning.
fn scratch_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("batlehub-cli-home-")
        .tempdir()
        .expect("scratch HOME")
}

/// Run the CLI binary with env-injected server/token and return (success, stdout, stderr).
///
/// A fresh config directory per call, because none of these commands carries
/// state from one invocation to the next: the token arrives in the environment
/// and everything else is asked of the server. A test that *does* need a config
/// directory to survive two calls uses [`cli_cmd_in`] with its own.
fn cli_cmd(args: &[&str], server: &str, token: &str) -> (bool, String, String) {
    let config = scratch_home();
    cli_cmd_in(args, server, token, config.path().to_str().unwrap())
}

/// [`cli_cmd`] with a config directory of the caller's choosing.
///
/// The auth tests read and write stored credentials, so each needs its own —
/// sharing one would let a login in one test satisfy another.
fn cli_cmd_in(
    args: &[&str],
    server: &str,
    token: &str,
    config_home: &str,
) -> (bool, String, String) {
    let home = scratch_home();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_batlehub-cli"))
        .args(args)
        .env("BATLEHUB_SERVER", server)
        .env("BATLEHUB_TOKEN", token)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", config_home)
        .output()
        .expect("failed to run batlehub-cli binary");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Build a minimal `.nupkg` ZIP containing only a `.nuspec` file.
fn make_nupkg(id: &str, version: &str) -> Vec<u8> {
    let nuspec = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>{id}</id>
    <version>{version}</version>
    <description>Integration test package</description>
    <authors>TestAuthor</authors>
  </metadata>
</package>"#
    );
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file(format!("{id}.nuspec"), opts).unwrap();
        zip.write_all(nuspec.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// Publish a minimal NuGet package directly via HTTP (not via CLI) to the local
/// registry endpoint. Used for `publish_nuget_via_cli` which tests the CLI publish
/// path itself. Note: this does NOT populate `InMemoryPackageRepository`; use
/// `TestServer::seed_package` if you need the package to appear in `package list`.
fn http_publish_nuget(base_url: &str, registry: &str, name: &str, version: &str) {
    let nupkg = make_nupkg(name, version);
    let boundary = "batlehub_test_boundary";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"package\"; \
         filename=\"package.nupkg\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(&nupkg);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let resp = reqwest::blocking::Client::new()
        .put(format!("{base_url}/proxy/{registry}/nuget/api/v2/package"))
        .header("Authorization", format!("Bearer {AUTH_TOKEN}"))
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .expect("HTTP publish request failed");

    assert!(
        resp.status().is_success(),
        "HTTP publish returned {}: {}",
        resp.status(),
        resp.text().unwrap_or_default()
    );
}

// ── Tests: registry ───────────────────────────────────────────────────────────

#[test]
fn registry_list_json() {
    let srv = TestServer::start();
    let (ok, stdout, _stderr) =
        cli_cmd(&["registry", "list", "--json"], &srv.base_url(), AUTH_TOKEN);
    assert!(ok, "registry list should succeed");
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("valid JSON array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], REGISTRY);
    assert_eq!(arr[0]["type"], "nuget");
    assert_eq!(arr[0]["mode"], "local");
}

/// A `mise.lock` covering the three shapes `registry suggest` must handle: a
/// GitHub release asset (typed registry), a preset toolchain host (curated
/// generic mirror), and a URL-less backend (mapped by backend prefix).
const SAMPLE_MISE_LOCK: &str = r#"
[[tools."aqua:EmbarkStudios/cargo-deny"]]
version = "0.20.2"
backend = "aqua:EmbarkStudios/cargo-deny"

[tools."aqua:EmbarkStudios/cargo-deny"."platforms.linux-x64"]
url = "https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny.tar.gz"

[[tools.node]]
version = "24.18.0"
backend = "core:node"

[tools.node."platforms.linux-x64"]
url = "https://nodejs.org/dist/v24.18.0/node-v24.18.0-linux-x64.tar.gz"

[[tools.semgrep]]
version = "1.170.0"
backend = "pipx:semgrep"
"#;

#[test]
fn registry_suggest_maps_mise_lock_sources_to_registries() {
    let srv = TestServer::start();
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("mise.lock"), SAMPLE_MISE_LOCK).unwrap();

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "registry",
            "suggest",
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "registry suggest should succeed: {stderr}");
    let val: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let suggested = val["suggested"].as_array().expect("suggested array");

    let by_name = |name: &str| {
        suggested
            .iter()
            .find(|s| s["name"] == name)
            .unwrap_or_else(|| panic!("no '{name}' suggestion in {stdout}"))
    };

    // GitHub release asset → typed adapter, which brings its own upstream.
    assert_eq!(by_name("github")["type"], "github");
    assert!(by_name("github")["upstreams"]
        .as_array()
        .unwrap()
        .is_empty());

    // A preset host → generic mirror with an explicit upstream + allowlist,
    // both of which the server requires for `type = "generic"`.
    let node = by_name("node-dist");
    assert_eq!(node["type"], "generic");
    assert_eq!(node["upstreams"][0], "https://nodejs.org/dist");
    assert!(!node["path_allow"].as_array().unwrap().is_empty());
    assert_eq!(
        node["client_env"]["NODEJS_ORG_MIRROR"],
        serde_json::json!(format!("{}/proxy/node-dist/generic", srv.base_url()))
    );

    // A URL-less backend maps by prefix (`pipx:` → PyPI).
    assert_eq!(by_name("pypi")["type"], "pypi");
}

#[test]
fn registry_suggest_omits_types_the_server_already_has() {
    // The test server exposes one `nuget` registry, so a NuGet project should be
    // reported as already covered rather than suggested again.
    let srv = TestServer::start();
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("MyPkg.nuspec"), "<package/>").unwrap();

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "registry",
            "suggest",
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "registry suggest should succeed: {stderr}");
    let val: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(
        val["suggested"].as_array().unwrap().is_empty(),
        "nuget is already configured, so nothing should be suggested: {stdout}"
    );
    assert_eq!(val["already_configured"][0]["type"], "nuget");

    // …unless the user asks for everything.
    let (ok, stdout, _) = cli_cmd(
        &[
            "registry",
            "suggest",
            "--dir",
            dir.path().to_str().unwrap(),
            "--include-existing",
            "--json",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok);
    let val: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(val["suggested"][0]["type"], "nuget");
}

#[test]
fn registry_suggest_works_without_a_reachable_server() {
    // Scanning is entirely local; an unreachable server may only cost the
    // "already configured" annotation, never the whole command.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("mise.lock"), SAMPLE_MISE_LOCK).unwrap();

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "registry",
            "suggest",
            "--dir",
            dir.path().to_str().unwrap(),
            "--client-env",
        ],
        "http://127.0.0.1:1", // nothing listening
        AUTH_TOKEN,
    );
    assert!(
        ok,
        "suggest must not fail on an unreachable server: {stderr}"
    );
    assert!(stdout.contains("[[registries]]"), "{stdout}");
    assert!(stdout.contains("NODEJS_ORG_MIRROR"), "{stdout}");
}

#[test]
fn registry_suggest_mise_block_is_valid_toml_with_doubled_escapes() {
    // A lone `\.` is a TOML parse error, and mise reacts by logging one line and
    // running on with the whole settings block dropped — so the escaping is the
    // difference between "routed through the proxy" and a silent no-op.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("mise.lock"), SAMPLE_MISE_LOCK).unwrap();

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "registry",
            "suggest",
            "--dir",
            dir.path().to_str().unwrap(),
            "--mise",
            "--json",
        ],
        "https://hub.example.com",
        AUTH_TOKEN,
    );
    assert!(ok, "suggest --mise should succeed: {stderr}");
    let val: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let mise_toml = val["mise_toml"].as_str().expect("mise_toml string");

    let parsed: toml::Value = toml::from_str(mise_toml)
        .unwrap_or_else(|e| panic!("mise block must be valid TOML: {e}\n{mise_toml}"));
    let repl = parsed["settings"]["url_replacements"].as_table().unwrap();

    // Generic mirror: rule derived from the upstream, proxied under /generic/.
    assert_eq!(
        repl[r"regex:^https://nodejs\.org/dist/(.+)"].as_str(),
        Some("https://hub.example.com/proxy/node-dist/generic/$1")
    );
    // GitHub: no `/github/` type segment — that route is /proxy/{name}/{owner}/…
    assert_eq!(
        repl[r"regex:^https://api\.github\.com/repos/(.+)"].as_str(),
        Some("https://hub.example.com/proxy/github/$1")
    );
}

#[test]
fn registry_suggest_on_empty_dir_reports_nothing_found() {
    let dir = tempfile::TempDir::new().unwrap();
    let (ok, stdout, _stderr) = cli_cmd(
        &["registry", "suggest", "--dir", dir.path().to_str().unwrap()],
        "http://127.0.0.1:1",
        AUTH_TOKEN,
    );
    assert!(ok);
    assert!(stdout.contains("No package sources found"), "{stdout}");
}

#[test]
fn registry_info_found() {
    let srv = TestServer::start();
    let (ok, stdout, _stderr) = cli_cmd(
        &["registry", "info", REGISTRY, "--json"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "registry info for known registry should succeed");
    let val: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(val["name"], REGISTRY);
    assert_eq!(val["type"], "nuget");
    assert_eq!(val["mode"], "local");
}

#[test]
fn registry_info_not_found() {
    let srv = TestServer::start();
    let (ok, _stdout, stderr) = cli_cmd(
        &["registry", "info", "no-such-registry", "--json"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(!ok, "registry info for unknown registry should fail");
    assert!(
        stderr.to_lowercase().contains("not found"),
        "stderr should mention 'not found', got: {stderr}"
    );
}

// ── Tests: auth ───────────────────────────────────────────────────────────────

#[test]
fn auth_whoami_authenticated() {
    let srv = TestServer::start();
    let (ok, stdout, _stderr) = cli_cmd(&["auth", "whoami", "--json"], &srv.base_url(), AUTH_TOKEN);
    assert!(ok, "auth whoami should succeed with valid token");
    let val: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(val["user_id"], "test-user");
    assert_eq!(val["role"], "admin");
}

#[test]
fn auth_whoami_anonymous() {
    let srv = TestServer::start();
    // Empty token → anonymous
    let (ok, stdout, _stderr) = cli_cmd(&["auth", "whoami", "--json"], &srv.base_url(), "");
    assert!(ok, "auth whoami without token should succeed (anonymous)");
    let val: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(val["role"], "anonymous");
}

#[test]
fn auth_whoami_table_mode() {
    let srv = TestServer::start();
    let (ok, stdout, _stderr) = cli_cmd(&["auth", "whoami"], &srv.base_url(), AUTH_TOKEN);
    assert!(ok, "auth whoami (table) should succeed with valid token");
    assert!(stdout.contains("test-user"), "stdout: {stdout}");
    assert!(stdout.contains("admin"), "stdout: {stdout}");
    assert!(stdout.contains("static-token"), "stdout: {stdout}");
}

// Managing personal access tokens takes an interactive OIDC login — creating,
// listing *and* revoking. `AUTH_TOKEN` here is a static token from config, i.e.
// a machine credential, and none of the three are open to it. Listing and
// revoking used to be: a leaked PAT or a copied static token could enumerate its
// victim's other tokens and revoke them.

#[test]
fn auth_token_list_requires_oidc_fails() {
    let srv = TestServer::start();
    let (ok, _stdout, stderr) = cli_cmd(
        &["auth", "token", "list", "--json"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(!ok, "token list with a static token session should fail");
    assert!(
        stderr.to_lowercase().contains("oidc"),
        "stderr should mention OIDC, got: {stderr}"
    );
}

#[test]
fn auth_token_create_requires_oidc_fails() {
    let srv = TestServer::start();
    let (ok, _stdout, stderr) = cli_cmd(
        &["auth", "token", "create", "--name", "ci-token"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(!ok, "token create with a static token session should fail");
    assert!(
        stderr.to_lowercase().contains("oidc"),
        "stderr should mention OIDC, got: {stderr}"
    );
}

/// RFC 0011-bis §4.4 — the two ways of naming a token's groups are exclusive.
///
/// `--all-groups` is sugar that resolves the list from `auth whoami` and sends
/// it; combining it with an explicit `--groups` would make the resulting
/// snapshot depend on which one the code happened to read, so clap refuses the
/// combination instead of picking.
#[test]
fn auth_token_create_refuses_groups_and_all_groups_together() {
    let srv = TestServer::start();
    let (ok, _stdout, stderr) = cli_cmd(
        &[
            "auth",
            "token",
            "create",
            "--name",
            "ci-token",
            "--groups",
            "eng",
            "--all-groups",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(!ok, "the two flags must not be usable together");
    assert!(
        stderr.contains("--all-groups") && stderr.contains("--groups"),
        "stderr should name both flags, got: {stderr}"
    );
}

#[test]
fn auth_token_revoke_requires_oidc_fails() {
    let srv = TestServer::start();
    let id = uuid::Uuid::new_v4().to_string();
    let (ok, _stdout, stderr) = cli_cmd(
        &["auth", "token", "revoke", &id],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(!ok, "token revoke with a static token session should fail");
    assert!(
        stderr.to_lowercase().contains("oidc"),
        "stderr should mention OIDC, got: {stderr}"
    );
}

// ── Tests: package ────────────────────────────────────────────────────────────

#[test]
fn package_list_empty() {
    let srv = TestServer::start();
    let (ok, stdout, _stderr) = cli_cmd(
        &["package", "list", "--registry", REGISTRY, "--json"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "package list on empty registry should succeed");
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("valid JSON array");
    assert!(arr.is_empty(), "expected empty list, got {arr:?}");
}

#[test]
fn package_list_after_seed() {
    let srv = TestServer::start();
    srv.seed_package("MyLib", "1.0.0");

    let (ok, stdout, _stderr) = cli_cmd(
        &["package", "list", "--registry", REGISTRY, "--json"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "package list should succeed");
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("valid JSON array");
    assert_eq!(arr.len(), 1, "expected one package");
    assert_eq!(
        arr[0]["name"].as_str().unwrap_or("").to_lowercase(),
        "mylib"
    );
    assert_eq!(arr[0]["version"], "1.0.0");
    assert_eq!(arr[0]["registry"], REGISTRY);
}

#[test]
fn package_versions_after_seed() {
    let srv = TestServer::start();
    srv.seed_package("SomeLib", "2.3.4");
    srv.seed_package("SomeLib", "2.3.5");

    let (ok, stdout, _stderr) = cli_cmd(
        &["package", "versions", REGISTRY, "SomeLib", "--json"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "package versions should succeed");
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("valid JSON array");
    assert!(
        arr.iter().any(|v| v["version"] == "2.3.4"),
        "expected version 2.3.4 in {arr:?}"
    );
    assert!(
        arr.iter().any(|v| v["version"] == "2.3.5"),
        "expected version 2.3.5 in {arr:?}"
    );
}

// ── Tests: version lifecycle ──────────────────────────────────────────────────
//
// Yank/unyank/delete operate on InMemoryLocalRegistry (the local-registry backend).
// Verification is done via the NuGet flat-index endpoint which also reads from
// InMemoryLocalRegistry. We don't use `package list` here because that queries
// InMemoryPackageRepository, a separate in-memory store (in Postgres they share
// the same tables).

fn nuget_flat_versions(base_url: &str, registry: &str, id_lower: &str) -> Vec<String> {
    let resp = reqwest::blocking::Client::new()
        .get(format!(
            "{base_url}/proxy/{registry}/nuget/v3/flat/{id_lower}/index.json"
        ))
        .header("Authorization", format!("Bearer {AUTH_TOKEN}"))
        .send()
        .expect("GET flat index");
    if !resp.status().is_success() {
        return vec![];
    }
    let body: serde_json::Value = resp.json().expect("flat index JSON");
    body["versions"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

#[test]
fn version_yank_and_unyank() {
    let srv = TestServer::start();
    http_publish_nuget(&srv.base_url(), REGISTRY, "YankLib", "0.1.0");

    // Confirm visible before yank
    assert!(
        nuget_flat_versions(&srv.base_url(), REGISTRY, "yanklib").contains(&"0.1.0".to_owned()),
        "version should be in flat index before yank"
    );

    // Yank
    let (ok, _stdout, stderr) = cli_cmd(
        &["version", "yank", REGISTRY, "YankLib", "0.1.0"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "version yank should succeed; stderr: {stderr}");

    // Yanked versions are filtered from the flat index
    assert!(
        !nuget_flat_versions(&srv.base_url(), REGISTRY, "yanklib").contains(&"0.1.0".to_owned()),
        "yanked version should not appear in flat index"
    );

    // Unyank
    let (ok, _stdout, stderr) = cli_cmd(
        &["version", "unyank", REGISTRY, "YankLib", "0.1.0"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "version unyank should succeed; stderr: {stderr}");

    // Unyanked version reappears in flat index
    assert!(
        nuget_flat_versions(&srv.base_url(), REGISTRY, "yanklib").contains(&"0.1.0".to_owned()),
        "unyanked version should reappear in flat index"
    );
}

#[test]
fn version_delete() {
    let srv = TestServer::start();
    http_publish_nuget(&srv.base_url(), REGISTRY, "DeleteLib", "3.0.0");

    // Confirm visible before delete
    assert!(
        nuget_flat_versions(&srv.base_url(), REGISTRY, "deletelib").contains(&"3.0.0".to_owned()),
        "version should be in flat index before delete"
    );

    let (ok, _stdout, stderr) = cli_cmd(
        &["version", "delete", REGISTRY, "DeleteLib", "3.0.0", "--yes"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "version delete --yes should succeed; stderr: {stderr}");

    // Deleted version no longer in flat index
    assert!(
        !nuget_flat_versions(&srv.base_url(), REGISTRY, "deletelib").contains(&"3.0.0".to_owned()),
        "deleted version should not appear in flat index"
    );
}

// ── Tests: publish ────────────────────────────────────────────────────────────

#[test]
fn publish_nuget_via_cli() {
    let srv = TestServer::start();
    let tmp = tempfile::tempdir().unwrap();
    let nupkg_path = tmp.path().join("TestPkg.1.2.3.nupkg");
    std::fs::write(&nupkg_path, make_nupkg("TestPkg", "1.2.3")).unwrap();

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "publish",
            nupkg_path.to_str().unwrap(),
            "--registry",
            REGISTRY,
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "publish should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("Published successfully"),
        "expected success message in stdout, got: {stdout}"
    );
}

#[test]
fn publish_npm_via_cli() {
    let srv = TestServer::start_with(&[("test-npm", "npm")]);
    let tmp = tempfile::tempdir().unwrap();
    let tgz_path = tmp.path().join("left-pad-1.3.0.tgz");
    std::fs::write(&tgz_path, b"fake-tarball-content").unwrap();

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "publish",
            tgz_path.to_str().unwrap(),
            "--registry",
            "test-npm",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "npm publish should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("Published successfully"),
        "expected success message in stdout, got: {stdout}"
    );
}

#[test]
fn publish_pypi_via_cli() {
    let srv = TestServer::start_with(&[("test-pypi", "pypi")]);
    let tmp = tempfile::tempdir().unwrap();
    let whl_path = tmp.path().join("my_pkg-1.0.0-py3-none-any.whl");
    std::fs::write(&whl_path, b"fake-wheel-content").unwrap();

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "publish",
            whl_path.to_str().unwrap(),
            "--registry",
            "test-pypi",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "pypi publish should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("Published successfully"),
        "expected success message in stdout, got: {stdout}"
    );
}

#[test]
fn publish_cargo_via_cli() {
    let srv = TestServer::start_with(&[("test-cargo", "cargo")]);
    let tmp = tempfile::tempdir().unwrap();
    let crate_path = tmp.path().join("my-crate-0.1.0.crate");
    std::fs::write(&crate_path, b"fake-crate-content").unwrap();

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "publish",
            crate_path.to_str().unwrap(),
            "--registry",
            "test-cargo",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "cargo publish should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("Published successfully"),
        "expected success message in stdout, got: {stdout}"
    );
}

// ── Tests: completion ─────────────────────────────────────────────────────────

/// Shell completion output requires no running server; the binary exits early.
#[test]
fn completion_bash_produces_output() {
    let (ok, stdout, stderr) = cli_cmd(&["completion", "bash"], "http://127.0.0.1:1", "");
    assert!(ok, "completion bash failed; stderr: {stderr}");
    assert!(!stdout.is_empty(), "bash completion should produce output");
    assert!(
        stdout.contains("batlehub-cli"),
        "bash completion should mention the binary name; got: {stdout:.200}"
    );
}

#[test]
fn completion_zsh_produces_output() {
    let (ok, stdout, stderr) = cli_cmd(&["completion", "zsh"], "http://127.0.0.1:1", "");
    assert!(ok, "completion zsh failed; stderr: {stderr}");
    assert!(!stdout.is_empty(), "zsh completion should produce output");
}

// ── Tests: auth login / refresh ───────────────────────────────────────────────

/// `auth login --kubernetes-token-path` should save the path to the config and
/// exit 0, without contacting any OIDC endpoint.
#[test]
fn auth_login_kubernetes_saves_config() {
    // Create an isolated config directory so this test does not interfere with others.
    let config_dir = tempfile::tempdir().unwrap();
    let token_dir = tempfile::tempdir().unwrap();
    let token_file = token_dir.path().join("sa-token");
    std::fs::write(&token_file, "my-k8s-service-account-token").unwrap();

    let token_path = token_file.to_str().unwrap();
    let srv = TestServer::start();

    let home = scratch_home();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_batlehub-cli"))
        .args(["auth", "login", "--kubernetes-token-path", token_path])
        .env("BATLEHUB_SERVER", srv.base_url())
        .env("BATLEHUB_TOKEN", AUTH_TOKEN)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", config_dir.path().to_str().unwrap())
        .output()
        .expect("failed to run batlehub-cli");

    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "auth login kubernetes should succeed; stderr: {stderr}"
    );

    let config_path = config_dir.path().join("batlehub/config.toml");
    assert!(
        config_path.exists(),
        "config file should be written at {config_path:?}"
    );
    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains(token_path),
        "config should contain the kubernetes_token_path; content: {content}"
    );
}

/// `auth refresh` with no stored refresh token should exit non-zero and print
/// a helpful error message.
#[test]
fn auth_refresh_no_stored_token_fails() {
    let config_dir = tempfile::tempdir().unwrap();
    let srv = TestServer::start();

    let (ok, _, stderr) = cli_cmd_in(
        &["auth", "refresh"],
        &srv.base_url(),
        AUTH_TOKEN,
        config_dir.path().to_str().unwrap(),
    );

    assert!(!ok, "auth refresh with no stored token should fail");
    assert!(
        stderr.contains("refresh token") || stderr.contains("auth login"),
        "error should guide the user; stderr: {stderr}"
    );
}

/// `auth login` without `--kubernetes-token-path` against a server with no OIDC
/// providers configured should bail with a helpful message.
#[test]
fn auth_login_without_kubernetes_no_oidc_fails() {
    let config_dir = tempfile::tempdir().unwrap();
    let srv = TestServer::start();

    let (ok, _, stderr) = cli_cmd_in(
        &["auth", "login"],
        &srv.base_url(),
        AUTH_TOKEN,
        config_dir.path().to_str().unwrap(),
    );

    assert!(!ok, "auth login without OIDC configured should fail");
    assert!(
        stderr.contains("OIDC is not configured"),
        "error should mention OIDC is not configured; stderr: {stderr}"
    );
}

/// `auth refresh` with a stored refresh token, but against a server with no OIDC
/// SSO configured, should call the refresh endpoint and surface its error.
#[test]
fn auth_refresh_with_stored_token_but_oidc_unavailable_fails() {
    let config_dir = tempfile::tempdir().unwrap();
    let srv = TestServer::start();

    let batlehub_dir = config_dir.path().join("batlehub");
    std::fs::create_dir_all(&batlehub_dir).unwrap();
    std::fs::write(
        batlehub_dir.join("config.toml"),
        r#"
[default]
token = "stale-token"
oidc_refresh_token = "stored-refresh-token"
oidc_expires_at = 0
"#,
    )
    .unwrap();

    let (ok, _, stderr) = cli_cmd_in(
        &["auth", "refresh"],
        &srv.base_url(),
        AUTH_TOKEN,
        config_dir.path().to_str().unwrap(),
    );

    assert!(
        !ok,
        "auth refresh against a server without OIDC SSO should fail"
    );
    assert!(
        stderr.contains("OIDC SSO is not configured"),
        "error should surface the server's response; stderr: {stderr}"
    );
}

// ── Tests: admin ──────────────────────────────────────────────────────────────

#[test]
fn admin_quota_list_json_empty() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "quota", "list", "--json"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "admin quota list should succeed; stderr: {stderr}");
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("valid JSON array");
    assert!(arr.is_empty(), "expected empty quota list, got: {stdout}");
}

#[test]
fn admin_quota_list_for_registry_table() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "quota", "list", "-r", REGISTRY],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "admin quota list -r should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("Registry") && stdout.contains("Storage (bytes)"),
        "table header expected; stdout: {stdout}"
    );
}

#[test]
fn admin_quota_reset() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "quota", "reset", REGISTRY, "some-user"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "admin quota reset should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("Reset quota for some-user in test-nuget"),
        "stdout: {stdout}"
    );
}

#[test]
fn admin_ip_block_add_list_remove() {
    let srv = TestServer::start();
    let base = srv.base_url();

    let (ok, stdout, stderr) = cli_cmd(&["admin", "ip-block", "list", "--json"], &base, AUTH_TOKEN);
    assert!(ok, "ip-block list should succeed; stderr: {stderr}");
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("valid JSON array");
    assert!(arr.is_empty(), "expected no blocks yet, got: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "admin",
            "ip-block",
            "add",
            "10.0.0.1",
            "--reason",
            "test block",
        ],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "ip-block add should succeed; stderr: {stderr}");
    assert!(stdout.contains("Blocked 10.0.0.1"), "stdout: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(&["admin", "ip-block", "list", "--json"], &base, AUTH_TOKEN);
    assert!(ok, "ip-block list should succeed; stderr: {stderr}");
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("valid JSON array");
    assert_eq!(arr.len(), 1, "stdout: {stdout}");
    assert_eq!(arr[0]["ip"], "10.0.0.1");
    assert_eq!(arr[0]["reason"], "test block");

    let (ok, stdout, stderr) = cli_cmd(&["admin", "ip-block", "list"], &base, AUTH_TOKEN);
    assert!(ok, "ip-block list (table) should succeed; stderr: {stderr}");
    assert!(stdout.contains("10.0.0.1"), "stdout: {stdout}");
    assert!(stdout.contains("1 block(s)"), "stdout: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "ip-block", "remove", "10.0.0.1"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "ip-block remove should succeed; stderr: {stderr}");
    assert!(stdout.contains("Unblocked 10.0.0.1"), "stdout: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(&["admin", "ip-block", "list", "--json"], &base, AUTH_TOKEN);
    assert!(ok, "ip-block list should succeed; stderr: {stderr}");
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("valid JSON array");
    assert!(arr.is_empty(), "expected block removed, got: {stdout}");
}

#[test]
fn admin_ip_block_add_invalid_ip_fails() {
    let srv = TestServer::start();
    let (ok, _stdout, stderr) = cli_cmd(
        &["admin", "ip-block", "add", "not-an-ip"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(!ok, "ip-block add with invalid IP should fail");
    assert!(
        stderr.contains("400") || stderr.to_lowercase().contains("ip address"),
        "stderr should mention the bad IP; got: {stderr}"
    );
}

#[test]
fn admin_banner_set_and_clear() {
    let srv = TestServer::start();
    let base = srv.base_url();

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "admin",
            "banner",
            "set",
            "Maintenance tonight",
            "--level",
            "warning",
        ],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "banner set should succeed; stderr: {stderr}");
    assert!(stdout.contains("Banner set (warning)"), "stdout: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(&["admin", "banner", "clear"], &base, AUTH_TOKEN);
    assert!(ok, "banner clear should succeed; stderr: {stderr}");
    assert!(stdout.contains("Banner cleared"), "stdout: {stdout}");
}

#[test]
fn admin_cache_clear() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "cache", "clear", REGISTRY],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "cache clear should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("Cache cleared for test-nuget"),
        "stdout: {stdout}"
    );
}

/// The CLI's `WarmRequest` now sends a valid `{"packages": [...]}` body (the
/// server accepts `packages`/`paths`), so the request reaches the handler. The
/// test server registers no `WarmingService`, so the handler responds 404
/// "warming not configured" — this pins that error-handling path through
/// `handle_cache`/`cache_warm`.
#[test]
fn admin_cache_warm_not_configured() {
    let srv = TestServer::start();
    let (ok, _stdout, stderr) = cli_cmd(
        &["admin", "cache", "warm", REGISTRY, "--packages", "pkg1"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(
        !ok,
        "cache warm should fail when warming is not configured for the registry"
    );
    assert!(stderr.contains("HTTP 404"), "stderr: {stderr}");
}

#[test]
fn admin_config_reload_disabled() {
    let srv = TestServer::start();
    let (ok, _stdout, stderr) =
        cli_cmd(&["admin", "config", "reload"], &srv.base_url(), AUTH_TOKEN);
    assert!(!ok, "config reload should fail when hot reload is disabled");
    assert!(
        stderr.contains("HTTP 503") && stderr.contains("hot reload is disabled"),
        "stderr: {stderr}"
    );
}

#[test]
fn admin_config_changes_errors_without_pool() {
    let srv = TestServer::start();
    let (ok, _stdout, stderr) =
        cli_cmd(&["admin", "config", "changes"], &srv.base_url(), AUTH_TOKEN);
    assert!(
        !ok,
        "config changes should fail without a database pool configured"
    );
    assert!(stderr.contains("HTTP 500"), "stderr: {stderr}");
}

#[test]
fn admin_audit_log_json_empty() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "audit-log", "--json"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "audit-log --json should succeed; stderr: {stderr}");
    let body: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON object");
    assert!(
        body["items"].as_array().unwrap().is_empty(),
        "stdout: {stdout}"
    );
    assert_eq!(body["total"], 0);
}

#[test]
fn admin_audit_log_table_empty() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(&["admin", "audit-log"], &srv.base_url(), AUTH_TOKEN);
    assert!(ok, "audit-log should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("Time") && stdout.contains("Denied"),
        "table header expected; stdout: {stdout}"
    );
    assert!(stdout.contains("0 entry/entries"), "stdout: {stdout}");
}

#[test]
fn admin_purge_audit_log() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(
        &[
            "admin",
            "audit-log",
            "--purge-before",
            "2026-01-01T00:00:00Z",
            "--json",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(
        ok,
        "audit-log --purge-before should succeed; stderr: {stderr}"
    );
    let body: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON object");
    assert_eq!(body["deleted"], 0, "stdout: {stdout}");
}

#[test]
fn admin_purge_audit_log_table_mode() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(
        &[
            "admin",
            "audit-log",
            "--purge-before",
            "2026-01-01T00:00:00Z",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(
        ok,
        "audit-log --purge-before should succeed; stderr: {stderr}"
    );
    assert!(
        stdout.contains("Deleted 0 audit-log row(s) older than 2026-01-01T00:00:00Z"),
        "stdout: {stdout}"
    );
}

#[test]
fn admin_audit_log_table_shows_seeded_event() {
    let srv = TestServer::start();
    let base = srv.base_url();
    cli_cmd(&["admin", "users", "block", "someone"], &base, AUTH_TOKEN);

    let (ok, stdout, stderr) = cli_cmd(&["admin", "audit-log"], &base, AUTH_TOKEN);
    assert!(ok, "audit-log should succeed; stderr: {stderr}");
    assert!(stdout.contains("1 entry/entries"), "stdout: {stdout}");
}

/// `--action` is what makes "what was deleted here" a question the CLI can ask:
/// without it the answer is buried under every download in the trail.
#[test]
fn admin_audit_log_filters_by_action() {
    let srv = TestServer::start();
    let base = srv.base_url();
    // A download (via seed_package) and an account-wide block: two actions.
    srv.seed_package("left-alone", "1.0.0");
    cli_cmd(&["admin", "users", "block", "someone"], &base, AUTH_TOKEN);

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "audit-log", "--action", "block_user"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "audit-log --action should succeed; stderr: {stderr}");
    assert!(stdout.contains("1 entry/entries"), "stdout: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "audit-log", "--action", "block_user,download"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "a comma-separated set should succeed; stderr: {stderr}");
    assert!(stdout.contains("2 entry/entries"), "stdout: {stdout}");
}

/// A typo is a `400` with the alternatives, not an empty table that reads as
/// "nothing happened".
#[test]
fn admin_audit_log_rejects_an_unknown_action() {
    let srv = TestServer::start();
    let (ok, _stdout, stderr) = cli_cmd(
        &["admin", "audit-log", "--action", "deleted"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(!ok, "an unknown action must fail");
    assert!(stderr.contains("unknown audit action"), "stderr: {stderr}");
}

/// The preview an operator runs before turning a size cap loose. The test
/// server configures no eviction strategies, so this asserts the command, its
/// flag and the report's shape — 404 would mean the route or the query never
/// reached the service.
#[test]
fn admin_cache_evict_dry_run_reports() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "cache", "evict", REGISTRY, "--dry-run"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "cache evict --dry-run should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("dry run — nothing was evicted"),
        "stdout: {stdout}"
    );
}

#[test]
fn admin_cache_evict_runs_live_without_the_flag() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "cache", "evict", REGISTRY],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "cache evict should succeed; stderr: {stderr}");
    assert!(stdout.contains("evicted"), "stdout: {stdout}");
    assert!(
        !stdout.contains("dry run"),
        "a cached copy comes back on the next request, so live is the default: {stdout}"
    );
}

/// The first sweep of a fresh estate deletes nothing and says why — the
/// two-pass grace, which an operator reading "0 deleted" would otherwise take
/// for "this command does nothing".
#[test]
fn admin_cache_coherence_reports() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "cache", "coherence", REGISTRY],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "cache coherence should succeed; stderr: {stderr}");
    assert!(stdout.contains("Cache coherence on"), "stdout: {stdout}");
    assert!(stdout.contains("deleted 0"), "stdout: {stdout}");
}

#[test]
fn admin_cache_coherence_dry_run_says_it_armed_nothing() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "cache", "coherence", REGISTRY, "--dry-run"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(
        ok,
        "cache coherence --dry-run should succeed; stderr: {stderr}"
    );
    assert!(
        stdout.contains("nothing was deleted, nothing was armed"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("would delete 0"), "stdout: {stdout}");
}

#[test]
fn admin_cache_warm_rejects_empty_selection() {
    let srv = TestServer::start();
    let (ok, _stdout, stderr) = cli_cmd(
        &["admin", "cache", "warm", REGISTRY],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(!ok, "cache warm without --packages/--paths should fail");
    assert!(
        stderr.contains("specify --packages and/or --paths"),
        "stderr: {stderr}"
    );
}

#[test]
fn admin_stats_json_and_table() {
    let srv = TestServer::start();
    let base = srv.base_url();

    let (ok, stdout, stderr) = cli_cmd(&["admin", "stats", "--json"], &base, AUTH_TOKEN);
    assert!(ok, "admin stats --json should succeed; stderr: {stderr}");
    let body: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON object");
    assert!(body["aggregate"].is_object(), "stdout: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(&["admin", "stats"], &base, AUTH_TOKEN);
    assert!(ok, "admin stats should succeed; stderr: {stderr}");
    assert!(stdout.contains("Since startup"), "stdout: {stdout}");
    assert!(stdout.contains("Aggregate"), "stdout: {stdout}");
}

#[test]
fn admin_health_json_and_table() {
    let srv = TestServer::start();
    let base = srv.base_url();

    let (ok, stdout, stderr) = cli_cmd(&["admin", "health", "--json"], &base, AUTH_TOKEN);
    assert!(ok, "admin health --json should succeed; stderr: {stderr}");
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("valid JSON array");
    assert_eq!(arr.len(), 1, "one configured registry; stdout: {stdout}");
    assert_eq!(arr[0]["registry"], REGISTRY);

    let (ok, stdout, stderr) = cli_cmd(&["admin", "health"], &base, AUTH_TOKEN);
    assert!(ok, "admin health should succeed; stderr: {stderr}");
    assert!(stdout.contains(REGISTRY), "stdout: {stdout}");
}

#[test]
fn admin_visibility_get_and_set() {
    let srv = TestServer::start();
    let base = srv.base_url();
    http_publish_nuget(&base, REGISTRY, "VisLib", "1.0.0");

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "visibility", "get", REGISTRY, "VisLib"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "visibility get should succeed; stderr: {stderr}");
    assert!(stdout.contains("public"), "stdout: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "visibility", "get", REGISTRY, "VisLib", "--json"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "visibility get --json should succeed; stderr: {stderr}");
    let body: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON object");
    assert_eq!(body["visibility"], "public");

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "visibility", "set", REGISTRY, "VisLib", "internal"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "visibility set should succeed; stderr: {stderr}");
    assert!(stdout.contains("internal"), "stdout: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "visibility", "get", REGISTRY, "VisLib"],
        &base,
        AUTH_TOKEN,
    );
    assert!(
        ok,
        "visibility get after set should succeed; stderr: {stderr}"
    );
    assert!(stdout.contains("internal"), "stdout: {stdout}");
}

#[test]
fn admin_users_block_list_unblock() {
    let srv = TestServer::start();
    let base = srv.base_url();

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "users", "list-blocked", "--json"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "users list-blocked should succeed; stderr: {stderr}");
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("valid JSON array");
    assert!(arr.is_empty(), "stdout: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "users", "block", "bad-actor", "--reason", "abuse"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "users block should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("Blocked user bad-actor"),
        "stdout: {stdout}"
    );

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "users", "list-blocked", "--json"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "users list-blocked should succeed; stderr: {stderr}");
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("valid JSON array");
    assert_eq!(arr.len(), 1, "stdout: {stdout}");
    assert_eq!(arr[0]["user_id"], "bad-actor");

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "users", "unblock", "bad-actor"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "users unblock should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("Unblocked user bad-actor"),
        "stdout: {stdout}"
    );
}

#[test]
fn admin_users_list_blocked_table_mode() {
    let srv = TestServer::start();
    let base = srv.base_url();
    cli_cmd(
        &["admin", "users", "block", "table-user"],
        &base,
        AUTH_TOKEN,
    );

    let (ok, stdout, stderr) = cli_cmd(&["admin", "users", "list-blocked"], &base, AUTH_TOKEN);
    assert!(
        ok,
        "users list-blocked (table) should succeed; stderr: {stderr}"
    );
    assert!(stdout.contains("table-user"), "stdout: {stdout}");
    assert!(stdout.contains("Blocked At"), "stdout: {stdout}");
}

#[test]
fn admin_namespace_list_claim_release() {
    let srv = TestServer::start();
    let base = srv.base_url();

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "namespace", "list", REGISTRY, "--json"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "namespace list should succeed; stderr: {stderr}");
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("valid JSON array");
    assert!(arr.is_empty(), "stdout: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "admin",
            "namespace",
            "claim",
            REGISTRY,
            "frontend",
            "team-a",
        ],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "namespace claim should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("Claimed namespace prefix 'frontend'"),
        "stdout: {stdout}"
    );

    let (ok, stdout, stderr) =
        cli_cmd(&["admin", "namespace", "list", REGISTRY], &base, AUTH_TOKEN);
    assert!(
        ok,
        "namespace list (table) should succeed; stderr: {stderr}"
    );
    assert!(stdout.contains("frontend"), "stdout: {stdout}");
    assert!(stdout.contains("team-a"), "stdout: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "namespace", "release", REGISTRY, "frontend"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "namespace release should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("Released namespace prefix 'frontend'"),
        "stdout: {stdout}"
    );

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "namespace", "list", REGISTRY, "--json"],
        &base,
        AUTH_TOKEN,
    );
    assert!(
        ok,
        "namespace list after release should succeed; stderr: {stderr}"
    );
    let arr: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("valid JSON array");
    assert!(arr.is_empty(), "stdout: {stdout}");
}

#[test]
fn admin_sbom_get_and_export_error_without_service() {
    // TestServer doesn't wire a `SbomService`, so these correctly fail —
    // still exercises the CLI's HTTP round trip and error propagation.
    let srv = TestServer::start();
    let base = srv.base_url();

    let (ok, _stdout, stderr) = cli_cmd(
        &["admin", "sbom", "get", REGISTRY, "SomeLib", "1.0.0"],
        &base,
        AUTH_TOKEN,
    );
    assert!(!ok, "sbom get should fail without a configured service");
    assert!(!stderr.is_empty(), "stderr: {stderr}");

    let (ok, _stdout, stderr) = cli_cmd(&["admin", "sbom", "export"], &base, AUTH_TOKEN);
    assert!(!ok, "sbom export should fail without a configured service");
    assert!(!stderr.is_empty(), "stderr: {stderr}");
}

#[test]
fn admin_bulk_yank_unyank_delete() {
    let srv = TestServer::start();
    let base = srv.base_url();
    http_publish_nuget(&base, REGISTRY, "BulkLib", "1.0.0");

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "bulk", "yank", REGISTRY, "BulkLib@1.0.0", "--json"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "bulk yank should succeed; stderr: {stderr}");
    let body: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON object");
    assert_eq!(body["processed"], 1, "stdout: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "bulk", "unyank", REGISTRY, "BulkLib@1.0.0"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "bulk unyank should succeed; stderr: {stderr}");
    assert!(stdout.contains("processed=1"), "stdout: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "bulk", "delete", REGISTRY, "BulkLib@1.0.0"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "bulk delete should succeed; stderr: {stderr}");
    assert!(stdout.contains("processed=1"), "stdout: {stdout}");
}

#[test]
fn admin_bulk_rejects_malformed_package_ref() {
    let srv = TestServer::start();
    let (ok, _stdout, stderr) = cli_cmd(
        &["admin", "bulk", "yank", REGISTRY, "no-at-sign"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(!ok, "bulk yank without name@version should fail");
    assert!(stderr.contains("expected name@version"), "stderr: {stderr}");
}

#[test]
fn admin_deprecate_undeprecate_unlist_relist() {
    let srv = TestServer::start();
    let base = srv.base_url();
    http_publish_nuget(&base, REGISTRY, "DepLib", "1.0.0");

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "admin",
            "deprecate",
            REGISTRY,
            "DepLib",
            "1.0.0",
            "--message",
            "use v2",
        ],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "deprecate should succeed; stderr: {stderr}");
    assert!(stdout.contains("Deprecated"), "stdout: {stdout}");

    // The rest of the lifecycle takes the same three arguments and differs
    // only in the word it reports back.
    for (subcommand, reported) in [
        ("undeprecate", "Undeprecated"),
        ("unlist", "Unlisted"),
        ("relist", "Relisted"),
    ] {
        let (ok, stdout, stderr) = cli_cmd(
            &["admin", subcommand, REGISTRY, "DepLib", "1.0.0"],
            &base,
            AUTH_TOKEN,
        );
        assert!(ok, "{subcommand} should succeed; stderr: {stderr}");
        assert!(stdout.contains(reported), "stdout: {stdout}");
    }
}

#[test]
fn admin_access_check_allow_and_json() {
    let srv = TestServer::start();
    let base = srv.base_url();

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "admin",
            "access-check",
            "-r",
            REGISTRY,
            "-p",
            "SomeLib",
            "-v",
            "1.0.0",
        ],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "access-check should succeed; stderr: {stderr}");
    assert!(stdout.contains("ALLOW"), "stdout: {stdout}");

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "admin",
            "access-check",
            "-r",
            REGISTRY,
            "-p",
            "SomeLib",
            "-v",
            "1.0.0",
            "--json",
        ],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "access-check --json should succeed; stderr: {stderr}");
    let body: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON object");
    assert_eq!(body["decision"], "allow", "stdout: {stdout}");
}

#[test]
fn admin_notifications_channels_and_list_error_without_service() {
    // TestServer doesn't wire a `NotificationService` (it needs channel config
    // like SMTP/webhook targets), so these correctly 503 "not configured" —
    // still exercises the CLI's HTTP round trip and error propagation.
    let srv = TestServer::start();
    let base = srv.base_url();

    let (ok, _stdout, stderr) = cli_cmd(
        &["admin", "notifications", "channels", "--json"],
        &base,
        AUTH_TOKEN,
    );
    assert!(!ok, "notifications channels should fail without a service");
    assert!(stderr.contains("503"), "stderr: {stderr}");

    let (ok, _stdout, stderr) = cli_cmd(
        &["admin", "notifications", "list", "--json"],
        &base,
        AUTH_TOKEN,
    );
    assert!(!ok, "notifications list should fail without a service");
    assert!(stderr.contains("503"), "stderr: {stderr}");
}

#[test]
fn admin_export_audit_log_json_default() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(&["admin", "export-audit-log"], &srv.base_url(), AUTH_TOKEN);
    assert!(ok, "export-audit-log should succeed; stderr: {stderr}");
    let _: Vec<serde_json::Value> =
        serde_json::from_str(&stdout).expect("export-audit-log default output should be JSON");
}

#[test]
fn admin_export_audit_log_csv_to_file() {
    let srv = TestServer::start();
    let tmp = tempfile::tempdir().unwrap();
    let out_path = tmp.path().join("audit.csv");
    let (ok, stdout, stderr) = cli_cmd(
        &[
            "admin",
            "export-audit-log",
            "--format",
            "csv",
            "-o",
            out_path.to_str().unwrap(),
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(
        ok,
        "export-audit-log --format csv -o should succeed; stderr: {stderr}"
    );
    assert!(stdout.contains("Exported to"), "stdout: {stdout}");
    let content = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        content.starts_with("id,timestamp,user_id"),
        "content: {content}"
    );
}

// ── Tests: config ─────────────────────────────────────────────────────────────

#[test]
fn config_show_defaults() {
    let config_dir = tempfile::tempdir().unwrap();
    let home = scratch_home();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_batlehub-cli"))
        .args(["config", "show"])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", config_dir.path().to_str().unwrap())
        .output()
        .expect("failed to run batlehub-cli");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "config show should succeed; stdout: {stdout}"
    );
    assert!(
        stdout.contains("server_url: http://localhost:8080"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("token:      (not set)"), "stdout: {stdout}");
    assert!(stdout.contains("registry:   (not set)"), "stdout: {stdout}");
}

#[test]
fn config_set_then_show_masks_token() {
    let config_dir = tempfile::tempdir().unwrap();
    // One `HOME` for all four calls — the config directory below is what has to
    // persist between them, and this keeps them equally isolated from everything
    // outside this test.
    let home = scratch_home();
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_batlehub-cli"))
            .args(args)
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", config_dir.path().to_str().unwrap())
            .output()
            .expect("failed to run batlehub-cli")
    };

    let out = run(&["config", "set", "server_url", "http://example.com"]);
    assert!(out.status.success(), "set server_url should succeed");
    assert!(String::from_utf8_lossy(&out.stdout).contains("Set server_url = http://example.com"));

    let out = run(&["config", "set", "token", "mysecrettoken1234"]);
    assert!(out.status.success(), "set token should succeed");

    let out = run(&["config", "set", "registry", "my-registry"]);
    assert!(out.status.success(), "set registry should succeed");

    let out = run(&["config", "show"]);
    assert!(out.status.success(), "config show should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("server_url: http://example.com"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("token:      myse…1234"), "stdout: {stdout}");
    assert!(
        stdout.contains("registry:   my-registry"),
        "stdout: {stdout}"
    );
}

#[test]
fn config_set_unknown_key_fails() {
    let config_dir = tempfile::tempdir().unwrap();
    let home = scratch_home();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_batlehub-cli"))
        .args(["config", "set", "bogus", "value"])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", config_dir.path().to_str().unwrap())
        .output()
        .expect("failed to run batlehub-cli");

    assert!(
        !out.status.success(),
        "config set with unknown key should fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown key"), "stderr: {stderr}");
}

// ── Tests: setup detect ───────────────────────────────────────────────────────
//
// These tests exercise `batlehub-cli setup detect --dir <path>` (and its JSON
// variant) to verify that the TUI setup-wizard detection logic works end-to-end
// when invoked from the compiled binary. No server connection is needed.

/// Helper: run `setup detect --json --dir <dir>` and return the parsed JSON array.
fn setup_detect_json(dir: &std::path::Path) -> (bool, Vec<serde_json::Value>, String) {
    setup_detect_json_depth(dir, 0)
}

/// `setup detect` against `server` in `dir`, with `extra` flags in between.
///
/// Its own `HOME` and its own config directory, both from `mktemp`: these run as
/// subprocesses against the real binary, and a shared config directory would let
/// one test's cached registry list answer another's question. The detection
/// walks `HOME` too, so a shared one lets anything already sitting in `/tmp`
/// decide what this test sees.
fn setup_detect(server: &str, dir: &std::path::Path, extra: &[&str]) -> (bool, String, String) {
    let mut args: Vec<&str> = vec!["--server", server, "setup", "detect"];
    args.extend_from_slice(extra);
    args.push("--dir");
    let home = scratch_home();
    let config = scratch_home();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_batlehub-cli"))
        .args(&args)
        .arg(dir)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", config.path())
        .output()
        .expect("failed to run batlehub-cli");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The marker `load_registries` prints on both of its fallback arms.
const PLACEHOLDER_NOTE: &str = "using placeholders.";

/// [`setup_detect`], for the tests that assert on what the *server* answered.
///
/// `load_registries` gives the server five seconds and then prints instructions
/// carrying `<registry>` rather than failing — the right behaviour for a wizard,
/// and a trap for a test that asserts on the resolved name. On a loaded machine
/// (two test binaries at once, each starting its own in-process server) that
/// deadline is missable, and the assertion then fails on the *snippet* while the
/// reason sits in a stderr this helper used to throw away. So: retry, and if it
/// never gets an answer, fail with the note rather than with a diff of the
/// snippet.
///
/// The retry is for the deadline, not for a wrong answer — a server that
/// responds with the wrong registry still fails on the first pass.
fn setup_detect_resolved(server: &str, dir: &std::path::Path, extra: &[&str]) -> String {
    let mut note = String::new();
    for _ in 0..4 {
        let (ok, stdout, stderr) = setup_detect(server, dir, extra);
        assert!(ok, "setup detect failed: {stdout}\nstderr: {stderr}");
        if !stderr.contains(PLACEHOLDER_NOTE) {
            return stdout;
        }
        note = stderr;
    }
    panic!("setup detect never got an answer out of the test server: {note}");
}

fn setup_detect_json_depth(
    dir: &std::path::Path,
    depth: usize,
) -> (bool, Vec<serde_json::Value>, String) {
    let home = scratch_home();
    let config = scratch_home();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_batlehub-cli"))
        .args([
            "setup",
            "detect",
            "--json",
            "--dir",
            dir.to_str().unwrap(),
            "--depth",
            &depth.to_string(),
        ])
        .env("BATLEHUB_SERVER", "http://127.0.0.1:1") // no real server needed
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", config.path())
        .output()
        .expect("failed to run batlehub-cli");
    let ok = out.status.success();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let items: Vec<serde_json::Value> = if ok {
        serde_json::from_slice(&out.stdout).unwrap_or_default()
    } else {
        vec![]
    };
    (ok, items, stderr)
}

fn registry_types(items: &[serde_json::Value]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|v| v["registry_type"].as_str())
        .collect()
}

#[test]
fn setup_detect_empty_dir_returns_empty_json() {
    let dir = tempfile::tempdir().unwrap();
    let (ok, items, stderr) = setup_detect_json(dir.path());
    assert!(ok, "setup detect should succeed; stderr: {stderr}");
    assert!(
        items.is_empty(),
        "empty dir should produce no detections; got: {items:?}"
    );
}

/// `setup ide --json` must emit a JSON array and detect a JetBrains IDE from a
/// `.idea/` directory in the working directory, with a placeholder registry name
/// (the CLI path has no loaded registry list, so nothing is "configured").
#[test]
fn setup_ide_json_detects_idea_project() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".idea")).unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_batlehub-cli"))
        .args(["setup", "ide", "--json"])
        .current_dir(dir.path())
        .env("HOME", dir.path()) // no ~/.config/{Code,VSCodium,JetBrains}
        .env("BATLEHUB_SERVER", "https://batlehub.example.com")
        // Strip editor signals that might leak from the test runner's own env so
        // the only detection is the `.idea/` directory above.
        .env_remove("TERM_PROGRAM")
        .env_remove("VSCODE_PID")
        .env_remove("VSCODE_IPC_HOOK")
        .env_remove("VSCODE_IPC_HOOK_CLI")
        .env_remove("VSCODE_GIT_IPC_HANDLE")
        .env_remove("VSCODE_INJECTION")
        .env_remove("TERMINAL_EMULATOR")
        .env_remove("IDEA_INITIAL_DIRECTORY")
        .env_remove("__INTELLIJ_COMMAND_HISTFILE__")
        .output()
        .expect("failed to run batlehub-cli");

    assert!(
        out.status.success(),
        "setup ide should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let items: Vec<serde_json::Value> =
        serde_json::from_slice(&out.stdout).expect("stdout should be a JSON array");
    let jb = items
        .iter()
        .find(|v| v["registry_type"] == "jetbrains-marketplace")
        .unwrap_or_else(|| panic!("expected a jetbrains-marketplace entry; got: {items:?}"));
    assert_eq!(jb["registry_configured"], serde_json::json!(false));
}

/// `setup detect` asks the server which registries exist, so the snippet names
/// the real one instead of `<registry>`.
#[test]
fn setup_detect_names_the_configured_registry() {
    let server = TestServer::start_with(&[("npm1", "npm")]);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{"name":"my-app"}"#).unwrap();

    let stdout = setup_detect_resolved(&server.base_url(), dir.path(), &[]);

    assert!(
        stdout.contains(&format!("registry={}/proxy/npm1/npm/", server.base_url())),
        "expected the configured registry name in the snippet; got:\n{stdout}"
    );
    // Path-routed: everything answers on the one host.
    assert!(
        stdout.contains("machine 127.0.0.1"),
        "expected a .netrc stanza for the server host; got:\n{stdout}"
    );
}

/// A host-routed registry (RFC 0001) is advertised on its own subdomain. The
/// snippet must point there *without* `/proxy/{name}` — that host adds the
/// prefix itself — and the `.netrc` stanza must name that host, not the API's.
#[test]
fn setup_detect_uses_the_registry_subdomain_and_netrc_host() {
    let server = TestServer::start_with_hosts(
        &[("npm1", "npm")],
        &[("npm1", "https://npm1.batlehub.example.com")],
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{"name":"my-app"}"#).unwrap();

    let stdout = setup_detect_resolved(&server.base_url(), dir.path(), &[]);

    assert!(
        stdout.contains("registry=https://npm1.batlehub.example.com/npm/"),
        "expected the registry's own host in the snippet; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("/proxy/npm1"),
        "the registry host prefixes /proxy/{{name}} itself; got:\n{stdout}"
    );
    assert!(
        stdout.contains("machine npm1.batlehub.example.com"),
        "expected a .netrc stanza for the registry host; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("machine 127.0.0.1"),
        "credentials go to the host the client talks to, not the API host; got:\n{stdout}"
    );
}

/// The JSON output carries the resolved registry and base URL.
#[test]
fn setup_detect_json_reports_the_resolved_registry() {
    let server = TestServer::start_with_hosts(
        &[("cargo1", "cargo")],
        &[("cargo1", "https://cargo1.batlehub.example.com")],
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"c\"\n").unwrap();

    let stdout = setup_detect_resolved(&server.base_url(), dir.path(), &["--json"]);

    let items: Vec<serde_json::Value> =
        serde_json::from_str(&stdout).expect("stdout should be a JSON array");
    assert_eq!(items[0]["registry_name"], serde_json::json!("cargo1"));
    assert_eq!(
        items[0]["base_url"],
        serde_json::json!("https://cargo1.batlehub.example.com")
    );
}

/// `--offline` keeps the placeholders and must not touch the network at all,
/// even when a reachable server is configured.
#[test]
fn setup_detect_offline_keeps_the_placeholder() {
    let server = TestServer::start_with(&[("npm1", "npm")]);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{"name":"my-app"}"#).unwrap();

    let (ok, stdout, stderr) = setup_detect(&server.base_url(), dir.path(), &["--offline"]);

    assert!(ok, "setup detect failed: {stdout}");
    assert!(
        stdout.contains("/proxy/<registry>/npm/"),
        "expected the placeholder; got:\n{stdout}"
    );
    // The placeholder came from `--offline`, not from a fetch that failed or
    // timed out — without this the test passes for the wrong reason on exactly
    // the loaded machine where the distinction matters.
    assert!(
        !stderr.contains(PLACEHOLDER_NOTE),
        "--offline must not contact the server at all; stderr: {stderr}"
    );
}

#[test]
fn setup_detect_cargo_toml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let (ok, items, stderr) = setup_detect_json(dir.path());
    assert!(ok, "setup detect failed; stderr: {stderr}");
    let types = registry_types(&items);
    assert_eq!(types, ["cargo"], "expected cargo; got {types:?}");
    assert_eq!(
        items[0]["package_name"].as_str(),
        Some("my-crate"),
        "wrong package name"
    );
}

#[test]
fn setup_detect_go_mod() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("go.mod"),
        "module github.com/example/myapp\n\ngo 1.21\n",
    )
    .unwrap();
    let (ok, items, stderr) = setup_detect_json(dir.path());
    assert!(ok, "setup detect failed; stderr: {stderr}");
    let types = registry_types(&items);
    assert_eq!(types, ["gomodules"], "expected gomodules; got {types:?}");
    assert_eq!(items[0]["package_name"].as_str(), Some("myapp"));
}

#[test]
fn setup_detect_package_json() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"name":"my-frontend","version":"1.0.0"}"#,
    )
    .unwrap();
    let (ok, items, stderr) = setup_detect_json(dir.path());
    assert!(ok, "setup detect failed; stderr: {stderr}");
    let types = registry_types(&items);
    assert_eq!(types, ["npm"], "expected npm; got {types:?}");
    assert_eq!(items[0]["package_name"].as_str(), Some("my-frontend"));
}

#[test]
fn setup_detect_pom_xml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("pom.xml"),
        "<project><artifactId>my-library</artifactId></project>",
    )
    .unwrap();
    let (ok, items, stderr) = setup_detect_json(dir.path());
    assert!(ok, "setup detect failed; stderr: {stderr}");
    let types = registry_types(&items);
    assert_eq!(types, ["maven"], "expected maven; got {types:?}");
    assert_eq!(items[0]["package_name"].as_str(), Some("my-library"));
}

#[test]
fn setup_detect_nuspec() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("MyLib.nuspec"), "<package/>").unwrap();
    let (ok, items, stderr) = setup_detect_json(dir.path());
    assert!(ok, "setup detect failed; stderr: {stderr}");
    let types = registry_types(&items);
    assert_eq!(types, ["nuget"], "expected nuget; got {types:?}");
    assert_eq!(items[0]["package_name"].as_str(), Some("MyLib"));
}

#[test]
fn setup_detect_terraform() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.tf"), "provider \"aws\" {}").unwrap();
    let (ok, items, stderr) = setup_detect_json(dir.path());
    assert!(ok, "setup detect failed; stderr: {stderr}");
    let types = registry_types(&items);
    assert_eq!(types, ["terraform"], "expected terraform; got {types:?}");
}

#[test]
fn setup_detect_conda_environment_yml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("environment.yml"),
        "name: data-science-env\ndependencies:\n  - numpy\n  - pandas\n",
    )
    .unwrap();
    let (ok, items, stderr) = setup_detect_json(dir.path());
    assert!(ok, "setup detect failed; stderr: {stderr}");
    let types = registry_types(&items);
    assert_eq!(types, ["conda"], "expected conda; got {types:?}");
    assert_eq!(items[0]["package_name"].as_str(), Some("data-science-env"));
}

#[test]
fn setup_detect_multiple_manifests() {
    // A monorepo-style directory with both Rust and Node projects.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"backend\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{"name":"frontend"}"#).unwrap();
    let (ok, items, stderr) = setup_detect_json(dir.path());
    assert!(ok, "setup detect failed; stderr: {stderr}");
    let mut types = registry_types(&items);
    types.sort_unstable();
    assert_eq!(types, ["cargo", "npm"], "expected cargo+npm; got {types:?}");
    let cargo = items
        .iter()
        .find(|v| v["registry_type"] == "cargo")
        .unwrap();
    let npm = items.iter().find(|v| v["registry_type"] == "npm").unwrap();
    assert_eq!(cargo["package_name"].as_str(), Some("backend"));
    assert_eq!(npm["package_name"].as_str(), Some("frontend"));
}

#[test]
fn setup_detect_human_readable_output_contains_instructions() {
    // Without --json the output should contain human-readable setup instructions.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"my-crate\"\n",
    )
    .unwrap();
    let (ok, stdout, stderr) = cli_cmd(
        &["setup", "detect", "--dir", dir.path().to_str().unwrap()],
        "http://127.0.0.1:1",
        "",
    );
    assert!(ok, "setup detect (human) failed; stderr: {stderr}");
    assert!(
        stdout.contains("cargo"),
        "expected 'cargo' in output; got: {stdout}"
    );
    assert!(
        stdout.contains("cargo publish"),
        "expected publish instructions; got: {stdout}"
    );
    assert!(
        stdout.contains("my-crate"),
        "expected package name in output; got: {stdout}"
    );
}

#[test]
fn setup_detect_no_manifests_human_readable() {
    // Empty dir with human-readable output should list supported manifest types.
    let dir = tempfile::tempdir().unwrap();
    let (ok, stdout, stderr) = cli_cmd(
        &["setup", "detect", "--dir", dir.path().to_str().unwrap()],
        "http://127.0.0.1:1",
        "",
    );
    assert!(ok, "setup detect on empty dir failed; stderr: {stderr}");
    assert!(
        stdout.contains("No known project manifests"),
        "expected 'no manifests' message; got: {stdout}"
    );
    assert!(
        stdout.contains("Cargo.toml"),
        "expected manifest list in output"
    );
}

// ── Tests: setup detect — subfolder scanning ──────────────────────────────────

/// depth=0 must NOT find a manifest that lives only in a subdirectory.
#[test]
fn setup_detect_depth0_ignores_subfolders() {
    let root = tempfile::tempdir().unwrap();
    let sub = root.path().join("crate-a");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("Cargo.toml"), "[package]\nname = \"crate-a\"\n").unwrap();

    let (ok, items, stderr) = setup_detect_json(root.path()); // depth=0
    assert!(ok, "setup detect failed; stderr: {stderr}");
    assert!(
        items.is_empty(),
        "depth=0 should not find subfolder manifests; got: {items:?}"
    );
}

/// depth=1 finds a manifest one level deep and reports the correct relative path.
#[test]
fn setup_detect_depth1_finds_immediate_subdir() {
    let root = tempfile::tempdir().unwrap();
    let sub = root.path().join("crate-a");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("Cargo.toml"), "[package]\nname = \"crate-a\"\n").unwrap();

    let (ok, items, stderr) = setup_detect_json_depth(root.path(), 1);
    assert!(ok, "setup detect failed; stderr: {stderr}");
    let types = registry_types(&items);
    assert_eq!(types, ["cargo"], "expected cargo; got {types:?}");
    assert_eq!(items[0]["package_name"].as_str(), Some("crate-a"));
    assert_eq!(
        items[0]["relative_path"].as_str(),
        Some("crate-a"),
        "relative_path should be the subdirectory name"
    );
}

/// depth=1 does NOT look two levels deep.
#[test]
fn setup_detect_depth1_ignores_deeply_nested() {
    let root = tempfile::tempdir().unwrap();
    let deep = root.path().join("packages").join("core");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("Cargo.toml"), "[package]\nname = \"core\"\n").unwrap();

    let (ok, items, stderr) = setup_detect_json_depth(root.path(), 1);
    assert!(ok, "setup detect failed; stderr: {stderr}");
    assert!(
        items.is_empty(),
        "depth=1 should not reach two levels deep; got: {items:?}"
    );
}

/// depth=2 finds a manifest two levels deep with the correct relative path.
#[test]
fn setup_detect_depth2_finds_nested_subdir() {
    let root = tempfile::tempdir().unwrap();
    let deep = root.path().join("packages").join("core");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("Cargo.toml"), "[package]\nname = \"core\"\n").unwrap();

    let (ok, items, stderr) = setup_detect_json_depth(root.path(), 2);
    assert!(ok, "setup detect failed; stderr: {stderr}");
    let types = registry_types(&items);
    assert_eq!(types, ["cargo"], "expected cargo; got {types:?}");
    assert_eq!(items[0]["package_name"].as_str(), Some("core"));
    assert_eq!(
        items[0]["relative_path"].as_str(),
        Some("packages/core"),
        "relative_path should include both directory components"
    );
}

/// Root manifest + subfolder manifest both appear; each has the correct
/// relative_path (empty for root, subdirectory name for the child).
#[test]
fn setup_detect_root_and_subdir_both_detected() {
    let root = tempfile::tempdir().unwrap();
    // Root: a Node project
    std::fs::write(root.path().join("package.json"), r#"{"name":"root-app"}"#).unwrap();
    // Subfolder: a Rust crate
    let sub = root.path().join("server");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("Cargo.toml"), "[package]\nname = \"server\"\n").unwrap();

    let (ok, items, stderr) = setup_detect_json_depth(root.path(), 1);
    assert!(ok, "setup detect failed; stderr: {stderr}");
    assert_eq!(items.len(), 2, "expected 2 detections; got: {items:?}");

    let npm = items.iter().find(|v| v["registry_type"] == "npm").unwrap();
    let cargo = items
        .iter()
        .find(|v| v["registry_type"] == "cargo")
        .unwrap();

    assert_eq!(npm["package_name"].as_str(), Some("root-app"));
    assert_eq!(
        npm["relative_path"].as_str(),
        Some(""),
        "root entry must have empty relative_path"
    );

    assert_eq!(cargo["package_name"].as_str(), Some("server"));
    assert_eq!(cargo["relative_path"].as_str(), Some("server"));
}

/// Workspace-style monorepo: multiple crates at depth 1, each with its own name.
#[test]
fn setup_detect_monorepo_multiple_crates() {
    let root = tempfile::tempdir().unwrap();
    for (subdir, name) in [("api", "my-api"), ("cli", "my-cli"), ("lib", "my-lib")] {
        let path = root.path().join(subdir);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\n"),
        )
        .unwrap();
    }

    let (ok, items, stderr) = setup_detect_json_depth(root.path(), 1);
    assert!(ok, "setup detect failed; stderr: {stderr}");
    assert_eq!(items.len(), 3, "expected 3 crates; got: {items:?}");

    let mut names: Vec<&str> = items
        .iter()
        .filter_map(|v| v["package_name"].as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["my-api", "my-cli", "my-lib"]);

    // All relative paths must be non-empty and match the subdirectory.
    for item in &items {
        let rp = item["relative_path"].as_str().unwrap_or("");
        assert!(
            !rp.is_empty(),
            "relative_path must not be empty for subdir crates; item: {item}"
        );
    }
}

/// hidden directories (`.git`, `.github`) and well-known skip dirs
/// (`node_modules`, `target`) are never entered.
#[test]
fn setup_detect_skips_hidden_and_ignored_dirs() {
    let root = tempfile::tempdir().unwrap();
    for skip in [".git", "node_modules", "target", ".github"] {
        let path = root.path().join(skip);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("Cargo.toml"),
            "[package]\nname = \"should-not-appear\"\n",
        )
        .unwrap();
    }

    let (ok, items, stderr) = setup_detect_json_depth(root.path(), 1);
    assert!(ok, "setup detect failed; stderr: {stderr}");
    assert!(
        items.is_empty(),
        "hidden/ignored dirs must not be scanned; got: {items:?}"
    );
}

/// Mixed language monorepo: Rust at root + Go in one subdir + npm in another.
#[test]
fn setup_detect_mixed_language_monorepo() {
    let root = tempfile::tempdir().unwrap();
    // Root: Rust workspace stub
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"root\"\n",
    )
    .unwrap();
    // Go service
    let go_dir = root.path().join("gateway");
    std::fs::create_dir(&go_dir).unwrap();
    std::fs::write(
        go_dir.join("go.mod"),
        "module github.com/example/gateway\ngo 1.22\n",
    )
    .unwrap();
    // Frontend
    let ui_dir = root.path().join("ui");
    std::fs::create_dir(&ui_dir).unwrap();
    std::fs::write(ui_dir.join("package.json"), r#"{"name":"web-ui"}"#).unwrap();

    let (ok, items, stderr) = setup_detect_json_depth(root.path(), 1);
    assert!(ok, "setup detect failed; stderr: {stderr}");
    assert_eq!(
        items.len(),
        3,
        "expected cargo+gomodules+npm; got: {items:?}"
    );

    let mut types = registry_types(&items);
    types.sort_unstable();
    assert_eq!(types, ["cargo", "gomodules", "npm"]);

    let go = items
        .iter()
        .find(|v| v["registry_type"] == "gomodules")
        .unwrap();
    assert_eq!(go["package_name"].as_str(), Some("gateway"));
    assert_eq!(go["relative_path"].as_str(), Some("gateway"));

    let ui = items.iter().find(|v| v["registry_type"] == "npm").unwrap();
    assert_eq!(ui["package_name"].as_str(), Some("web-ui"));
    assert_eq!(ui["relative_path"].as_str(), Some("ui"));
}

/// Verify that the NuGet package uploaded via CLI is accessible via the proxy
/// flat-index endpoint (the local registry's own query path, not the admin list).
#[test]
fn publish_nuget_package_accessible_via_proxy() {
    let srv = TestServer::start();
    http_publish_nuget(&srv.base_url(), REGISTRY, "PubLib", "4.0.0");

    // The NuGet flat index returns the list of available versions.
    let resp = reqwest::blocking::Client::new()
        .get(format!(
            "{}/proxy/{REGISTRY}/nuget/v3/flat/publib/index.json",
            srv.base_url()
        ))
        .header("Authorization", format!("Bearer {AUTH_TOKEN}"))
        .send()
        .expect("GET flat index");
    assert!(
        resp.status().is_success(),
        "flat index returned {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().expect("valid JSON from flat index");
    let versions = body["versions"].as_array().expect("versions array");
    assert!(
        versions.iter().any(|v| v == "4.0.0"),
        "expected 4.0.0 in flat index versions: {versions:?}"
    );
}

// ── `package readme` (RFC 0007 §6.6) ──────────────────────────────────────────

/// The source, not a rendering: markdown in a terminal is readable, and turning
/// it into ANSI is a separate concern.
#[test]
fn package_readme_prints_the_source_for_an_explicit_version() {
    let srv = TestServer::start();
    srv.seed_readme("mylib", "1.0.0", "# mylib\n\nDoes a thing.");
    srv.seed_readme("mylib", "2.0.0", "# mylib 2\n\nDoes another thing.");

    let (ok, stdout, _stderr) = cli_cmd(
        &["package", "readme", &format!("{REGISTRY}/mylib@1.0.0")],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "package readme should succeed");
    assert!(stdout.contains("# mylib"), "{stdout}");
    assert!(stdout.contains("Does a thing."), "{stdout}");
    // Each version's own text, which is the whole point of the version key.
    assert!(!stdout.contains("Does another thing."), "{stdout}");
}

/// Without a version, the newest that has a README answers.
#[test]
fn package_readme_without_a_version_prints_one_anyway() {
    let srv = TestServer::start();
    srv.seed_readme("mylib", "1.0.0", "# the only one");

    let (ok, stdout, _stderr) = cli_cmd(
        &["package", "readme", &format!("{REGISTRY}/mylib")],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "package readme should succeed");
    assert!(stdout.contains("# the only one"), "{stdout}");
}

/// The qualifications go to stderr, so `batlehub package readme x/y > README.md`
/// writes the document and not a header — while a reader at a terminal still
/// sees that they are looking at another version's prose.
#[test]
fn package_readme_puts_the_fallback_note_on_stderr_and_the_text_on_stdout() {
    let srv = TestServer::start();
    srv.seed_readme("mylib", "1.4.2", "# 1.4.2 documentation");

    let (ok, stdout, stderr) = cli_cmd(
        &["package", "readme", &format!("{REGISTRY}/mylib@2.0.0-rc1")],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "package readme should succeed");
    assert!(stdout.contains("# 1.4.2 documentation"), "{stdout}");
    assert!(
        !stdout.contains("note:"),
        "the note leaked into stdout: {stdout}"
    );
    assert!(stderr.contains("1.4.2"), "{stderr}");
    assert!(stderr.contains("2.0.0-rc1"), "{stderr}");
}

/// An unknown coordinate exits non-zero with something a person can act on.
#[test]
fn package_readme_for_an_unknown_package_fails_readably() {
    let srv = TestServer::start();

    let (ok, _stdout, stderr) = cli_cmd(
        &["package", "readme", &format!("{REGISTRY}/no-such-package")],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(!ok, "an unknown package should exit non-zero");
    assert!(
        stderr.to_lowercase().contains("readme") || stderr.contains("404"),
        "the message should say what was not found: {stderr}"
    );
}

/// A malformed coordinate is rejected before any request is made.
#[test]
fn package_readme_rejects_a_coordinate_with_no_registry() {
    let srv = TestServer::start();

    let (ok, _stdout, stderr) = cli_cmd(
        &["package", "readme", "just-a-name"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(!ok);
    assert!(stderr.contains("registry/name"), "{stderr}");
}

/// `--json` prints the whole response, so a script can read `is_fallback` and
/// `stored` rather than parsing the notes.
#[test]
fn package_readme_json_carries_the_qualifiers() {
    let srv = TestServer::start();
    srv.seed_readme("mylib", "1.0.0", "# mylib");

    let (ok, stdout, _stderr) = cli_cmd(
        &[
            "package",
            "readme",
            &format!("{REGISTRY}/mylib@1.0.0"),
            "--json",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(
        ok,
        "package readme --json should succeed; stdout={stdout} stderr={_stderr}"
    );
    let body: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(body["version"], "1.0.0");
    assert_eq!(body["is_fallback"], false);
    assert_eq!(body["stored"], true);
    assert_eq!(body["source_text"], "# mylib");
}

/// `--no-upstream` maps to `?upstream=skip`. This harness has no upstream at
/// all, so the assertion is that the flag is accepted and still answers from
/// what is held — the "makes no upstream call" half is asserted on a mock in
/// `crates/web/tests/explore_upstream_detail.rs`, which can count requests.
#[test]
fn package_readme_no_upstream_still_answers_from_what_is_held() {
    let srv = TestServer::start();
    srv.seed_readme("mylib", "1.0.0", "# held here");

    let (ok, stdout, _stderr) = cli_cmd(
        &[
            "package",
            "readme",
            &format!("{REGISTRY}/mylib@1.0.0"),
            "--no-upstream",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "package readme --no-upstream should succeed");
    assert!(stdout.contains("# held here"), "{stdout}");
}

// ── Tests: authz (RFC 0015 §4.8) ─────────────────────────────────────────────

/// `authz explain` answers, and the answer carries provenance.
///
/// §4.8: *"`granted_by` is the point. A resolved set without provenance tells an
/// operator what they have; naming the tier and the subject form that produced
/// each verb tells them which line to edit."* A `--json` shape without it would
/// be a permission dump rather than a diagnostic.
#[test]
fn authz_explain_json_carries_provenance() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(
        &[
            "authz",
            "explain",
            REGISTRY,
            "--subject",
            "role:user",
            "--action",
            "releases:read",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "authz explain should succeed: {stderr}");
    assert!(
        stdout.contains("ALLOW") || stdout.contains("DENY"),
        "the verdict must lead: {stdout}"
    );
    assert!(
        stdout.contains("Not covered by this answer"),
        "a bare verdict is ambiguous between 'nothing denies this' and 'nothing I looked at \
         denies this': {stdout}"
    );
    assert!(
        stdout.contains("Tiers walked"),
        "a tier missing from the list reads as 'not considered': {stdout}"
    );
}

/// The `--json` form is the same data, for scripting.
#[test]
fn authz_explain_json() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(
        &[
            "authz",
            "explain",
            REGISTRY,
            "--subject",
            "role:user",
            "--action",
            "releases:read",
            "--json",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "authz explain --json should succeed: {stderr}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(v["decision"].is_string());
    assert!(v["tiers_walked"].is_array());
    assert!(
        v["attributes"]["visibility"].is_string(),
        "§4.8 puts the attributes beside the verbs: {v}"
    );
}

/// A subject the resolver cannot answer about is an error, not a confident
/// verdict.
///
/// `token:` is the case §4.3 names: no principal is a machine token yet, so
/// answering would mean inventing a caller. A `deny` here would be a diagnostic
/// stating something false about a subject that matches nobody.
#[test]
fn authz_explain_refuses_a_subject_it_cannot_answer_about() {
    let srv = TestServer::start();
    let (ok, _stdout, stderr) = cli_cmd(
        &[
            "authz",
            "explain",
            REGISTRY,
            "--subject",
            "token:release-bot",
            "--action",
            "releases:read",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(!ok, "a token subject must not produce a confident verdict");
    assert!(
        stderr.contains("token") || stderr.contains("400"),
        "{stderr}"
    );
}

/// `authz shadow` distinguishes "no shadow" from "a quiet shadow".
///
/// The two look identical in an empty list and mean opposite things: the first
/// says enforcing is safe, the second says nothing was measured.
#[test]
fn authz_shadow_says_when_nothing_is_shadowed() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(&["authz", "shadow"], &srv.base_url(), AUTH_TOKEN);
    assert!(ok, "authz shadow should succeed: {stderr}");
    assert!(
        stdout.contains("No node is in shadow"),
        "an empty list must not read as 'the shadow found nothing': {stdout}"
    );
}

// ── RFC 0017 — `admin grants list|set|rm` ────────────────────────────────────
//
// Phase 3 shipped the CLI with no test of its own: the service beneath it is
// covered in `crates/core`, the routes in `crates/web/tests/grants_editor.rs`,
// and the three subcommands in between by nothing. What lives only here is the
// CLI's own logic — the `name@version` split, the two output modes, and the
// distinction between "removed" and "there was nothing to remove", which the
// exit status does not carry because neither is an error.

#[test]
fn admin_grants_set_list_and_remove() {
    let srv = TestServer::start();
    let base = srv.base_url();

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "grants", "list", REGISTRY, "pkg-a"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "grants list should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("No grants written"),
        "an empty node reports itself rather than printing an empty table: {stdout}"
    );

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "admin",
            "grants",
            "set",
            REGISTRY,
            "pkg-a",
            "--subject",
            "group:oidc1:eng",
            "--actions",
            "releases:read,releases:list",
        ],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "grants set should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("releases:read") && stdout.contains("releases:list"),
        "set reports what was stored: {stdout}"
    );

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "grants", "list", REGISTRY, "pkg-a", "--json"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "grants list --json should succeed; stderr: {stderr}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(doc["grants"][0]["subject"], "group:oidc1:eng");
    assert_eq!(doc["grants"][0]["node_kind"], "package");
    assert_eq!(doc["grants"][0]["node_key"], "pkg-a");

    let (ok, stdout, stderr) = cli_cmd(
        &["admin", "grants", "list", REGISTRY, "pkg-a"],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "grants list (table) should succeed; stderr: {stderr}");
    assert!(
        stdout.contains("group:oidc1:eng") && stdout.contains("package:pkg-a"),
        "the table names the node and the subject: {stdout}"
    );

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "admin",
            "grants",
            "rm",
            REGISTRY,
            "pkg-a",
            "--subject",
            "group:oidc1:eng",
        ],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "grants rm should succeed; stderr: {stderr}");
    assert!(stdout.contains("Removed"), "stdout: {stdout}");

    // Removing what is not there is **not** an error: the operator asked for a
    // state and got it. The two outcomes differ in the message alone, which is
    // why the message is asserted rather than the exit status.
    let (ok, stdout, stderr) = cli_cmd(
        &[
            "admin",
            "grants",
            "rm",
            REGISTRY,
            "pkg-a",
            "--subject",
            "group:oidc1:eng",
        ],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "a second rm is not a failure; stderr: {stderr}");
    assert!(
        stdout.contains("had no grant"),
        "the second rm says so rather than repeating 'Removed': {stdout}"
    );
}

/// `releases:*` names one verb and stores several (§4.2 — expansion at write),
/// and the CLI prints what was stored. An operator who cannot see the
/// difference cannot review the grant they just wrote.
#[test]
fn admin_grants_set_reports_the_expanded_wildcard() {
    let srv = TestServer::start();
    let (ok, stdout, stderr) = cli_cmd(
        &[
            "admin",
            "grants",
            "set",
            REGISTRY,
            "pkg-b",
            "--subject",
            "user:alice",
            "--actions",
            "releases:*",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "grants set should succeed; stderr: {stderr}");
    assert!(
        stdout.matches("releases:").count() > 1,
        "the wildcard expanded at write and the CLI must print the expansion, not \
         the wildcard: {stdout}"
    );
}

/// The `name@version` split is the CLI's own, and it splits on the **last**
/// `@` so a scoped npm name is not read as a version node.
#[test]
fn admin_grants_splits_a_scoped_name_on_the_last_at() {
    let srv = TestServer::start();
    let base = srv.base_url();

    let (ok, _stdout, stderr) = cli_cmd(
        &[
            "admin",
            "grants",
            "set",
            REGISTRY,
            "@acme/billing",
            "--subject",
            "user:alice",
            "--actions",
            "releases:read",
        ],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "a scoped name is a package node; stderr: {stderr}");

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "admin",
            "grants",
            "list",
            REGISTRY,
            "@acme/billing",
            "--json",
        ],
        &base,
        AUTH_TOKEN,
    );
    assert!(ok, "grants list should succeed; stderr: {stderr}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(
        doc["grants"][0]["node_kind"], "package",
        "splitting on the first `@` would have made this a version node on a \
         package named '': {stdout}"
    );
    assert_eq!(doc["grants"][0]["node_key"], "@acme/billing");
}

/// An unparseable subject is refused by the service, and the CLI surfaces the
/// refusal as a non-zero exit rather than printing a success line.
#[test]
fn admin_grants_set_rejects_an_unparseable_subject() {
    let srv = TestServer::start();
    let (ok, stdout, _stderr) = cli_cmd(
        &[
            "admin",
            "grants",
            "set",
            REGISTRY,
            "pkg-c",
            "--subject",
            "not-a-subject-form",
            "--actions",
            "releases:read",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(!ok, "an unparseable subject must fail; stdout: {stdout}");
}

// ── Tests: RFC 0008, the air gap ─────────────────────────────────────────────
//
// `mise plan` turns a lock into a bill of materials; `mise export` turns the
// plan into a signed bundle; `mise import` carries it in. The first is
// offline apart from asking the server which registries exist; the last two
// go through the running server, which is what makes this file the right
// place for them.

const AIR_GAP_LOCK: &str = r#"
[[tools."aqua:EmbarkStudios/cargo-deny"]]
version = "0.18.2"
backend = "aqua:EmbarkStudios/cargo-deny"

[tools."aqua:EmbarkStudios/cargo-deny"."platforms.linux-x64"]
checksum = "sha256:4f0c000000000000000000000000000000000000000000000000000000000000"
url = "https://github.com/EmbarkStudios/cargo-deny/releases/download/0.18.2/cargo-deny-0.18.2-x86_64-unknown-linux-musl.tar.gz"
url_api = "https://api.github.com/repos/EmbarkStudios/cargo-deny/releases/assets/471598214"

[[tools."asdf:mise-plugins/mise-postgres"]]
version = "16.2"
backend = "asdf:mise-plugins/mise-postgres"

[[tools.sonar]]
version = "5.0"
[tools.sonar."platforms.linux-x64"]
url = "https://binaries.sonarsource.com/Distribution/sonar-scanner/sonar-scanner-5.0.zip"
"#;

fn write_lock(dir: &std::path::Path) -> String {
    let path = dir.join("mise.lock");
    std::fs::write(&path, AIR_GAP_LOCK).unwrap();
    path.to_str().unwrap().to_owned()
}

#[test]
fn mise_plan_names_the_downloads_the_unsupported_and_the_unmirrored() {
    let srv = TestServer::start_with(&[("gh", "github")]);
    let dir = tempfile::TempDir::new().unwrap();
    let lock = write_lock(dir.path());

    let (ok, stdout, stderr) = cli_cmd(
        &["mise", "plan", "--lock", &lock, "--platform", "linux-x64"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "mise plan should succeed: {stderr}");
    let plan: serde_json::Value = serde_json::from_str(&stdout).expect("a plan is JSON");
    assert_eq!(plan["plan_version"], 1);
    assert_eq!(plan["platforms"][0], "linux-x64");

    let entries = plan["entries"].as_array().unwrap();
    // The download URL and the API address of the same asset, plus the
    // sonar zip nothing mirrors.
    let deny: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|e| e["tool"].as_str().unwrap().contains("cargo-deny"))
        .collect();
    assert_eq!(deny.len(), 2, "both addresses are planned: {stdout}");
    assert!(deny.iter().all(|e| e["registry"]["name"] == "gh"));
    assert!(
        deny.iter()
            .any(|e| e["proxy_path"]
                == "/proxy/gh/EmbarkStudios/cargo-deny/releases/assets/471598214")
    );

    // The git-fetched backend is named, with the reason, before a bundle
    // exists rather than at install time on a disconnected workstation.
    let unsupported = plan["unsupported"].as_array().unwrap();
    assert_eq!(unsupported.len(), 1, "{stdout}");
    assert!(unsupported[0]["tool"]
        .as_str()
        .unwrap()
        .contains("mise-postgres"));

    // "Is my rewrite table complete?" — the question that has no answer today.
    assert_eq!(plan["unmirrored_hosts"][0], "binaries.sonarsource.com");
}

/// RFC 0008 §13 decision 2: a bundle carries the binary that reads the next
/// plan, so upgrading mise never means crossing the gap by hand.
#[test]
fn mise_plan_can_carry_mise_itself() {
    let srv = TestServer::start_with(&[("gh", "github")]);
    let dir = tempfile::TempDir::new().unwrap();
    let lock = write_lock(dir.path());

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "mise",
            "plan",
            "--lock",
            &lock,
            "--platform",
            "linux-x64",
            "--include-mise",
            "--mise-version",
            "2026.8.6",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "{stderr}");
    let plan: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let mise = plan["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["tool"] == "github:jdx/mise")
        .unwrap_or_else(|| panic!("the plan should carry mise: {stdout}"));
    assert_eq!(mise["version"], "2026.8.6");
    assert!(mise["url"]
        .as_str()
        .unwrap()
        .contains("mise-v2026.8.6-linux-x64"));
}

/// A plan built against a server with no matching registry still plans: the
/// lock is the bill of materials, and every host is reported as unmirrored,
/// which is the honest answer rather than an empty file.
#[test]
fn mise_plan_against_a_server_with_no_matching_registry_reports_every_host() {
    let srv = TestServer::start();
    let dir = tempfile::TempDir::new().unwrap();
    let lock = write_lock(dir.path());

    let (ok, stdout, stderr) = cli_cmd(
        &["mise", "plan", "--lock", &lock, "--platform", "all"],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "{stderr}");
    let plan: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let hosts: Vec<&str> = plan["unmirrored_hosts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h.as_str().unwrap())
        .collect();
    assert!(hosts.contains(&"github.com"), "{stdout}");
    assert!(hosts.contains(&"binaries.sonarsource.com"), "{stdout}");
    assert!(plan["entries"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["key"].is_null()));
}

/// Export → import, through the server both ways.
///
/// The plan is hand-written rather than derived from a lock, because what is
/// under test is the bundle: a published artifact is fetched back through the
/// proxy path, signed into a bundle, and carried in by an instance that
/// trusts the signing key. The lock path is covered above.
#[test]
fn a_bundle_round_trips_from_export_to_import() {
    let srv = TestServer::start();
    let tmp = tempfile::tempdir().unwrap();

    // Something to carry.
    let nupkg = tmp.path().join("Carried.1.0.0.nupkg");
    std::fs::write(&nupkg, make_nupkg("Carried", "1.0.0")).unwrap();
    let (ok, _, stderr) = cli_cmd(
        &["publish", nupkg.to_str().unwrap(), "--registry", REGISTRY],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "publish should succeed: {stderr}");

    let plan = serde_json::json!({
        "plan_version": 1,
        "generated_from": { "file": "mise.lock", "sha256": "0".repeat(64) },
        "platforms": ["linux-x64"],
        "entries": [{
            "tool": "nuget:Carried",
            "version": "1.0.0",
            "platform": "linux-x64",
            "url": "https://example.invalid/Carried.1.0.0.nupkg",
            "registry": { "name": REGISTRY, "type": "nuget" },
            "key": format!("{REGISTRY}/carried/1.0.0/carried.1.0.0.nupkg"),
            "proxy_path": format!("/proxy/{REGISTRY}/nuget/v3/flat/carried/1.0.0/carried.1.0.0.nupkg"),
        }],
        "unsupported": [],
        "unmirrored_hosts": [],
    });
    let plan_path = tmp.path().join("plan.json");
    std::fs::write(&plan_path, serde_json::to_string(&plan).unwrap()).unwrap();

    // A signing key is a file, never a flag: a key on a command line is a key
    // in the shell history and in every process listing on the machine.
    let key_path = tmp.path().join("estate.key");
    std::fs::write(&key_path, "0".repeat(63) + "1").unwrap();
    let bundle_path = tmp.path().join("estate.bhub");

    let (ok, stdout, stderr) = cli_cmd(
        &[
            "mise",
            "export",
            "--plan",
            plan_path.to_str().unwrap(),
            "--sign-key",
            key_path.to_str().unwrap(),
            "-o",
            bundle_path.to_str().unwrap(),
            "--bundle-id",
            "round-trip-1",
        ],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    assert!(ok, "export should succeed: {stdout}{stderr}");
    assert!(bundle_path.exists(), "{stdout}");

    // The bundle really carries the artifact, not an empty manifest: the
    // published bytes are in it, found by content rather than by name.
    let bundle = std::fs::read(&bundle_path).unwrap();
    assert!(
        bundle.windows(9).any(|w| w == b"manifest."),
        "the container holds a manifest"
    );
    assert!(
        stdout.contains("1 entr") && stdout.contains("1 blob"),
        "one entry, one blob: {stdout}"
    );

    // The import side, on an instance that trusts no key. This is the first
    // thing an import checks and the reason it is checked *before* a blob is
    // read: an instance whose only content path is unauthenticated is worse
    // than one with no content path.
    let (ok, stdout, stderr) = cli_cmd(
        &["mise", "import", bundle_path.to_str().unwrap()],
        &srv.base_url(),
        AUTH_TOKEN,
    );
    let said = format!("{stdout}{stderr}");
    assert!(
        !ok,
        "a bundle no configured key vouches for is refused: {said}"
    );
    assert!(
        said.contains("bundle_trusted_keys"),
        "and the message names the thing to configure: {said}"
    );
}

// ── Tests: RFC 0011, the credential contract file ────────────────────────────
//
// `auth write-token-file` and `auth status` are how a process that is not the
// CLI learns which credential to present. What is asserted here is the
// consumer's side of that contract through the real binary: the file lands
// where the editor patch looks, one registry's entry does not disturb
// another's, and nothing on the reporting path prints a secret.

/// The contract file the CLI would write, for a run with its own home.
fn contract_home() -> tempfile::TempDir {
    tempfile::TempDir::new().unwrap()
}

fn cli_in_home(
    args: &[&str],
    server: &str,
    token: &str,
    home: &std::path::Path,
) -> (bool, String, String) {
    let config = scratch_home();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_batlehub-cli"))
        .args(args)
        .env("BATLEHUB_SERVER", server)
        .env("BATLEHUB_TOKEN", token)
        .env("BATLEHUB_HOME", home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", config.path())
        // The patch reads this first, and so does the CLI: unset here so the
        // default path under BATLEHUB_HOME is what is exercised.
        .env_remove("VSX_REGISTRY_AUTH_TOKEN_FILE")
        .output()
        .expect("run the CLI");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn write_token_file_puts_the_credential_where_the_editor_patch_looks() {
    let srv = TestServer::start();
    let home = contract_home();

    let (ok, stdout, stderr) = cli_in_home(
        &["auth", "write-token-file"],
        &srv.base_url(),
        "bh_pat_secret-value",
        home.path(),
    );
    assert!(ok, "write-token-file should succeed: {stdout}{stderr}");
    assert!(stdout.contains("written"), "{stdout}");

    // The path is the contract's, not the CLI profile store's: an editor
    // patch that had to know about XDG on Linux and Application Support on
    // macOS would be a second contract.
    let path = home.path().join("state").join("vsx-token.json");
    assert!(path.exists(), "{stdout}");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(doc["version"], 1);

    let origin = srv.base_url();
    let entry = &doc["registries"][origin.trim_end_matches('/')];
    assert_eq!(entry["token"], "bh_pat_secret-value");
    assert_eq!(
        entry["kind"], "pat",
        "the bh_pat_ prefix is the server's own dispatch rule"
    );
    assert_eq!(
        entry["refresh"]["source"], "none",
        "a PAT has nothing to redeem"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the file holds a credential");
    }
}

/// A laptop pointed at three Batlehubs keeps three credentials in one file,
/// and a login to one is not a logout from the others.
#[test]
fn writing_one_registry_leaves_every_other_entry_alone() {
    let srv = TestServer::start();
    let home = contract_home();
    let path = home.path().join("state").join("vsx-token.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{
          "version": 1,
          "aFieldFromTheFuture": 42,
          "registries": {
            "https://hub.elsewhere.dev": {
              "token": "someone-elses",
              "kind": "oidc",
              "unknownEntryField": true
            }
          }
        }"#,
    )
    .unwrap();

    let (ok, stdout, stderr) = cli_in_home(
        &["auth", "write-token-file"],
        &srv.base_url(),
        "bh_pat_mine",
        home.path(),
    );
    assert!(ok, "{stdout}{stderr}");

    // Writing the same origin again says it replaced rather than added: the
    // one case where this command is not additive is the one worth naming.
    let (_, second, _) = cli_in_home(
        &["auth", "write-token-file"],
        &srv.base_url(),
        "bh_pat_mine",
        home.path(),
    );
    assert!(second.contains("replaced"), "{second}");

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        doc["registries"]["https://hub.elsewhere.dev"]["token"],
        "someone-elses"
    );
    assert_eq!(
        doc["registries"]["https://hub.elsewhere.dev"]["unknownEntryField"], true,
        "a consumer that dropped what it did not understand would undo the writer that added it"
    );
    assert_eq!(doc["aFieldFromTheFuture"], 42);
    assert_eq!(doc["registries"].as_object().unwrap().len(), 2);
}

/// A credential that stays where it is: the file records a path, and the
/// contract file stops being a place a secret rests.
#[test]
fn a_mounted_token_is_recorded_as_a_path_rather_than_a_value() {
    let srv = TestServer::start();
    let home = contract_home();
    let mounted = home.path().join("sa-token");
    std::fs::write(&mounted, "mounted-secret\n").unwrap();

    let (ok, stdout, stderr) = cli_in_home(
        &[
            "auth",
            "write-token-file",
            "--from-file",
            mounted.to_str().unwrap(),
        ],
        &srv.base_url(),
        "unused",
        home.path(),
    );
    assert!(ok, "{stdout}{stderr}");

    let body = std::fs::read_to_string(home.path().join("state").join("vsx-token.json")).unwrap();
    assert!(
        !body.contains("mounted-secret"),
        "the value must not be copied into the contract file: {body}"
    );
    let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
    let entry = &doc["registries"][srv.base_url().trim_end_matches('/')];
    assert_eq!(entry["token"]["from"], "file");
    assert_eq!(entry["token"]["path"], mounted.to_str().unwrap());
    assert_eq!(
        entry["refresh"]["source"], "reresolve",
        "something else keeps a projected token fresh"
    );

    // And `auth status` resolves it — now, not from a cache — and still
    // does not print it.
    let (ok, stdout, stderr) = cli_in_home(
        &["--json", "auth", "status"],
        &srv.base_url(),
        "unused",
        home.path(),
    );
    assert!(ok, "{stderr}");
    assert!(!stdout.contains("mounted-secret"), "{stdout}");
    let rows: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(rows[0]["state"], "ok");
    assert_eq!(rows[0]["kind"], "kubernetes");

    // The state is a resolution performed now: take the file away and the
    // same command says so, which is the whole value of the command.
    std::fs::remove_file(&mounted).unwrap();
    let (_, stdout, _) = cli_in_home(
        &["--json", "auth", "status"],
        &srv.base_url(),
        "unused",
        home.path(),
    );
    let rows: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(rows[0]["state"], "unset");
    assert!(
        rows[0]["detail"].as_str().unwrap().contains("No such file"),
        "`unset` and a refused endpoint look identical from the editor and want opposite fixes: {}",
        rows[0]["detail"]
    );
}

#[test]
fn auth_token_prints_the_credential_and_status_never_does() {
    let srv = TestServer::start();
    let home = contract_home();

    let (ok, stdout, stderr) = cli_in_home(
        &["auth", "token"],
        &srv.base_url(),
        "bh_pat_printable",
        home.path(),
    );
    assert!(ok, "{stderr}");
    assert_eq!(
        stdout.trim(),
        "bh_pat_printable",
        "`auth token` is the one command whose job is to emit a credential"
    );

    let (ok, stdout, _) = cli_in_home(
        &["auth", "token", "--output", "json"],
        &srv.base_url(),
        "bh_pat_printable",
        home.path(),
    );
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["token"], "bh_pat_printable");
    assert_eq!(v["kind"], "pat");

    // The PAT subcommands still dispatch: adding the bare verb did not take
    // the namespace from them. `list` refuses a static token because listing
    // PATs needs an OIDC session — which is the point, because reaching that
    // refusal means the subcommand ran rather than the credential printer.
    let (ok, stdout, stderr) = cli_in_home(
        &["auth", "token", "list"],
        &srv.base_url(),
        AUTH_TOKEN,
        home.path(),
    );
    let said = format!("{stdout}{stderr}");
    assert!(!ok, "{said}");
    assert!(
        said.contains("only OIDC sessions can list API tokens"),
        "`auth token list` reached the listing endpoint rather than printing a credential: {said}"
    );
    assert!(
        !said.contains(AUTH_TOKEN),
        "and it did not print the credential on its way there: {said}"
    );

    // And the status table renders the same entry with no way to carry it.
    cli_in_home(
        &["auth", "write-token-file"],
        &srv.base_url(),
        "bh_pat_printable",
        home.path(),
    );
    let (ok, stdout, stderr) =
        cli_in_home(&["auth", "status"], &srv.base_url(), "unused", home.path());
    assert!(ok, "{stderr}");
    assert!(
        !stdout.contains("bh_pat_printable"),
        "the status table rendered a credential: {stdout}"
    );
    assert!(stdout.contains("inline"), "{stdout}");
}

#[test]
fn a_contract_file_that_does_not_parse_is_no_credential_rather_than_a_failure() {
    let srv = TestServer::start();
    let home = contract_home();
    let path = home.path().join("state").join("vsx-token.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{ not json at all").unwrap();

    // Reading it is not an error — it must never break an anonymous gallery.
    let (ok, _, stderr) = cli_in_home(&["auth", "status"], &srv.base_url(), "unused", home.path());
    assert!(ok, "status must not fail on a broken file: {stderr}");
    assert!(stderr.contains("not a usable contract file"), "{stderr}");

    // Writing over it *is* refused: overwriting a file we could not read
    // would silently discard another registry's entry.
    let (ok, stdout, stderr) = cli_in_home(
        &["auth", "write-token-file"],
        &srv.base_url(),
        "bh_pat_x",
        home.path(),
    );
    assert!(!ok, "{stdout}{stderr}");
    assert!(
        format!("{stdout}{stderr}").contains("not a usable contract file"),
        "{stdout}{stderr}"
    );
}

#[test]
fn the_three_verbs_agree_on_a_path_other_than_the_default() {
    let srv = TestServer::start();
    let home = contract_home();
    let elsewhere = home.path().join("somewhere-else.json");

    let (ok, stdout, stderr) = cli_in_home(
        &[
            "auth",
            "write-token-file",
            "--path",
            elsewhere.to_str().unwrap(),
        ],
        &srv.base_url(),
        "bh_pat_elsewhere",
        home.path(),
    );
    assert!(ok, "{stdout}{stderr}");
    assert!(elsewhere.exists(), "{stdout}");
    assert!(
        !home.path().join("state").join("vsx-token.json").exists(),
        "--path means --path"
    );

    // `auth status` has to be able to read what `write-token-file` wrote, or
    // the one command that explains the file cannot explain half of them.
    let (ok, stdout, _) = cli_in_home(
        &[
            "--json",
            "auth",
            "status",
            "--path",
            elsewhere.to_str().unwrap(),
        ],
        &srv.base_url(),
        "unused",
        home.path(),
    );
    assert!(ok);
    let rows: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(rows[0]["state"], "ok");
    assert!(!stdout.contains("bh_pat_elsewhere"), "{stdout}");

    // …and the default path is still empty, which the status command says
    // rather than leaving as an empty table.
    let (ok, stdout, _) = cli_in_home(&["auth", "status"], &srv.base_url(), "unused", home.path());
    assert!(ok);
    assert!(stdout.contains("No credentials in"), "{stdout}");
}

/// The one command whose job is to emit a secret has to fail loudly when it
/// has none: a caller substituting `$(… auth token)` into a header would
/// otherwise send an empty bearer and get a 401 it cannot explain.
#[test]
fn auth_token_exits_non_zero_when_there_is_no_credential() {
    let srv = TestServer::start();
    let home = contract_home();
    let config = scratch_home();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_batlehub-cli"))
        .args(["auth", "token"])
        .env("BATLEHUB_SERVER", srv.base_url())
        .env("BATLEHUB_HOME", home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", config.path())
        .env_remove("BATLEHUB_TOKEN")
        .output()
        .expect("run the CLI");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("auth login"),
        "and it says what to do about it"
    );
}

#[test]
fn auth_token_refuses_an_output_format_it_does_not_have() {
    let srv = TestServer::start();
    let home = contract_home();
    let (ok, stdout, stderr) = cli_in_home(
        &["auth", "token", "--output", "yaml"],
        &srv.base_url(),
        "bh_pat_x",
        home.path(),
    );
    let said = format!("{stdout}{stderr}");
    assert!(!ok, "{said}");
    assert!(said.contains("`raw` or `json`"), "{said}");
    assert!(
        !said.contains("bh_pat_x"),
        "and it prints nothing on the way out"
    );
}

/// `--min-ttl` is a knob on the refresh threshold, not a second freshness
/// rule: it is accepted, and with a credential already in hand it changes
/// nothing about what comes out.
#[test]
fn min_ttl_is_accepted_and_does_not_change_a_credential_that_needs_no_refresh() {
    let srv = TestServer::start();
    let home = contract_home();
    for ttl in ["0", "120", "86400"] {
        let (ok, stdout, stderr) = cli_in_home(
            &["auth", "token", "--min-ttl", ttl],
            &srv.base_url(),
            "bh_pat_stable",
            home.path(),
        );
        assert!(ok, "--min-ttl {ttl}: {stdout}{stderr}");
        assert_eq!(stdout.trim(), "bh_pat_stable", "--min-ttl {ttl}");
    }
}

/// A path that yields nothing today is legitimate — a projected token in a
/// pod that has not started yet — so the entry is written and the operator is
/// told, rather than the command refusing and leaving the estate unconfigured.
#[test]
fn from_file_warns_but_still_records_a_path_that_yields_nothing_yet() {
    let srv = TestServer::start();
    let home = contract_home();
    let not_yet = home.path().join("not-mounted-yet");

    let (ok, stdout, stderr) = cli_in_home(
        &[
            "auth",
            "write-token-file",
            "--from-file",
            not_yet.to_str().unwrap(),
        ],
        &srv.base_url(),
        "unused",
        home.path(),
    );
    assert!(ok, "{stdout}{stderr}");
    assert!(
        stderr.contains("yields no credential right now"),
        "the operator is told: {stderr}"
    );

    let (_, stdout, _) = cli_in_home(
        &["--json", "auth", "status"],
        &srv.base_url(),
        "unused",
        home.path(),
    );
    let rows: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(rows[0]["state"], "unset");

    // And when it appears, the same entry resolves — nothing had to be
    // rewritten, which is the point of recording a path rather than a value.
    std::fs::write(&not_yet, "arrived-later\n").unwrap();
    let (_, stdout, _) = cli_in_home(
        &["--json", "auth", "status"],
        &srv.base_url(),
        "unused",
        home.path(),
    );
    let rows: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(rows[0]["state"], "ok");
    assert!(!stdout.contains("arrived-later"), "{stdout}");
}
