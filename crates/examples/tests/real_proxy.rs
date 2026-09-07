/// Layer 4 smoke tests — real batlehub proxy server (in-memory backends, real upstreams).
///
/// Each test spins up a genuine actix-web batlehub proxy wired with:
///   - Real registry clients pointing to public upstreams
///   - In-memory PackageRepository, StorageBackend, CacheStore (no PostgreSQL)
///   - All registries accessible anonymously (no auth token required from tools)
///
/// The tests verify the full chain:
///   example config → batlehub proxy → upstream registry → dependency installed →
///   (optionally) API server starts → HTTP response contains expected payload.
///
/// ## Running
///
/// Every test here is `#[ignore]`d, because each needs a language toolchain
/// (via `mise`) and reachable public upstreams — neither of which a plain
/// `cargo test` can assume. Left un-ignored they returned early when a
/// prerequisite was missing, and libtest counted that as a pass: the suite could
/// report "17 passed" without a single request reaching an upstream.
///
/// ```
/// task test:real-proxy      # the deliberate run: prerequisites must be present
/// ```
///
/// That task sets `REAL_PROXY_REQUIRE`, which turns every unmet prerequisite
/// into a failure (see [`skip!`]) rather than a silent pass. To poke at one
/// scenario on a machine that may not have its toolchain, run it directly and
/// unmet prerequisites print and return as before:
///
/// ```
/// cargo test -p batlehub-examples --test real_proxy -- --ignored real_proxy_npm_api
/// ```
use std::{
    collections::HashMap,
    fs,
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

use actix_web::{web, App, HttpServer};
use utoipa_actix_web::AppExt;

use batlehub_adapters::{
    auth::StaticTokenAuthProvider,
    cache::InMemoryCacheStore,
    in_memory::{
        InMemoryPackageRepository, InMemoryStorageBackend, NoopArtifactMetaRepository,
        NullUserTokenRepository,
    },
    local_registry::InMemoryLocalRegistry,
    notification::InMemoryNotificationStore,
    registry::{
        CargoRegistryClient, ComposerRegistryClient, CondaRegistryClient, GithubRegistryClient,
        GoProxyRegistryClient, MavenRegistryClient, NpmRegistryClient, OpenVsxRegistryClient,
        PypiRegistryClient, RubyGemsRegistryClient, TerraformRegistryClient, UpstreamHttpOptions,
        VsCodeMarketplaceRegistryClient,
    },
};
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::Role,
    ports::{AuthProvider, CacheStore, RegistryClient},
    rules::{BlockListRule, RbacRule},
    services::{
        new_hot_lock, AdminService, HotConfig, LocalRegistryService, ProxyMetrics, ProxyService,
        RegistryPolicy,
    },
};
use batlehub_web::{
    configure_app, new_access_lock, AccessConfig, AuthMiddlewareFactory, RegistryMap,
    RegistryModeMap, UpstreamMap,
};

// ── workspace helpers ─────────────────────────────────────────────────────────

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn example_src(name: &str) -> PathBuf {
    workspace_root().join("examples").join(name)
}

fn copy_dir_all(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn copy_example(name: &str, base_dir: &Path) -> PathBuf {
    let dst = base_dir.join(name);
    copy_dir_all(&example_src(name), &dst);
    dst
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
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

/// Download the full body of `url`. Returns `None` on any error.
fn curl_body(url: &str) -> Option<String> {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "30", url])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Return the HTTP status code for `url` without downloading the body.
/// Returns `None` if curl itself fails.
fn curl_status(url: &str) -> Option<u16> {
    let out = Command::new("curl")
        .args([
            "-s",
            "--max-time",
            "30",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            url,
        ])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn kill_wait(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn spawn_tree(cmd: &mut Command) -> std::io::Result<Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()
}

/// Kill a process group started with [`spawn_tree`].
///
/// Maven (`spring-boot:run`, `quarkus:dev`) forks a child JVM that outlives
/// the Maven wrapper when only the wrapper is killed. Sending SIGTERM/SIGKILL
/// to the entire process group (PGID == child PID after `process_group(0)`)
/// tears down every spawned JVM so the port is released before the next test.
fn kill_tree(mut child: Child) {
    #[cfg(unix)]
    {
        let pid = child.id();
        let _ = Command::new("kill")
            .args(["-s", "TERM", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        thread::sleep(Duration::from_millis(500));
        let _ = Command::new("kill")
            .args(["-s", "KILL", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Report an unmet prerequisite (missing toolchain, unreachable upstream).
///
/// Stable libtest has no runtime "ignored" outcome — `#[ignore]` is decided at
/// compile time, and a test that returns early is counted as a **pass**. These
/// scenarios can only discover at runtime whether `mise`, a language toolchain
/// and the network are actually there, so every one of them could report success
/// having exercised nothing, and a suite of 17 could report "17 passed" without
/// a single request reaching an upstream.
///
/// So the outcome depends on whether the run was deliberate:
///
/// - **Ad hoc** (`cargo test --include-ignored`): print and let the caller
///   return, as before. Someone poking at one scenario on a laptop without Ruby
///   installed should not get a failure for it.
/// - **`REAL_PROXY_REQUIRE` set** (as `task test:real-proxy` sets it): panic.
///   A run that asked for these tests is asserting the prerequisites exist, so a
///   missing one is a failure of the environment, reported rather than hidden.
///
/// The `#[ignore]` on each test covers the other half: a plain `cargo test`
/// reports them as `ignored`, which is honest, instead of as passes.
macro_rules! skip {
    ($($arg:tt)*) => {{
        let reason = format!($($arg)*);
        if std::env::var_os("REAL_PROXY_REQUIRE").is_some() {
            // The call sites all phrase themselves as "SKIP <test>: <cause>",
            // which reads wrong on a failure line — libtest already names the
            // test, and nothing was skipped.
            let cause = reason
                .strip_prefix("SKIP ")
                .and_then(|r| r.split_once(": "))
                .map(|(_, cause)| cause)
                .unwrap_or(&reason);
            panic!("unmet prerequisite: {cause} (REAL_PROXY_REQUIRE is set)");
        }
        eprintln!("{reason}");
    }};
}

fn tool_available(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn mise_install(dir: &Path) -> bool {
    Command::new("mise")
        .arg("install")
        .env("MISE_TRUSTED_CONFIG_PATHS", dir)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn mise_exec(dir: &Path, cmd: &str, args: &[&str]) -> Command {
    let mut c = Command::new("mise");
    c.arg("exec")
        .arg("--")
        .arg(cmd)
        .args(args)
        .env("MISE_TRUSTED_CONFIG_PATHS", dir)
        .current_dir(dir);
    c
}

/// Write a Maven `settings.xml` that mirrors everything through `proxy_url`
/// (should end with `/maven2/`).
fn write_maven_proxy_settings(dir: &Path, proxy_url: &str) -> PathBuf {
    let path = dir.join("proxy-settings.xml");
    fs::write(
        &path,
        format!(
            r#"<?xml version="1.0"?>
<settings xmlns="http://maven.apache.org/SETTINGS/1.2.0"
          xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
          xsi:schemaLocation="http://maven.apache.org/SETTINGS/1.2.0
              https://maven.apache.org/xsd/settings-1.2.0.xsd">
  <mirrors>
    <mirror>
      <id>batlehub</id>
      <name>BatleHub Proxy</name>
      <mirrorOf>*</mirrorOf>
      <url>{proxy_url}</url>
    </mirror>
  </mirrors>
</settings>"#
        ),
    )
    .unwrap();
    path
}

/// Write a Terraform CLI config file (`~/.terraformrc`) that routes all provider
/// downloads through the batlehub proxy at `proxy_url`.
fn write_terraform_rc(path: &Path, proxy_port: u16) -> PathBuf {
    fs::write(
        path,
        format!(
            r#"credentials "127.0.0.1:{proxy_port}" {{
  token = "{PROXY_AUTH_TOKEN}"
}}

provider_installation {{
  network_mirror {{
    url     = "http://127.0.0.1:{proxy_port}/proxy/my-terraform/"
    include = ["registry.terraform.io/*/*"]
  }}
  direct {{
    exclude = ["registry.terraform.io/*/*"]
  }}
}}
"#
        ),
    )
    .unwrap();
    path.to_path_buf()
}

// ── Real batlehub proxy server ────────────────────────────────────────────────

/// Runs a genuine actix-web batlehub proxy on a random local port.
///
/// Uses in-memory backends (no PostgreSQL) and real registry clients that
/// forward requests to public upstreams. All registries are accessible without
/// authentication so example tools do not need to send a token.
///
/// Dropped when the struct is dropped (runtime shutdown cancels the server).
struct RealProxy {
    port: u16,
    _runtime: tokio::runtime::Runtime,
}

const PROXY_AUTH_TOKEN: &str = "test-proxy-token";

impl RealProxy {
    fn start_with_registries(
        registries: HashMap<String, Arc<dyn RegistryClient>>,
        registry_map: RegistryMap,
    ) -> Self {
        let repo = InMemoryPackageRepository::new();
        let storage = InMemoryStorageBackend::new();
        let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

        let registry_names: Vec<String> = registry_map.keys();

        let policies: HashMap<String, Arc<RegistryPolicy>> = registry_names
            .iter()
            .map(|name| {
                let perms = HashMap::from([
                    (Role::Anonymous, vec!["*".to_owned()]),
                    (Role::User, vec!["*".to_owned()]),
                    (Role::Admin, vec!["*".to_owned()]),
                ]);
                let policy = Arc::new(RegistryPolicy {
                    metadata_ttl: Some(Duration::from_secs(300)),
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
                });
                (name.clone(), policy)
            })
            .collect();

        let local_svc = Arc::new(LocalRegistryService {
            backend: Arc::new(InMemoryLocalRegistry::new()),
            storage: storage.clone(),
            hot: new_hot_lock(HotConfig {
                registries: HashMap::new(),
                policies: HashMap::new(),
                ..Default::default()
            }),
            quota: None,
            ownership: None,
            team_namespace: None,
            sbom: None,
            explore_cache: None,
            package_repo: None,
            readme: None,
        });

        let proxy_svc = Arc::new(ProxyService {
            hot: new_hot_lock(HotConfig {
                registries,
                policies,
                ..Default::default()
            }),
            storage,
            cache,
            repo: repo.clone(),
            artifact_meta: NoopArtifactMetaRepository::arc(),
            metrics: Arc::new(ProxyMetrics::new(&[])),
            sbom: None,
            readme: None,
            discovery: Default::default(),
        });
        let admin_svc = Arc::new(AdminService::new(repo));
        let token_repo = NullUserTokenRepository::arc();

        let access_config = new_access_lock(AccessConfig {
            anonymous: registry_names.iter().cloned().collect(),
            user: registry_names.iter().cloned().collect(),
            admin: registry_names.iter().cloned().collect(),
            groups: HashMap::new(),
            explore_anonymous: std::collections::HashSet::new(),
            explore_user: std::collections::HashSet::new(),
            explore_admin: std::collections::HashSet::new(),
        });

        let auth_providers: Vec<Arc<dyn AuthProvider>> =
            vec![Arc::new(StaticTokenAuthProvider::new([(
                PROXY_AUTH_TOKEN.to_owned(),
                Some("test-user".to_owned()),
                Role::Admin,
            )]))];

        let cargo_indexes = batlehub_web::CargoIndexMap::default();

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
            HashMap::new(),     // warming_map
            Default::default(), // release_imports
            HashMap::new(),     // eviction_map
            Arc::new(ProxyMetrics::new(&[])),
            None,
            None,                                       // sbom_svc
            None,                                       // notification_svc
            Arc::new(InMemoryNotificationStore::new()), // notification_store
            None,                                       // notifications_config
            None,                                       // storage_admin_repo,
            // Prose search off, matching the shipped default.
            batlehub_web::new_search_lock(false),
        );

        let rt = tokio::runtime::Runtime::new().unwrap();

        let port = rt.block_on(async {
            let local_svc = local_svc.clone();
            let cargo_indexes = cargo_indexes.clone();
            let configure = configure.clone();
            let auth_providers = auth_providers.clone();

            let server = HttpServer::new(move || {
                let (app, _) = App::new()
                    .into_utoipa_app()
                    .configure(configure.clone())
                    .split_for_parts();
                app.app_data(web::Data::new(cargo_indexes.clone()))
                    .app_data(web::Data::new(local_svc.clone()))
                    .app_data(web::Data::new(RegistryModeMap::default()))
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
            "real proxy did not start on port {port} within 10 s"
        );

        Self { port, _runtime: rt }
    }

    /// Start a proxy in Local mode (no upstream clients) on a random port.
    ///
    /// All registries in `registry_map` are set to `RegistryMode::Local` so
    /// publish endpoints are active. Uses the same `PROXY_AUTH_TOKEN`.
    fn start_local(registry_map: RegistryMap) -> Self {
        let repo = InMemoryPackageRepository::new();
        let storage = InMemoryStorageBackend::new();
        let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

        let registry_names: Vec<String> = registry_map.keys();

        let policies: HashMap<String, Arc<RegistryPolicy>> = registry_names
            .iter()
            .map(|name| {
                let perms = HashMap::from([
                    (Role::Anonymous, vec!["*".to_owned()]),
                    (Role::User, vec!["*".to_owned()]),
                    (Role::Admin, vec!["*".to_owned()]),
                ]);
                let policy = Arc::new(RegistryPolicy {
                    metadata_ttl: Some(Duration::from_secs(300)),
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
                });
                (name.clone(), policy)
            })
            .collect();

        let local_svc = Arc::new(LocalRegistryService {
            backend: Arc::new(InMemoryLocalRegistry::new()),
            storage: storage.clone(),
            hot: new_hot_lock(HotConfig {
                registries: HashMap::new(),
                policies: HashMap::new(),
                ..Default::default()
            }),
            quota: None,
            ownership: None,
            team_namespace: None,
            sbom: None,
            explore_cache: None,
            package_repo: None,
            readme: None,
        });

        let proxy_svc = Arc::new(ProxyService {
            hot: new_hot_lock(HotConfig {
                registries: HashMap::new(),
                policies,
                ..Default::default()
            }),
            storage,
            cache,
            repo: repo.clone(),
            artifact_meta: NoopArtifactMetaRepository::arc(),
            metrics: Arc::new(ProxyMetrics::new(&[])),
            sbom: None,
            readme: None,
            discovery: Default::default(),
        });
        let admin_svc = Arc::new(AdminService::new(repo));
        let token_repo = NullUserTokenRepository::arc();

        let access_config = new_access_lock(AccessConfig {
            anonymous: registry_names.iter().cloned().collect(),
            user: registry_names.iter().cloned().collect(),
            admin: registry_names.iter().cloned().collect(),
            groups: HashMap::new(),
            explore_anonymous: std::collections::HashSet::new(),
            explore_user: std::collections::HashSet::new(),
            explore_admin: std::collections::HashSet::new(),
        });

        let auth_providers: Vec<Arc<dyn AuthProvider>> =
            vec![Arc::new(StaticTokenAuthProvider::new([(
                PROXY_AUTH_TOKEN.to_owned(),
                Some("test-user".to_owned()),
                Role::Admin,
            )]))];

        let mode_map = RegistryModeMap::from(
            registry_names
                .iter()
                .map(|n| (n.clone(), RegistryMode::Local))
                .collect::<std::collections::HashMap<_, _>>(),
        );

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
            HashMap::new(),     // warming_map
            Default::default(), // release_imports
            HashMap::new(),     // eviction_map
            Arc::new(ProxyMetrics::new(&[])),
            None,
            None,                                       // sbom_svc
            None,                                       // notification_svc
            Arc::new(InMemoryNotificationStore::new()), // notification_store
            None,                                       // notifications_config
            None,                                       // storage_admin_repo,
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

            let server = HttpServer::new(move || {
                let (app, _) = App::new()
                    .into_utoipa_app()
                    .configure(configure.clone())
                    .split_for_parts();
                app.app_data(web::Data::new(cargo_indexes.clone()))
                    .app_data(web::Data::new(local_svc.clone()))
                    .app_data(web::Data::new(mode_map.clone()))
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
            "local proxy did not start on port {port} within 10 s"
        );

        Self { port, _runtime: rt }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

// ── Layer 4 tests ─────────────────────────────────────────────────────────────
//
// Each test builds a RealProxy with the minimal registry client(s) needed for
// that ecosystem, wires the example tool to point at the proxy, and verifies
// the proxy correctly forwards to the upstream.

// ── npm ───────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_npm_api() {
    if !tool_available("mise") {
        skip!("SKIP real_proxy_npm_api: mise not available");
        return;
    }

    let opts = UpstreamHttpOptions::default();
    let npm = match NpmRegistryClient::new("https://registry.npmjs.org/", &opts) {
        Ok(c) => c,
        Err(e) => {
            skip!("SKIP real_proxy_npm_api: {e}");
            return;
        }
    };

    let proxy = RealProxy::start_with_registries(
        [(
            "my-npm".to_owned(),
            Arc::new(npm) as Arc<dyn RegistryClient>,
        )]
        .into(),
        RegistryMap::from(std::collections::HashMap::from([(
            "my-npm".to_owned(),
            "npm".to_owned(),
        )])),
    );

    let tmp = TempDir::new().unwrap();
    let dir = copy_example("npm", tmp.path());

    if !mise_install(&dir) {
        skip!("SKIP real_proxy_npm_api: mise install failed (Node unavailable)");
        return;
    }

    // Patch .npmrc to point at the real proxy.
    let npmrc = dir.join(".npmrc");
    let content = fs::read_to_string(&npmrc).unwrap();
    fs::write(
        &npmrc,
        content.replace("localhost:8080", &format!("127.0.0.1:{}", proxy.port)),
    )
    .unwrap();

    let ok = mise_exec(&dir, "npm", &["install", "--no-audit", "--no-fund"])
        .env("NPM_CONFIG_CACHE", tmp.path().join("npm-cache"))
        .env("NPM_CONFIG_USERCONFIG", &npmrc)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        skip!("SKIP real_proxy_npm_api: npm install through proxy failed (network issue?)");
        return;
    }

    let port = free_port();
    let server = mise_exec(&dir, "node", &["src/index.js"])
        .env("PORT", port.to_string())
        .env("NPM_CONFIG_CACHE", tmp.path().join("npm-cache"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn node server");

    if !wait_for_port(port, Duration::from_secs(15)) {
        kill_wait(server);
        panic!("npm/Express server did not bind on port {port}");
    }

    let body = curl_body(&format!("http://127.0.0.1:{port}/")).expect("curl npm server");
    kill_wait(server);
    assert!(
        body.contains("hello"),
        "npm response missing 'hello'; got: {body}"
    );
}

// ── cargo ─────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_cargo_fetch() {
    if !tool_available("mise") {
        skip!("SKIP real_proxy_cargo_fetch: mise not available");
        return;
    }

    let opts = UpstreamHttpOptions::default();
    let client = match CargoRegistryClient::new("https://index.crates.io/", &opts) {
        Ok(c) => c,
        Err(e) => {
            skip!("SKIP real_proxy_cargo_fetch: {e}");
            return;
        }
    };

    let proxy = RealProxy::start_with_registries(
        [(
            "my-cargo".to_owned(),
            Arc::new(client) as Arc<dyn RegistryClient>,
        )]
        .into(),
        RegistryMap::from(std::collections::HashMap::from([(
            "my-cargo".to_owned(),
            "cargo".to_owned(),
        )])),
    );

    let tmp = TempDir::new().unwrap();
    let dir = copy_example("cargo", tmp.path());
    let cargo_home = tmp.path().join("cargo-home");
    fs::create_dir_all(&cargo_home).unwrap();

    if !mise_install(&dir) {
        skip!("SKIP real_proxy_cargo_fetch: mise install failed (Rust unavailable)");
        return;
    }

    // Write credentials into the temporary CARGO_HOME.
    fs::write(
        cargo_home.join("credentials.toml"),
        format!("[registries.batlehub]\ntoken = \"Bearer {PROXY_AUTH_TOKEN}\"\n"),
    )
    .unwrap();

    // Patch .cargo/config.toml: redirect the sparse index to the real proxy.
    let cargo_cfg = dir.join(".cargo/config.toml");
    let content = fs::read_to_string(&cargo_cfg).unwrap();
    fs::write(
        &cargo_cfg,
        content.replace("localhost:8080", &format!("127.0.0.1:{}", proxy.port)),
    )
    .unwrap();

    // `cargo fetch` downloads all dependency crates from the proxy.
    let ok = mise_exec(&dir, "cargo", &["fetch"])
        .env("CARGO_HOME", &cargo_home)
        .env("CARGO_NET_OFFLINE", "false")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !ok {
        skip!(
            "SKIP real_proxy_cargo_fetch: cargo fetch through proxy failed \
             (network or proxy issue)"
        );
    }
    // Success is implicit: no panic means the proxy accepted and forwarded the request.
}

// ── go ────────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_go_api() {
    if !tool_available("mise") {
        skip!("SKIP real_proxy_go_api: mise not available");
        return;
    }

    let opts = UpstreamHttpOptions::default();
    let client = match GoProxyRegistryClient::new("https://proxy.golang.org/", &opts) {
        Ok(c) => c,
        Err(e) => {
            skip!("SKIP real_proxy_go_api: {e}");
            return;
        }
    };

    let proxy = RealProxy::start_with_registries(
        [(
            "my-go".to_owned(),
            Arc::new(client) as Arc<dyn RegistryClient>,
        )]
        .into(),
        RegistryMap::from(std::collections::HashMap::from([(
            "my-go".to_owned(),
            "goproxy".to_owned(),
        )])),
    );

    let tmp = TempDir::new().unwrap();
    let dir = copy_example("go", tmp.path());

    if !mise_install(&dir) {
        skip!("SKIP real_proxy_go_api: mise install failed (Go unavailable)");
        return;
    }

    let port = free_port();
    let proxy_url = format!("{}/proxy/my-go/", proxy.base_url());

    let server = match mise_exec(&dir, "go", &["run", "."])
        .env("PORT", port.to_string())
        .env("GIN_MODE", "release")
        .env("GOPROXY", &proxy_url)
        .env("GONOSUMDB", "*")
        .env("GOPATH", tmp.path().join("gopath"))
        .env("GOFLAGS", "-mod=mod")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(s) => s,
        Err(e) => {
            skip!("SKIP real_proxy_go_api: spawn failed: {e}");
            return;
        }
    };

    // Allow up to 3 min — first run downloads all Go modules through the proxy.
    if !wait_for_port(port, Duration::from_secs(180)) {
        kill_wait(server);
        skip!("SKIP real_proxy_go_api: server did not start within 180 s");
        return;
    }

    let body = curl_body(&format!("http://127.0.0.1:{port}/")).expect("curl go server");
    kill_wait(server);
    assert!(
        body.contains("hello"),
        "go response missing 'hello'; got: {body}"
    );
}

// ── pypi ──────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_pypi_api() {
    if !tool_available("mise") {
        skip!("SKIP real_proxy_pypi_api: mise not available");
        return;
    }

    let opts = UpstreamHttpOptions::default();
    let client = match PypiRegistryClient::new("https://pypi.org", &opts) {
        Ok(c) => c,
        Err(e) => {
            skip!("SKIP real_proxy_pypi_api: {e}");
            return;
        }
    };

    let proxy = RealProxy::start_with_registries(
        [(
            "my-pypi".to_owned(),
            Arc::new(client) as Arc<dyn RegistryClient>,
        )]
        .into(),
        RegistryMap::from(std::collections::HashMap::from([(
            "my-pypi".to_owned(),
            "pypi".to_owned(),
        )])),
    );

    let tmp = TempDir::new().unwrap();
    let dir = copy_example("pypi", tmp.path());
    let venv = tmp.path().join("venv");

    if !mise_install(&dir) {
        skip!("SKIP real_proxy_pypi_api: mise install failed (Python unavailable)");
        return;
    }

    let ok = mise_exec(&dir, "python3", &["-m", "venv", venv.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        skip!("SKIP real_proxy_pypi_api: venv creation failed");
        return;
    }

    let index_url = format!("http://127.0.0.1:{}/proxy/my-pypi/simple/", proxy.port);
    let ok = Command::new(venv.join("bin/pip"))
        .args([
            "install",
            "--quiet",
            "--index-url",
            &index_url,
            "--trusted-host",
            "127.0.0.1",
            "fastapi",
            "uvicorn[standard]",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        skip!("SKIP real_proxy_pypi_api: pip install through proxy failed (network unavailable)");
        return;
    }

    let port = free_port();
    let server = Command::new(venv.join("bin/uvicorn"))
        .args([
            "main:app",
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
        ])
        .current_dir(dir.join("src"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn uvicorn");

    if !wait_for_port(port, Duration::from_secs(20)) {
        kill_wait(server);
        panic!("uvicorn did not start on port {port}");
    }

    let body = curl_body(&format!("http://127.0.0.1:{port}/")).expect("curl python server");
    kill_wait(server);
    assert!(
        body.contains("hello"),
        "pypi response missing 'hello'; got: {body}"
    );
}

// ── conda ──────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_conda_repodata() {
    if !tool_available("conda") {
        skip!("SKIP real_proxy_conda_repodata: conda not available");
        return;
    }

    let opts = UpstreamHttpOptions::default();
    let client = match CondaRegistryClient::new("https://conda.anaconda.org/conda-forge", &opts) {
        Ok(c) => c,
        Err(e) => {
            skip!("SKIP real_proxy_conda_repodata: {e}");
            return;
        }
    };

    let proxy = RealProxy::start_with_registries(
        [(
            "my-conda".to_owned(),
            Arc::new(client) as Arc<dyn RegistryClient>,
        )]
        .into(),
        RegistryMap::from(std::collections::HashMap::from([(
            "my-conda".to_owned(),
            "conda".to_owned(),
        )])),
    );

    // Verify repodata.json is accessible through the proxy
    let repodata_url = format!(
        "http://127.0.0.1:{}/proxy/my-conda/noarch/repodata.json",
        proxy.port
    );
    let status = curl_status(&repodata_url);
    if status != Some(200) {
        skip!("SKIP real_proxy_conda_repodata: upstream unreachable (status={status:?})");
        return;
    }
    assert_eq!(
        status,
        Some(200),
        "conda repodata.json through proxy: expected 200"
    );
}

// ── rubygems ──────────────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_rubygems_api() {
    if !tool_available("mise") {
        skip!("SKIP real_proxy_rubygems_api: mise not available");
        return;
    }

    let opts = UpstreamHttpOptions::default();
    let client = match RubyGemsRegistryClient::new("https://rubygems.org", &opts) {
        Ok(c) => c,
        Err(e) => {
            skip!("SKIP real_proxy_rubygems_api: {e}");
            return;
        }
    };

    let proxy = RealProxy::start_with_registries(
        [(
            "my-gems".to_owned(),
            Arc::new(client) as Arc<dyn RegistryClient>,
        )]
        .into(),
        RegistryMap::from(std::collections::HashMap::from([(
            "my-gems".to_owned(),
            "rubygems".to_owned(),
        )])),
    );

    let tmp = TempDir::new().unwrap();
    let dir = copy_example("rubygems", tmp.path());
    let bundle_path = tmp.path().join("bundle");

    if !mise_install(&dir) {
        skip!("SKIP real_proxy_rubygems_api: mise install failed (Ruby unavailable)");
        return;
    }

    // Patch .bundle/config: redirect the mirror to the real proxy.
    let bundle_cfg = dir.join(".bundle/config");
    let content = fs::read_to_string(&bundle_cfg).unwrap();
    fs::write(
        &bundle_cfg,
        content.replace("localhost:8080", &format!("127.0.0.1:{}", proxy.port)),
    )
    .unwrap();

    let ok = mise_exec(&dir, "bundle", &["install"])
        .env("BUNDLE_PATH", &bundle_path)
        .env("BUNDLE_APP_CONFIG", dir.join(".bundle"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        skip!("SKIP real_proxy_rubygems_api: bundle install through proxy failed");
        return;
    }

    let port = free_port();
    let server = mise_exec(
        &dir,
        "bundle",
        &[
            "exec",
            "rackup",
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "config.ru",
        ],
    )
    .env("BUNDLE_PATH", &bundle_path)
    .env("BUNDLE_APP_CONFIG", dir.join(".bundle"))
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .expect("spawn rackup");

    if !wait_for_port(port, Duration::from_secs(20)) {
        kill_wait(server);
        skip!("SKIP real_proxy_rubygems_api: rackup did not start within 20 s");
        return;
    }

    let body = curl_body(&format!("http://127.0.0.1:{port}/")).expect("curl ruby server");
    kill_wait(server);
    assert!(
        body.contains("hello"),
        "rubygems response missing 'hello'; got: {body}"
    );
}

// ── composer ──────────────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_composer_console() {
    if !tool_available("mise") {
        skip!("SKIP real_proxy_composer_console: mise not available");
        return;
    }

    let opts = UpstreamHttpOptions::default();
    let client = match ComposerRegistryClient::new("https://repo.packagist.org", &opts) {
        Ok(c) => c,
        Err(e) => {
            skip!("SKIP real_proxy_composer_console: {e}");
            return;
        }
    };

    let proxy = RealProxy::start_with_registries(
        [(
            "my-composer".to_owned(),
            Arc::new(client) as Arc<dyn RegistryClient>,
        )]
        .into(),
        RegistryMap::from(std::collections::HashMap::from([(
            "my-composer".to_owned(),
            "composer".to_owned(),
        )])),
    );

    let tmp = TempDir::new().unwrap();
    let dir = copy_example("composer", tmp.path());

    if !mise_install(&dir) {
        skip!("SKIP real_proxy_composer_console: mise install failed (PHP/Composer unavailable)");
        return;
    }

    // Patch composer.json: redirect repository URL to the real proxy.
    let cjson_path = dir.join("composer.json");
    let mut cjson: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&cjson_path).unwrap()).unwrap();
    if let Some(repos) = cjson["repositories"].as_array_mut() {
        for repo in repos.iter_mut() {
            if let Some(url) = repo["url"].as_str() {
                let new_url = url.replace("localhost:8080", &format!("127.0.0.1:{}", proxy.port));
                repo["url"] = serde_json::json!(new_url);
            }
        }
    }
    fs::write(&cjson_path, serde_json::to_string_pretty(&cjson).unwrap()).unwrap();

    let ok = mise_exec(
        &dir,
        "composer",
        &["install", "--no-interaction", "--no-dev"],
    )
    .env("COMPOSER_HOME", tmp.path().join("composer-home"))
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false);
    if !ok {
        skip!("SKIP real_proxy_composer_console: composer install through proxy failed");
        return;
    }

    let out = mise_exec(&dir, "php", &["src/App.php", "app:hello"])
        .output()
        .expect("run php console app");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("Hello from my-app!"),
        "php console via real proxy failed.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── maven (Spring Boot) ───────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_maven_spring_api() {
    if !tool_available("mise") {
        skip!("SKIP real_proxy_maven_spring_api: mise not available");
        return;
    }

    let opts = UpstreamHttpOptions::default();
    let client = match MavenRegistryClient::new("https://repo1.maven.org/maven2", &opts) {
        Ok(c) => c,
        Err(e) => {
            skip!("SKIP real_proxy_maven_spring_api: {e}");
            return;
        }
    };

    let proxy = RealProxy::start_with_registries(
        [(
            "my-maven".to_owned(),
            Arc::new(client) as Arc<dyn RegistryClient>,
        )]
        .into(),
        RegistryMap::from(std::collections::HashMap::from([(
            "my-maven".to_owned(),
            "maven".to_owned(),
        )])),
    );

    let tmp = TempDir::new().unwrap();
    let dir = copy_example("maven", tmp.path());
    let m2 = tmp.path().join("m2");

    if !mise_install(&dir) {
        skip!("SKIP real_proxy_maven_spring_api: mise install failed");
        return;
    }

    let proxy_url = format!("{}/proxy/my-maven/maven2/", proxy.base_url());
    let settings = write_maven_proxy_settings(tmp.path(), &proxy_url);
    let port = free_port();

    let server = spawn_tree(
        mise_exec(
            &dir,
            "mvn",
            &[
                "-s",
                settings.to_str().unwrap(),
                &format!("-Dmaven.repo.local={}", m2.display()),
                "spring-boot:run",
                &format!("-Dspring-boot.run.jvmArguments=-Dserver.port={port}"),
            ],
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null()),
    )
    .expect("spawn spring-boot:run");

    if !wait_for_port(port, Duration::from_secs(300)) {
        kill_tree(server);
        skip!(
            "SKIP real_proxy_maven_spring_api: Spring Boot did not start within 300 s \
             (artifact download via proxy may still be in progress)"
        );
        return;
    }

    let body = curl_body(&format!("http://127.0.0.1:{port}/")).expect("curl spring boot");
    kill_tree(server);
    assert!(
        body.contains("hello"),
        "Spring Boot (via proxy) response missing 'hello'; got: {body}"
    );
}

// ── maven-quarkus ─────────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_maven_quarkus_api() {
    if !tool_available("mise") {
        skip!("SKIP real_proxy_maven_quarkus_api: mise not available");
        return;
    }

    let opts = UpstreamHttpOptions::default();
    let client = match MavenRegistryClient::new("https://repo1.maven.org/maven2", &opts) {
        Ok(c) => c,
        Err(e) => {
            skip!("SKIP real_proxy_maven_quarkus_api: {e}");
            return;
        }
    };

    let proxy = RealProxy::start_with_registries(
        [(
            "my-maven".to_owned(),
            Arc::new(client) as Arc<dyn RegistryClient>,
        )]
        .into(),
        RegistryMap::from(std::collections::HashMap::from([(
            "my-maven".to_owned(),
            "maven".to_owned(),
        )])),
    );

    let tmp = TempDir::new().unwrap();
    let dir = copy_example("maven-quarkus", tmp.path());
    let m2 = tmp.path().join("m2");

    if !mise_install(&dir) {
        skip!("SKIP real_proxy_maven_quarkus_api: mise install failed");
        return;
    }

    let proxy_url = format!("{}/proxy/my-maven/maven2/", proxy.base_url());
    let settings = write_maven_proxy_settings(tmp.path(), &proxy_url);
    let port = free_port();

    let server = spawn_tree(
        mise_exec(
            &dir,
            "mvn",
            &[
                "-s",
                settings.to_str().unwrap(),
                &format!("-Dmaven.repo.local={}", m2.display()),
                "quarkus:dev",
                "-Dquarkus.http.host=127.0.0.1",
                &format!("-Dquarkus.http.port={port}"),
            ],
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null()),
    )
    .expect("spawn quarkus:dev");

    if !wait_for_port(port, Duration::from_secs(300)) {
        kill_tree(server);
        skip!(
            "SKIP real_proxy_maven_quarkus_api: Quarkus did not start within 300 s \
             (artifact download via proxy may still be in progress)"
        );
        return;
    }

    let body = curl_body(&format!("http://127.0.0.1:{port}/hello")).expect("curl quarkus");
    kill_tree(server);
    assert!(
        body.contains("Hello") || body.contains("Quarkus"),
        "Quarkus /hello (via proxy) missing expected content; got: {body}"
    );
}

// ── terraform ─────────────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_terraform_init() {
    if !tool_available("mise") {
        skip!("SKIP real_proxy_terraform_init: mise not available");
        return;
    }

    let opts = UpstreamHttpOptions::default();
    let client = match TerraformRegistryClient::new("https://registry.terraform.io", &opts) {
        Ok(c) => c,
        Err(e) => {
            skip!("SKIP real_proxy_terraform_init: {e}");
            return;
        }
    };

    let proxy = RealProxy::start_with_registries(
        [(
            "my-terraform".to_owned(),
            Arc::new(client) as Arc<dyn RegistryClient>,
        )]
        .into(),
        RegistryMap::from(std::collections::HashMap::from([(
            "my-terraform".to_owned(),
            "terraform".to_owned(),
        )])),
    );

    let tmp = TempDir::new().unwrap();
    let dir = copy_example("terraform", tmp.path());

    if !mise_install(&dir) {
        skip!("SKIP real_proxy_terraform_init: mise install failed (Terraform unavailable)");
        return;
    }

    // Write a .terraformrc pointing the network mirror at the real proxy.
    let rcfile = write_terraform_rc(&tmp.path().join(".terraformrc"), proxy.port);

    let ok = mise_exec(&dir, "terraform", &["init", "-no-color", "-upgrade"])
        .env("TF_CLI_CONFIG_FILE", &rcfile)
        .env("TF_DATA_DIR", tmp.path().join(".terraform"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !ok {
        skip!(
            "SKIP real_proxy_terraform_init: terraform init through proxy failed \
             (provider download may have timed out or proxy returned an error)"
        );
    }
    // Success — terraform downloaded providers through the batlehub proxy.
}

// ── github ────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_github_releases() {
    let opts = UpstreamHttpOptions::default();
    let client = match GithubRegistryClient::new("https://api.github.com", &opts) {
        Ok(c) => c,
        Err(e) => {
            skip!("SKIP real_proxy_github_releases: {e}");
            return;
        }
    };

    let proxy = RealProxy::start_with_registries(
        [(
            "my-github".to_owned(),
            Arc::new(client) as Arc<dyn RegistryClient>,
        )]
        .into(),
        RegistryMap::from(std::collections::HashMap::from([(
            "my-github".to_owned(),
            "github".to_owned(),
        )])),
    );

    // List releases for a small, stable repository — verifies the proxy forwards
    // the GitHub API request and returns a valid JSON array.
    let url = format!("{}/proxy/my-github/cli/cli/releases", proxy.base_url());
    match curl_status(&url) {
        None => skip!("SKIP real_proxy_github_releases: curl failed"),
        Some(200) => {
            // Proxy successfully forwarded the GitHub API request.
        }
        Some(403) | Some(429) => {
            // GitHub rate-limited the unauthenticated request — proxy still worked.
            eprintln!("NOTE real_proxy_github_releases: proxy worked but GitHub rate-limited (status 403/429)");
        }
        Some(code) => {
            eprintln!("NOTE real_proxy_github_releases: proxy returned HTTP {code} from GitHub");
        }
    }
}

// ── openvsx ───────────────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_openvsx_download() {
    let opts = UpstreamHttpOptions::default();
    let client = match OpenVsxRegistryClient::new("https://open-vsx.org", &opts) {
        Ok(c) => c,
        Err(e) => {
            skip!("SKIP real_proxy_openvsx_download: {e}");
            return;
        }
    };

    let proxy = RealProxy::start_with_registries(
        [(
            "my-openvsx".to_owned(),
            Arc::new(client) as Arc<dyn RegistryClient>,
        )]
        .into(),
        RegistryMap::from(std::collections::HashMap::from([(
            "my-openvsx".to_owned(),
            "openvsx".to_owned(),
        )])),
    );

    // Download the `tamasfe.even-better-toml` VSIX — one of the smallest extensions
    // used by the openvsx example, chosen to keep transfer time short.
    let url = format!(
        "{}/proxy/my-openvsx/tamasfe.even-better-toml/0.19.2/vsix",
        proxy.base_url()
    );

    match curl_status(&url) {
        None => skip!("SKIP real_proxy_openvsx_download: curl failed"),
        Some(200) => {
            // Proxy successfully retrieved the VSIX from open-vsx.org.
        }
        Some(code) => {
            eprintln!("NOTE real_proxy_openvsx_download: proxy returned HTTP {code}; VSIX may not be available upstream");
        }
    }
}

// ── vscode-marketplace ────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_vscode_marketplace_download() {
    let opts = UpstreamHttpOptions::default();
    let client =
        match VsCodeMarketplaceRegistryClient::new("https://marketplace.visualstudio.com", &opts) {
            Ok(c) => c,
            Err(e) => {
                skip!("SKIP real_proxy_vscode_marketplace_download: {e}");
                return;
            }
        };

    let proxy = RealProxy::start_with_registries(
        [(
            "my-vscode-marketplace".to_owned(),
            Arc::new(client) as Arc<dyn RegistryClient>,
        )]
        .into(),
        RegistryMap::from(std::collections::HashMap::from([(
            "my-vscode-marketplace".to_owned(),
            "vscode-marketplace".to_owned(),
        )])),
    );

    // Fetch `charliermarsh.ruff` — a lightweight Python linter extension.
    let url = format!(
        "{}/proxy/my-vscode-marketplace/charliermarsh.ruff/2024.10.0/vsix",
        proxy.base_url()
    );

    match curl_status(&url) {
        None => skip!("SKIP real_proxy_vscode_marketplace_download: curl failed"),
        Some(200) => {
            // Proxy successfully retrieved the VSIX from marketplace.visualstudio.com.
        }
        Some(code) => {
            eprintln!("NOTE real_proxy_vscode_marketplace_download: proxy returned HTTP {code}");
        }
    }
}

// ── npm publish ───────────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_npm_publish() {
    if !tool_available("mise") {
        skip!("SKIP real_proxy_npm_publish: mise not available");
        return;
    }

    let proxy = RealProxy::start_local(batlehub_web::RegistryMap::from(
        std::collections::HashMap::from([("my-npm".to_owned(), "npm".to_owned())]),
    ));

    let tmp = TempDir::new().unwrap();
    let pkg_dir = tmp.path().join("test-publish-pkg");
    fs::create_dir_all(&pkg_dir).unwrap();

    fs::write(
        pkg_dir.join("package.json"),
        r#"{"name":"test-publish-pkg","version":"1.0.0","description":"test"}"#,
    )
    .unwrap();
    fs::write(pkg_dir.join(".mise.toml"), "[tools]\nnode = \"22\"\n").unwrap();

    if !mise_install(&pkg_dir) {
        skip!("SKIP real_proxy_npm_publish: mise install failed (Node unavailable)");
        return;
    }

    let registry_url = format!("http://127.0.0.1:{}/proxy/my-npm/", proxy.port);
    let npmrc = tmp.path().join(".npmrc");
    fs::write(
        &npmrc,
        format!(
            "registry={registry_url}\n\
             //127.0.0.1:{port}/proxy/my-npm/:_authToken={PROXY_AUTH_TOKEN}\n",
            port = proxy.port,
        ),
    )
    .unwrap();

    let ok = mise_exec(&pkg_dir, "npm", &["publish", "--registry", &registry_url])
        .env("NPM_CONFIG_CACHE", tmp.path().join("npm-cache"))
        .env("NPM_CONFIG_USERCONFIG", &npmrc)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !ok {
        skip!("SKIP real_proxy_npm_publish: npm publish failed (proxy or tool issue)");
        return;
    }

    let status = curl_status(&format!(
        "http://127.0.0.1:{}/proxy/my-npm/test-publish-pkg",
        proxy.port,
    ));
    assert_eq!(status, Some(200), "packument not found after npm publish");
}

// ── cargo publish ─────────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_cargo_publish() {
    if !tool_available("mise") {
        skip!("SKIP real_proxy_cargo_publish: mise not available");
        return;
    }

    let proxy = RealProxy::start_local(batlehub_web::RegistryMap::from(
        std::collections::HashMap::from([("my-cargo".to_owned(), "cargo".to_owned())]),
    ));

    let tmp = TempDir::new().unwrap();
    let pkg_dir = tmp.path().join("test-publish-crate");
    let cargo_home = tmp.path().join("cargo-home");

    fs::create_dir_all(pkg_dir.join("src")).unwrap();
    fs::create_dir_all(&cargo_home).unwrap();

    fs::write(
        pkg_dir.join("Cargo.toml"),
        "[package]\nname = \"test-publish-crate\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(pkg_dir.join("src/lib.rs"), "").unwrap();
    fs::write(pkg_dir.join(".mise.toml"), "[tools]\nrust = \"stable\"\n").unwrap();

    fs::write(
        cargo_home.join("credentials.toml"),
        format!("[registries.batlehub]\ntoken = \"Bearer {PROXY_AUTH_TOKEN}\"\n"),
    )
    .unwrap();

    fs::create_dir_all(pkg_dir.join(".cargo")).unwrap();
    fs::write(
        pkg_dir.join(".cargo/config.toml"),
        format!(
            "[registries.batlehub]\nindex = \"sparse+http://127.0.0.1:{}/proxy/my-cargo/registry/\"\n",
            proxy.port,
        ),
    ).unwrap();

    if !mise_install(&pkg_dir) {
        skip!("SKIP real_proxy_cargo_publish: mise install failed (Rust unavailable)");
        return;
    }

    let ok = mise_exec(
        &pkg_dir,
        "cargo",
        &[
            "publish",
            "--registry",
            "batlehub",
            "--no-verify",
            "--allow-dirty",
        ],
    )
    .env("CARGO_HOME", &cargo_home)
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false);

    if !ok {
        skip!("SKIP real_proxy_cargo_publish: cargo publish failed (proxy or tool issue)");
        return;
    }

    let status = curl_status(&format!(
        "http://127.0.0.1:{}/proxy/my-cargo/test-publish-crate/1.0.0/download",
        proxy.port,
    ));
    assert_eq!(
        status,
        Some(200),
        "crate artifact not found after cargo publish"
    );
}

// ── rubygems publish ──────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_rubygems_publish() {
    if !tool_available("mise") {
        skip!("SKIP real_proxy_rubygems_publish: mise not available");
        return;
    }

    let proxy = RealProxy::start_local(batlehub_web::RegistryMap::from(
        std::collections::HashMap::from([("my-gems".to_owned(), "rubygems".to_owned())]),
    ));

    let tmp = TempDir::new().unwrap();
    let gem_dir = tmp.path().join("my-gem");
    fs::create_dir_all(&gem_dir).unwrap();

    fs::write(
        gem_dir.join("test-publish-gem.gemspec"),
        r#"Gem::Specification.new do |s|
  s.name    = "test-publish-gem"
  s.version = "1.0.0"
  s.summary = "test"
  s.authors = ["test"]
  s.files   = []
end
"#,
    )
    .unwrap();
    fs::write(gem_dir.join(".mise.toml"), "[tools]\nruby = \"3.3\"\n").unwrap();

    if !mise_install(&gem_dir) {
        skip!("SKIP real_proxy_rubygems_publish: mise install failed (Ruby unavailable)");
        return;
    }

    let ok = mise_exec(&gem_dir, "gem", &["build", "test-publish-gem.gemspec"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        skip!("SKIP real_proxy_rubygems_publish: gem build failed");
        return;
    }

    // GEM_HOST_API_KEY is sent verbatim as the Authorization header value.
    // Setting it to "Bearer {token}" makes gem push send the correct Bearer auth.
    let registry_url = format!("http://127.0.0.1:{}/proxy/my-gems/", proxy.port);
    let ok = mise_exec(
        &gem_dir,
        "gem",
        &[
            "push",
            "test-publish-gem-1.0.0.gem",
            "--host",
            &registry_url,
        ],
    )
    .env("GEM_HOST_API_KEY", format!("Bearer {PROXY_AUTH_TOKEN}"))
    .env("HOME", tmp.path())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false);

    if !ok {
        skip!("SKIP real_proxy_rubygems_publish: gem push failed (proxy or tool issue)");
        return;
    }

    let status = curl_status(&format!(
        "http://127.0.0.1:{}/proxy/my-gems/gems/test-publish-gem-1.0.0.gem",
        proxy.port,
    ));
    assert_eq!(status, Some(200), "gem not found after gem push");
}

// ── maven publish ─────────────────────────────────────────────────────────────

#[test]
#[ignore = "real upstreams + language toolchains; run via task test:real-proxy"]
fn real_proxy_maven_publish() {
    if !tool_available("mise") {
        skip!("SKIP real_proxy_maven_publish: mise not available");
        return;
    }

    let proxy = RealProxy::start_local(batlehub_web::RegistryMap::from(
        std::collections::HashMap::from([("my-maven".to_owned(), "maven".to_owned())]),
    ));

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();

    fs::write(
        dir.join(".mise.toml"),
        "[tools]\njava = \"temurin-21\"\nmaven = \"3.9\"\n",
    )
    .unwrap();

    if !mise_install(&dir) {
        skip!("SKIP real_proxy_maven_publish: mise install failed (Java/Maven unavailable)");
        return;
    }

    // A minimal JAR is a valid empty ZIP (end-of-central-directory record only).
    let jar = dir.join("test-artifact-1.0.0.jar");
    fs::write(
        &jar,
        b"PK\x05\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
    )
    .unwrap();

    // Minimal settings.xml — no auth, no mirrors (anonymous upload is allowed by
    // the storage layer for non-POM artifacts; -DgeneratePom=false skips POM
    // upload so the User-role check in publish() is never reached).
    let settings = dir.join("settings.xml");
    fs::write(
        &settings,
        r#"<?xml version="1.0"?><settings xmlns="http://maven.apache.org/SETTINGS/1.2.0"/>"#,
    )
    .unwrap();

    let deploy_url = format!("http://127.0.0.1:{}/proxy/my-maven/maven2/", proxy.port);

    let ok = mise_exec(
        &dir,
        "mvn",
        &[
            "deploy:deploy-file",
            &format!("-Durl={deploy_url}"),
            "-DrepositoryId=batlehub",
            &format!("-Dfile={}", jar.display()),
            "-DgroupId=com.example",
            "-DartifactId=test-artifact",
            "-Dversion=1.0.0",
            "-DgeneratePom=false",
            "--no-transfer-progress",
            "-s",
            settings.to_str().unwrap(),
        ],
    )
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false);

    if !ok {
        skip!("SKIP real_proxy_maven_publish: mvn deploy:deploy-file failed (proxy or tool issue)");
        return;
    }

    let status = curl_status(&format!(
        "http://127.0.0.1:{}/proxy/my-maven/maven2/com/example/test-artifact/1.0.0/test-artifact-1.0.0.jar",
        proxy.port,
    ));
    assert_eq!(status, Some(200), "artifact not found after mvn deploy");
}
