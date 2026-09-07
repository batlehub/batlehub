/// Local-registry upload/pull cycle tests.
///
/// Each test starts a genuine actix-web batlehub proxy in **Local** mode (no
/// upstream forwarding) using in-memory backends, publishes a minimal package
/// via HTTP, then downloads it back and asserts a 200 response.
///
/// Registries covered: npm, cargo, goproxy, rubygems, composer, maven,
/// openvsx, terraform-module.  GitHub and vscode-marketplace are read-only and
/// have no publish endpoints.
///
/// ## Running
///
/// ```
/// cargo test -p batlehub-examples --test local_registry
/// ```
use std::{
    collections::HashMap,
    fs,
    net::TcpStream,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use base64::Engine as _;
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

// ── Local proxy server ────────────────────────────────────────────────────────

/// Runs a genuine actix-web batlehub proxy in **Local** mode on a random port.
///
/// All registries configured in `registry_map` are set to `RegistryMode::Local`
/// so publish endpoints are accessible and no upstream clients are consulted.
/// All registries are accessible without authentication.
///
/// Dropped when the struct is dropped (runtime shutdown cancels the server).
struct LocalProxy {
    port: u16,
    _runtime: tokio::runtime::Runtime,
}

const AUTH_TOKEN: &str = "test-local-token";

impl LocalProxy {
    fn start(registry_map: RegistryMap) -> Self {
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

        // No upstream registries — local mode only.
        let registries: HashMap<String, Arc<dyn RegistryClient>> = HashMap::new();

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
                AUTH_TOKEN.to_owned(),
                Some("test-user".to_owned()),
                Role::Admin,
            )]))];

        // All registries run in Local mode.
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
            "local proxy did not start on port {port}"
        );

        Self { port, _runtime: rt }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn proxy_url(&self, reg: &str) -> String {
        format!("{}/proxy/{reg}", self.base_url())
    }
}

// ── Utilities ─────────────────────────────────────────────────────────────────

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

/// GET a URL and return the HTTP status code.
fn get_status(url: &str) -> u16 {
    Command::new("curl")
        .args([
            "-s",
            "--max-time",
            "10",
            "-H",
            &format!("Authorization: Bearer {AUTH_TOKEN}"),
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            url,
        ])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

/// GET a URL and return `(status, body)`.
fn get(url: &str) -> (u16, String) {
    let out = Command::new("curl")
        .args([
            "-s",
            "--max-time",
            "10",
            "-H",
            &format!("Authorization: Bearer {AUTH_TOKEN}"),
            "-w",
            "\n%{http_code}",
            url,
        ])
        .output()
        .expect("curl GET");
    let text = String::from_utf8_lossy(&out.stdout);
    split_status(text.as_ref())
}

/// PUT/POST a file and return the HTTP status code.
fn upload_file(method: &str, url: &str, file: &Path, content_type: &str) -> u16 {
    Command::new("curl")
        .args([
            "-s",
            "--max-time",
            "15",
            "-X",
            method,
            "-H",
            &format!("Authorization: Bearer {AUTH_TOKEN}"),
            "-H",
            &format!("Content-Type: {content_type}"),
            "--data-binary",
            &format!("@{}", file.display()),
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            url,
        ])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

fn split_status(text: &str) -> (u16, String) {
    if let Some(pos) = text.rfind('\n') {
        let status: u16 = text[pos + 1..].trim().parse().unwrap_or(0);
        (status, text[..pos].to_owned())
    } else {
        (0, text.to_owned())
    }
}

fn write_tmp(tmp: &TempDir, name: &str, data: &[u8]) -> PathBuf {
    let p = tmp.path().join(name);
    fs::write(&p, data).unwrap();
    p
}

// ── Payload builders ──────────────────────────────────────────────────────────

/// npm publish JSON body with base64 tarball.
fn npm_publish_body(name: &str, version: &str, tarball: &[u8]) -> Vec<u8> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(tarball);
    serde_json::to_vec(&serde_json::json!({
        "name": name,
        "versions": {
            version: { "name": name, "version": version, "dist": {} }
        },
        "_attachments": {
            format!("{name}-{version}.tgz"): { "data": encoded }
        }
    }))
    .unwrap()
}

/// Cargo publish binary wire format: [LE u32 meta][JSON][LE u32 crate][bytes].
fn cargo_publish_body(name: &str, version: &str, crate_bytes: &[u8]) -> Vec<u8> {
    let meta = serde_json::json!({
        "name": name, "vers": version,
        "deps": [], "features": {}, "cksum": "",
        "keywords": [], "categories": [],
        "readme": null, "license": "MIT",
        "repository": null, "homepage": null,
        "documentation": null, "badges": {}, "links": null
    });
    let meta_bytes = serde_json::to_vec(&meta).unwrap();
    let mut body = Vec::new();
    body.extend_from_slice(&(meta_bytes.len() as u32).to_le_bytes());
    body.extend_from_slice(&meta_bytes);
    body.extend_from_slice(&(crate_bytes.len() as u32).to_le_bytes());
    body.extend_from_slice(crate_bytes);
    body
}

/// Minimal Go module ZIP: one entry `{module}@{version}/go.mod`.
fn go_module_zip(module: &str, version: &str) -> Vec<u8> {
    use std::io::Write as _;
    let go_mod = format!("module {module}\n\ngo 1.21\n");
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file(format!("{module}@{version}/go.mod"), opts)
            .unwrap();
        zw.write_all(go_mod.as_bytes()).unwrap();
        zw.finish().unwrap();
    }
    buf.into_inner()
}

/// Minimal `.gem` file: TAR containing a gzip-compressed YAML metadata entry.
fn minimal_gem(name: &str, version: &str) -> Vec<u8> {
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write as _;

    let yaml = format!("name: {name}\nversion:\n  version: '{version}'\nplatform: ruby\n");

    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(yaml.as_bytes()).unwrap();
    let metadata_gz = gz.finish().unwrap();

    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(metadata_gz.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, "metadata.gz", metadata_gz.as_slice())
        .unwrap();
    builder.into_inner().unwrap()
}

/// Minimal Composer ZIP: one `composer.json` at the root.
fn composer_zip(vendor: &str, pkg: &str, version: &str) -> Vec<u8> {
    use std::io::Write as _;
    let json_bytes = serde_json::to_vec(&serde_json::json!({
        "name": format!("{vendor}/{pkg}"),
        "version": version,
        "description": "test"
    }))
    .unwrap();
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file("composer.json", opts).unwrap();
        zw.write_all(&json_bytes).unwrap();
        zw.finish().unwrap();
    }
    buf.into_inner()
}

// ── npm: publish + packument + tarball ───────────────────────────────────────

#[test]
fn local_npm_publish_pull() {
    let proxy = LocalProxy::start(batlehub_web::RegistryMap::from(
        std::collections::HashMap::from([("my-npm".to_owned(), "npm".to_owned())]),
    ));
    let tmp = TempDir::new().unwrap();

    let body = npm_publish_body("test-pkg", "1.0.0", b"fake tarball bytes");
    let body_file = write_tmp(&tmp, "npm-body.json", &body);

    let status = upload_file(
        "PUT",
        &format!("{}/test-pkg", proxy.proxy_url("my-npm")),
        &body_file,
        "application/json",
    );
    assert_eq!(status, 200, "npm publish: expected 200, got {status}");

    let (status, body) = get(&format!("{}/test-pkg", proxy.proxy_url("my-npm")));
    assert_eq!(status, 200, "npm packument: expected 200");
    let packument: serde_json::Value = serde_json::from_str(&body).expect("packument not JSON");
    assert!(
        packument["versions"]["1.0.0"].is_object(),
        "packument missing version 1.0.0: {packument}"
    );

    let dl_status = get_status(&format!(
        "{}/test-pkg/1.0.0/tarball",
        proxy.proxy_url("my-npm")
    ));
    assert_eq!(
        dl_status, 200,
        "npm tarball download: expected 200, got {dl_status}"
    );
}

// ── cargo: publish + download ─────────────────────────────────────────────────

#[test]
fn local_cargo_publish_pull() {
    let proxy = LocalProxy::start(batlehub_web::RegistryMap::from(
        std::collections::HashMap::from([("my-cargo".to_owned(), "cargo".to_owned())]),
    ));
    let tmp = TempDir::new().unwrap();

    let body = cargo_publish_body("test-crate", "1.0.0", b"fake .crate bytes");
    let body_file = write_tmp(&tmp, "cargo-pub.bin", &body);

    let status = upload_file(
        "PUT",
        &format!("{}/api/v1/crates/new", proxy.proxy_url("my-cargo")),
        &body_file,
        "application/octet-stream",
    );
    assert_eq!(status, 200, "cargo publish: expected 200, got {status}");

    let dl_status = get_status(&format!(
        "{}/test-crate/1.0.0/download",
        proxy.proxy_url("my-cargo")
    ));
    assert_eq!(
        dl_status, 200,
        "cargo download: expected 200, got {dl_status}"
    );
}

// ── goproxy: publish + list + mod + zip ──────────────────────────────────────

#[test]
fn local_go_publish_pull() {
    let proxy = LocalProxy::start(batlehub_web::RegistryMap::from(
        std::collections::HashMap::from([("my-go".to_owned(), "goproxy".to_owned())]),
    ));
    let tmp = TempDir::new().unwrap();

    const MODULE: &str = "example.com/testmod";
    const VERSION: &str = "v1.0.0";

    let zip_bytes = go_module_zip(MODULE, VERSION);
    let zip_file = write_tmp(&tmp, "testmod.zip", &zip_bytes);

    // PUT /{module}@v/{version}.zip
    let status = upload_file(
        "PUT",
        &format!("{}/{MODULE}@v/{VERSION}.zip", proxy.proxy_url("my-go")),
        &zip_file,
        "application/zip",
    );
    assert_eq!(status, 200, "go publish: expected 200, got {status}");

    // GET /{module}@v/list → newline-separated version list
    let (list_status, list_body) = get(&format!("{}/{MODULE}@v/list", proxy.proxy_url("my-go")));
    assert_eq!(list_status, 200, "go list: expected 200, got {list_status}");
    assert!(
        list_body.contains(VERSION),
        "go list missing {VERSION}: {list_body}"
    );

    // GET /{module}@v/{version}.mod
    let mod_status = get_status(&format!(
        "{}/{MODULE}@v/{VERSION}.mod",
        proxy.proxy_url("my-go")
    ));
    assert_eq!(
        mod_status, 200,
        "go mod download: expected 200, got {mod_status}"
    );

    // GET /{module}@v/{version}.zip
    let zip_status = get_status(&format!(
        "{}/{MODULE}@v/{VERSION}.zip",
        proxy.proxy_url("my-go")
    ));
    assert_eq!(
        zip_status, 200,
        "go zip download: expected 200, got {zip_status}"
    );
}

// ── rubygems: publish + download ──────────────────────────────────────────────

#[test]
fn local_rubygems_publish_pull() {
    let proxy = LocalProxy::start(batlehub_web::RegistryMap::from(
        std::collections::HashMap::from([("my-gems".to_owned(), "rubygems".to_owned())]),
    ));
    let tmp = TempDir::new().unwrap();

    let gem_bytes = minimal_gem("test-gem", "1.0.0");
    let gem_file = write_tmp(&tmp, "test-gem-1.0.0.gem", &gem_bytes);

    let status = upload_file(
        "POST",
        &format!("{}/api/v1/gems", proxy.proxy_url("my-gems")),
        &gem_file,
        "application/octet-stream",
    );
    assert_eq!(status, 200, "rubygems publish: expected 200, got {status}");

    let dl_status = get_status(&format!(
        "{}/gems/test-gem-1.0.0.gem",
        proxy.proxy_url("my-gems")
    ));
    assert_eq!(
        dl_status, 200,
        "rubygems download: expected 200, got {dl_status}"
    );
}

// ── composer: upload + p2 metadata + dist artifact ───────────────────────────

#[test]
fn local_composer_publish_pull() {
    let proxy = LocalProxy::start(batlehub_web::RegistryMap::from(
        std::collections::HashMap::from([("my-composer".to_owned(), "composer".to_owned())]),
    ));
    let tmp = TempDir::new().unwrap();

    let zip_bytes = composer_zip("myvendor", "mypkg", "1.0.0");
    let zip_file = write_tmp(&tmp, "composer-pkg.zip", &zip_bytes);

    let status = upload_file(
        "POST",
        &format!("{}/api/upload", proxy.proxy_url("my-composer")),
        &zip_file,
        "application/zip",
    );
    assert_eq!(status, 200, "composer upload: expected 200, got {status}");

    // GET p2 metadata: /p2/{vendor}/{pkg}.json
    let (p2_status, p2_body) = get(&format!(
        "{}/p2/myvendor/mypkg.json",
        proxy.proxy_url("my-composer")
    ));
    assert_eq!(
        p2_status, 200,
        "composer p2 metadata: expected 200, got {p2_status}"
    );
    let p2: serde_json::Value = serde_json::from_str(&p2_body).expect("p2 not JSON");
    assert!(
        p2["packages"].is_object() || p2["packages"].is_array(),
        "p2 response missing 'packages': {p2}"
    );

    // GET dist artifact: /dist/{vendor}/{pkg}/{version}
    let dist_status = get_status(&format!(
        "{}/dist/myvendor/mypkg/1.0.0",
        proxy.proxy_url("my-composer")
    ));
    assert_eq!(
        dist_status, 200,
        "composer dist download: expected 200, got {dist_status}"
    );
}

// ── maven: PUT artifact + GET artifact ───────────────────────────────────────

#[test]
fn local_maven_publish_pull() {
    let proxy = LocalProxy::start(batlehub_web::RegistryMap::from(
        std::collections::HashMap::from([("my-maven".to_owned(), "maven".to_owned())]),
    ));
    let tmp = TempDir::new().unwrap();

    let jar_bytes = b"fake jar bytes for test artifact";
    let jar_file = write_tmp(&tmp, "test-artifact-1.0.0.jar", jar_bytes);

    const MAVEN_PATH: &str = "com/example/test-artifact/1.0.0/test-artifact-1.0.0.jar";

    let put_status = upload_file(
        "PUT",
        &format!("{}/maven2/{MAVEN_PATH}", proxy.proxy_url("my-maven")),
        &jar_file,
        "application/octet-stream",
    );
    assert_eq!(put_status, 201, "maven PUT: expected 201, got {put_status}");

    let get_status_code = get_status(&format!(
        "{}/maven2/{MAVEN_PATH}",
        proxy.proxy_url("my-maven")
    ));
    assert_eq!(
        get_status_code, 200,
        "maven GET: expected 200, got {get_status_code}"
    );
}

// ── openvsx: PUT VSIX + GET VSIX ─────────────────────────────────────────────

#[test]
fn local_openvsx_publish_pull() {
    let proxy = LocalProxy::start(batlehub_web::RegistryMap::from(
        std::collections::HashMap::from([("my-openvsx".to_owned(), "openvsx".to_owned())]),
    ));
    let tmp = TempDir::new().unwrap();

    let vsix_bytes = b"fake vsix extension bytes";
    let vsix_file = write_tmp(&tmp, "test.extension-1.0.0.vsix", vsix_bytes);

    // PUT /proxy/{registry}/{publisher}.{name}/{version}/vsix
    let put_status = upload_file(
        "PUT",
        &format!(
            "{}/test.extension/1.0.0/vsix",
            proxy.proxy_url("my-openvsx")
        ),
        &vsix_file,
        "application/octet-stream",
    );
    assert_eq!(
        put_status, 200,
        "openvsx publish: expected 200, got {put_status}"
    );

    let dl_status = get_status(&format!(
        "{}/test.extension/1.0.0/vsix",
        proxy.proxy_url("my-openvsx")
    ));
    assert_eq!(
        dl_status, 200,
        "openvsx download: expected 200, got {dl_status}"
    );
}

// ── terraform module: POST + versions + artifact ─────────────────────────────

#[test]
fn local_terraform_module_publish_pull() {
    let proxy = LocalProxy::start(batlehub_web::RegistryMap::from(
        std::collections::HashMap::from([("my-terraform".to_owned(), "terraform".to_owned())]),
    ));
    let tmp = TempDir::new().unwrap();

    let module_bytes = b"fake terraform module tarball";
    let module_file = write_tmp(&tmp, "module.tar.gz", module_bytes);

    // POST /proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}
    let post_status = upload_file(
        "POST",
        &format!(
            "{}/v1/modules/hashicorp/testmod/aws/1.0.0",
            proxy.proxy_url("my-terraform")
        ),
        &module_file,
        "application/octet-stream",
    );
    assert_eq!(
        post_status, 201,
        "terraform module upload: expected 201, got {post_status}"
    );

    // GET versions list
    let (versions_status, versions_body) = get(&format!(
        "{}/v1/modules/hashicorp/testmod/aws/versions",
        proxy.proxy_url("my-terraform")
    ));
    assert_eq!(
        versions_status, 200,
        "terraform versions: expected 200, got {versions_status}"
    );
    let versions: serde_json::Value =
        serde_json::from_str(&versions_body).expect("versions not JSON");
    assert!(
        versions["modules"].is_array(),
        "terraform versions missing 'modules': {versions}"
    );

    // GET artifact download
    let artifact_status = get_status(&format!(
        "{}/v1/modules/hashicorp/testmod/aws/1.0.0/artifact",
        proxy.proxy_url("my-terraform")
    ));
    assert_eq!(
        artifact_status, 200,
        "terraform artifact: expected 200, got {artifact_status}"
    );
}

// ── PyPI: multipart publish + simple-index + file download ───────────────────

/// Build a minimal wheel ZIP (enough structure for the server to accept it).
fn minimal_wheel(name: &str, version: &str) -> Vec<u8> {
    use std::io::Write as _;

    let dist_name = format!("{}-{}.dist-info", name.replace('-', "_"), version);
    let metadata = format!("Metadata-Version: 2.1\nName: {name}\nVersion: {version}\n");
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file(format!("{dist_name}/METADATA"), opts)
            .unwrap();
        zw.write_all(metadata.as_bytes()).unwrap();
        zw.finish().unwrap();
    }
    buf.into_inner()
}

/// POST a twine-style multipart form upload and return the HTTP status code.
fn pypi_upload(url: &str, name: &str, version: &str, file: &std::path::Path) -> u16 {
    std::process::Command::new("curl")
        .args([
            "-s",
            "--max-time",
            "15",
            "-X",
            "POST",
            "-H",
            &format!("Authorization: Bearer {AUTH_TOKEN}"),
            "-F",
            ":action=file_upload",
            "-F",
            &format!("name={name}"),
            "-F",
            &format!("version={version}"),
            "-F",
            &format!("content=@{}", file.display()),
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            url,
        ])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

#[test]
fn local_pypi_publish_pull() {
    let proxy = LocalProxy::start(batlehub_web::RegistryMap::from(
        std::collections::HashMap::from([("my-pypi".to_owned(), "pypi".to_owned())]),
    ));
    let tmp = TempDir::new().unwrap();

    let wheel_bytes = minimal_wheel("myapp", "1.0.0");
    let wheel_file = write_tmp(&tmp, "myapp-1.0.0-py3-none-any.whl", &wheel_bytes);

    let status = pypi_upload(
        &format!("{}/legacy/", proxy.proxy_url("my-pypi")),
        "myapp",
        "1.0.0",
        &wheel_file,
    );
    assert_eq!(status, 200, "pypi publish: expected 200, got {status}");

    // GET simple index page
    let (simple_status, simple_body) =
        get(&format!("{}/simple/myapp/", proxy.proxy_url("my-pypi")));
    assert_eq!(
        simple_status, 200,
        "pypi simple: expected 200, got {simple_status}"
    );
    assert!(
        simple_body.contains("myapp-1.0.0"),
        "simple index missing 'myapp-1.0.0': {simple_body}"
    );

    // GET package file
    let dl_status = get_status(&format!(
        "{}/packages/myapp-1.0.0-py3-none-any.whl",
        proxy.proxy_url("my-pypi")
    ));
    assert_eq!(
        dl_status, 200,
        "pypi download: expected 200, got {dl_status}"
    );
}

// ── Conda: publish + repodata.json + file download ───────────────────────────

/// Build a minimal `.tar.bz2` conda package containing `info/index.json`.
fn minimal_conda_tar_bz2(name: &str, version: &str, build: &str) -> Vec<u8> {
    use bzip2::write::BzEncoder;
    use bzip2::Compression;
    use std::io::Write as _;

    let index_json = serde_json::json!({
        "name": name,
        "version": version,
        "build": build,
        "build_number": 0,
        "depends": [],
        "subdir": "linux-64"
    });
    let index_bytes = serde_json::to_vec(&index_json).unwrap();

    let mut tar_bytes = Vec::new();
    {
        let mut tar_builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(index_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar_builder
            .append_data(&mut header, "info/index.json", index_bytes.as_slice())
            .unwrap();
        tar_builder.finish().unwrap();
    }

    let mut encoder = BzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&tar_bytes).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn local_conda_publish_pull() {
    let proxy = LocalProxy::start(batlehub_web::RegistryMap::from(
        std::collections::HashMap::from([("my-conda".to_owned(), "conda".to_owned())]),
    ));
    let tmp = TempDir::new().unwrap();

    let pkg_bytes = minimal_conda_tar_bz2("numpy-stub", "1.26.0", "py311h0_0");
    let pkg_file = write_tmp(&tmp, "numpy-stub-1.26.0-py311h0_0.tar.bz2", &pkg_bytes);

    // POST publish
    let post_status = upload_file(
        "POST",
        &format!("{}/linux-64/", proxy.proxy_url("my-conda")),
        &pkg_file,
        "application/octet-stream",
    );
    assert_eq!(
        post_status, 200,
        "conda publish: expected 200, got {post_status}"
    );

    // GET repodata.json and verify our package appears
    let (repodata_status, repodata_body) = get(&format!(
        "{}/linux-64/repodata.json",
        proxy.proxy_url("my-conda")
    ));
    assert_eq!(
        repodata_status, 200,
        "conda repodata: expected 200, got {repodata_status}"
    );
    let repodata: serde_json::Value =
        serde_json::from_str(&repodata_body).expect("repodata not JSON");
    assert!(
        repodata["packages"].is_object(),
        "repodata missing 'packages': {repodata}"
    );
    let packages = repodata["packages"].as_object().unwrap();
    assert!(
        packages.keys().any(|k| k.starts_with("numpy-stub")),
        "conda repodata missing numpy-stub entry: {packages:?}"
    );

    // GET package file
    let dl_status = get_status(&format!(
        "{}/linux-64/numpy-stub-1.26.0-py311h0_0.tar.bz2",
        proxy.proxy_url("my-conda")
    ));
    assert_eq!(
        dl_status, 200,
        "conda download: expected 200, got {dl_status}"
    );
}
