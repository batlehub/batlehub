pub mod access;
pub mod badges;
pub mod error;
pub mod extractors;
pub mod handlers;
pub mod middleware;
pub mod services;
pub mod spa;

pub use access::{
    new_access_lock, new_search_lock, AccessConfig, AccessConfigLock, SearchConfigLock,
};

use std::collections::HashMap;
use std::sync::Arc;

use batlehub_config::schema::RegistryMode;

/// A registry-name-keyed `HashMap` behind `Arc<RwLock<>>`, reused by every
/// lookup table below so hot-reload can swap entries without restarting actix
/// workers. `Clone` shares the same lock (all clones see the same data).
///
/// The six domain types below (`RegistryMap`, `RegistryModeMap`, `RepoSignerMap`,
/// `UpstreamMap`, `VulnDbMap`, `CargoIndexMap`) each wrap one of these plus only
/// their own domain-specific accessor methods (`type_of`, `upstream_for`, …) —
/// no downstream call site needs to change, since those six public type names
/// and methods stay the same; only their formerly-duplicated lock/clone
/// boilerplate moves here.
#[derive(Clone)]
struct LockedMap<V>(Arc<std::sync::RwLock<HashMap<String, V>>>);

impl<V: Clone> LockedMap<V> {
    fn new(map: HashMap<String, V>) -> Self {
        Self(Arc::new(std::sync::RwLock::new(map)))
    }

    fn get(&self, key: &str) -> Option<V> {
        self.0
            .read()
            .expect("locked map lock poisoned")
            .get(key)
            .cloned()
    }

    fn contains(&self, key: &str) -> bool {
        self.0
            .read()
            .expect("locked map lock poisoned")
            .contains_key(key)
    }

    fn keys(&self) -> Vec<String> {
        self.0
            .read()
            .expect("locked map lock poisoned")
            .keys()
            .cloned()
            .collect()
    }

    /// Emptiness without materialising the keys — this runs on the request hot
    /// path (see `RegistryHostMap::is_empty`), where cloning every key just to
    /// throw the `Vec` away costs one allocation per configured host per request.
    fn is_empty(&self) -> bool {
        self.0.read().expect("locked map lock poisoned").is_empty()
    }

    fn insert(&self, key: String, value: V) {
        self.0
            .write()
            .expect("locked map lock poisoned")
            .insert(key, value);
    }

    /// A cloned snapshot of every `(key, value)` pair.
    fn entries(&self) -> Vec<(String, V)> {
        self.0
            .read()
            .expect("locked map lock poisoned")
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Replace this map's contents with a snapshot of `other`'s, under both
    /// locks in turn. Used by the hot-reload applier to swap in a pending
    /// map's contents without replacing the `Arc` (and therefore without
    /// invalidating any clone already held by an in-flight request).
    ///
    /// This map's `replace_from` call is independent of every other hot-reload
    /// map's — see `ConfigReloadApplier::apply`'s doc comment (`services/reload/
    /// applier.rs`) for the resulting request-scoped skew window and what to do
    /// if a handler ever needs two of these maps to agree within one request.
    fn replace_from(&self, other: &Self) {
        let snapshot = other.0.read().expect("locked map lock poisoned").clone();
        *self.0.write().expect("locked map lock poisoned") = snapshot;
    }
}

impl<V: Clone> Default for LockedMap<V> {
    fn default() -> Self {
        Self::new(HashMap::new())
    }
}

impl<V: Clone> From<HashMap<String, V>> for LockedMap<V> {
    fn from(map: HashMap<String, V>) -> Self {
        Self::new(map)
    }
}

/// Maps registry name → registry type (e.g. `"github1"` → `"github"`).
#[derive(Clone, Default)]
pub struct RegistryMap(LockedMap<String>);

impl RegistryMap {
    pub fn new(map: HashMap<String, String>) -> Self {
        Self(LockedMap::new(map))
    }

    pub fn type_of(&self, name: &str) -> Option<String> {
        self.0.get(name)
    }

    pub fn is_type(&self, name: &str, expected: &str) -> bool {
        self.type_of(name).as_deref() == Some(expected)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }

    pub fn keys(&self) -> Vec<String> {
        self.0.keys()
    }

    /// A cloned snapshot of every `(registry name, registry type)` pair.
    pub fn entries(&self) -> Vec<(String, String)> {
        self.0.entries()
    }

    /// Replace this map's contents with `other`'s (called by the hot-reload applier).
    pub fn replace_from(&self, other: &Self) {
        self.0.replace_from(&other.0);
    }
}

impl From<HashMap<String, String>> for RegistryMap {
    fn from(map: HashMap<String, String>) -> Self {
        Self::new(map)
    }
}

/// Maps registry name → configured `RegistryMode` (proxy / local / hybrid).
#[derive(Clone, Default)]
pub struct RegistryModeMap(LockedMap<RegistryMode>);

impl RegistryModeMap {
    pub fn new(map: HashMap<String, RegistryMode>) -> Self {
        Self(LockedMap::new(map))
    }

    pub fn get(&self, name: &str) -> RegistryMode {
        self.0.get(name).unwrap_or_default()
    }

    pub fn insert(&self, name: String, mode: RegistryMode) {
        self.0.insert(name, mode);
    }

    /// Replace this map's contents with `other`'s (called by the hot-reload applier).
    pub fn replace_from(&self, other: &Self) {
        self.0.replace_from(&other.0);
    }
}

impl From<HashMap<String, RegistryMode>> for RegistryModeMap {
    fn from(map: HashMap<String, RegistryMode>) -> Self {
        Self::new(map)
    }
}

/// Maps a `deb`/`rpm` registry name → its repository-metadata signing key, when
/// configured. Registries absent from the map host **unsigned** repositories
/// (clients must use `[trusted=yes]` / `gpgcheck=0`).
#[derive(Clone, Default)]
pub struct RepoSignerMap(LockedMap<Arc<batlehub_adapters::repo::OpenPgpSigner>>);

impl RepoSignerMap {
    pub fn get(&self, name: &str) -> Option<Arc<batlehub_adapters::repo::OpenPgpSigner>> {
        self.0.get(name)
    }

    /// Replace this map's contents with `other`'s (called by the hot-reload applier).
    pub fn replace_from(&self, other: &Self) {
        self.0.replace_from(&other.0);
    }
}

impl From<HashMap<String, Arc<batlehub_adapters::repo::OpenPgpSigner>>> for RepoSignerMap {
    fn from(map: HashMap<String, Arc<batlehub_adapters::repo::OpenPgpSigner>>) -> Self {
        Self(LockedMap::new(map))
    }
}

/// Maps npm/terraform/pypi/conda registry name → first upstream base URL (for audit pass-through).
#[derive(Clone, Default)]
pub struct UpstreamMap(LockedMap<String>);

impl UpstreamMap {
    pub fn new(map: HashMap<String, String>) -> Self {
        Self(LockedMap::new(map))
    }

    pub fn upstream_for(&self, name: &str) -> Option<String> {
        self.0.get(name)
    }

    /// Replace this map's contents with `other`'s (called by the hot-reload applier).
    pub fn replace_from(&self, other: &Self) {
        self.0.replace_from(&other.0);
    }
}

impl From<HashMap<String, String>> for UpstreamMap {
    fn from(map: HashMap<String, String>) -> Self {
        Self::new(map)
    }
}

/// Maps a `goproxy` registry name → the base URL of its upstream Go Vulnerability
/// Database (default `https://vuln.go.dev`). Registries absent from the map have
/// the vuln DB passthrough disabled (`vuln_db_url = ""`).
///
/// Holds a shared `reqwest::Client` so all registries reuse one connection pool.
#[derive(Clone)]
pub struct VulnDbMap {
    pub http: reqwest::Client,
    urls: LockedMap<String>,
}

impl VulnDbMap {
    pub fn new(urls: HashMap<String, String>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("batlehub/0.1")
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .expect("building vuln DB HTTP client");
        Self {
            http,
            urls: LockedMap::new(urls),
        }
    }

    pub fn url_for(&self, registry: &str) -> Option<String> {
        self.urls.get(registry)
    }

    /// Replace the URL map in place (called by the hot-reload applier).
    pub fn update(&self, urls: HashMap<String, String>) {
        *self.urls.0.write().expect("locked map lock poisoned") = urls;
    }

    /// Replace this map's contents with `other`'s (called by the hot-reload applier).
    pub fn replace_from(&self, other: &Self) {
        self.urls.replace_from(&other.urls);
    }
}

/// Per-registry Go checksum database base URLs (RFC 0009 §7.4).
///
/// Same absence-means-disabled contract as [`VulnDbMap`]: a registry missing
/// from this map answers `404` on `/sumdb/{path}` rather than proxying a lookup
/// that would leak private module paths to a public log.
#[derive(Clone, Default)]
pub struct SumDbMap {
    urls: LockedMap<String>,
}

impl SumDbMap {
    pub fn new(urls: HashMap<String, String>) -> Self {
        Self {
            urls: LockedMap::new(urls),
        }
    }

    pub fn url_for(&self, registry: &str) -> Option<String> {
        self.urls.get(registry)
    }

    pub fn replace_from(&self, other: &Self) {
        self.urls.replace_from(&other.urls);
    }
}

impl Default for VulnDbMap {
    fn default() -> Self {
        Self::new(HashMap::new())
    }
}

/// Host-based registry routing tables (RFC 0001).
///
/// Three registry-scoped maps that always change together, materialised at
/// config-load time so a request-time lookup is one hash lookup with no suffix
/// parsing and no "does this registry exist" check:
///
/// * `by_host` — the routing table the middleware consults;
/// * `public` — the *preferred* public URL per registry (first explicit host,
///   else the wildcard host, prefixed with the configured scheme). This is what
///   the API and UI advertise, and what `registry_public_base` returns for a
///   host-only registry reached from somewhere else;
/// * `host_only` — registries whose `/proxy/{name}/…` ingress is switched off.
///
/// Empty by default, which is the whole feature turned off: the middleware
/// passes every request through untouched.
#[derive(Clone, Default)]
pub struct RegistryHostMap {
    /// `"npm.acme.io"` | `"npm1.hub.example.com"` → `"npm1"`. Keys are normalised
    /// (see `batlehub_config::schema::normalise_host`).
    by_host: LockedMap<String>,
    /// `"npm1"` → `"https://npm.acme.io"`.
    public: LockedMap<String>,
    /// `"npm1"` → `true` when `path_routing = false`.
    host_only: LockedMap<bool>,
}

impl RegistryHostMap {
    /// Build from resolved config bindings. `bindings` is in preference order
    /// (explicit hosts first), so a same-registry duplicate keeps the explicit
    /// entry. Cross-registry conflicts are already rejected by
    /// `AppConfig::validate`.
    pub fn new(
        by_host: HashMap<String, String>,
        public: HashMap<String, String>,
        host_only: HashMap<String, bool>,
    ) -> Self {
        Self {
            by_host: LockedMap::new(by_host),
            public: LockedMap::new(public),
            host_only: LockedMap::new(host_only),
        }
    }

    /// The registry bound to `normalised_host`, if any. The caller must have
    /// normalised the host already.
    pub fn registry_for(&self, normalised_host: &str) -> Option<String> {
        self.by_host.get(normalised_host)
    }

    /// The advertised public URL of `registry` (scheme included, no trailing
    /// slash), when it has a host.
    pub fn public_url_for(&self, registry: &str) -> Option<String> {
        self.public.get(registry)
    }

    /// Whether `registry` has opted out of `/proxy/{name}/…` (RFC 0001 §4.6).
    pub fn is_host_only(&self, registry: &str) -> bool {
        self.host_only.get(registry).unwrap_or(false)
    }

    /// True when no host is bound at all — the feature is off and the middleware
    /// can skip every lookup.
    pub fn is_empty(&self) -> bool {
        self.by_host.is_empty()
    }

    /// Materialise the tables from a validated `AppConfig`.
    ///
    /// Assumes `AppConfig::validate` has already run: cross-registry host
    /// conflicts are rejected there, so the first binding for a host wins here
    /// rather than being detected as an error.
    pub fn from_app_config(config: &batlehub_config::schema::AppConfig) -> Self {
        let mut by_host = HashMap::new();
        for binding in config.registry_host_bindings() {
            by_host.entry(binding.host).or_insert(binding.registry);
        }
        Self::new(
            by_host,
            config.registry_public_urls().into_iter().collect(),
            config
                .host_only_registries()
                .into_iter()
                .map(|name| (name, true))
                .collect(),
        )
    }

    /// Replace this map's contents with `other`'s (called by the hot-reload applier).
    ///
    /// The three inner maps swap one after another, like every other hot-reload
    /// map — see `LockedMap::replace_from`.
    pub fn replace_from(&self, other: &Self) {
        self.by_host.replace_from(&other.by_host);
        self.public.replace_from(&other.public);
        self.host_only.replace_from(&other.host_only);
    }
}

/// Maps Cargo registry name → [`CargoIndexProxy`] (sparse-index HTTP client + URL).
#[derive(Clone, Default)]
pub struct CargoIndexMap(LockedMap<CargoIndexProxy>);

impl CargoIndexMap {
    pub fn new(map: HashMap<String, CargoIndexProxy>) -> Self {
        Self(LockedMap::new(map))
    }

    /// Clone the proxy for the given registry name, if configured.
    pub fn get(&self, name: &str) -> Option<CargoIndexProxy> {
        self.0.get(name)
    }

    /// Replace this map's contents with `other`'s (called by the hot-reload applier).
    pub fn replace_from(&self, other: &Self) {
        self.0.replace_from(&other.0);
    }
}

impl From<HashMap<String, CargoIndexProxy>> for CargoIndexMap {
    fn from(map: HashMap<String, CargoIndexProxy>) -> Self {
        Self::new(map)
    }
}

use actix_web::web;
use handlers::back_office::ops::eviction::EvictionServiceMap;
use handlers::back_office::ops::warming::WarmingServiceMap;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::OpenApi;
use utoipa_actix_web::{service_config::ServiceConfig as UtoipaServiceConfig, AppExt};
use utoipa_scalar::{Scalar, Servable as _};

use sqlx::PgPool;

use batlehub_adapters::auth::OidcSsoFlow;
use batlehub_core::{
    ports::{StorageAdminRepository, UserTokenRepository},
    services::{AdminService, ProxyMetrics, ProxyService, SbomService},
};
use metrics_exporter_prometheus::PrometheusHandle;

pub use handlers::auth::OidcProviderNames;
pub use handlers::back_office::exposure::ExposureConfig;
pub use handlers::flags::FlagSources;
pub use handlers::front_office::cli_download::CliBinaryPath;
pub use handlers::healthz::{healthz, livez};
pub use handlers::metrics::prometheus_metrics;
pub use handlers::proxy::cargo::CargoIndexProxy;
pub use middleware::AuthMiddlewareFactory;
pub use middleware::HostRoutingMiddlewareFactory;
pub use middleware::IpBlockMiddlewareFactory;
pub use middleware::PeerTrust;
pub use middleware::ProxyTrust;
pub use middleware::RateLimitMiddlewareFactory;
pub use middleware::RateLimitService;
pub use middleware::UserBlockMiddlewareFactory;
pub use middleware::{
    protocol_document_csp, security_headers, API_DOCS_CSP, PROTOCOL_DOCUMENT_CSP,
};
pub use spa::{configure_spa, narrow_csp, SpaDir};

#[derive(OpenApi)]
#[openapi(
    tags(
        (name = "proxy/github",   description = "GitHub proxy — releases, assets, tarballs, raw files (also serves Forgejo/Gitea registries, which share this URL scheme)"),
        (name = "proxy/gitlab",   description = "GitLab proxy — releases, release link assets, and source archives"),
        (name = "proxy/deb",      description = "Debian APT repository — proxy + local hosting (Packages/Release generation, Ed25519 OpenPGP signing)"),
        (name = "proxy/rpm",      description = "RPM/YUM repository — proxy + local hosting (repodata generation, Ed25519 OpenPGP signing)"),
        (name = "proxy/pacman",   description = "Arch Linux pacman repository — proxy + local hosting (.pkg.tar.zst, repo DB generation, Ed25519 OpenPGP signing)"),
        (name = "proxy/npm",      description = "npm proxy — packuments, version metadata, tarballs"),
        (name = "proxy/cargo",    description = "Cargo proxy — sparse index, crate metadata, .crate downloads"),
        (name = "proxy/openvsx",  description = "OpenVSX & VS Code Marketplace — extension gallery (extensionquery, assets, item), the OpenVSX REST API, VSIX packages, and private extension publishing"),
        (name = "proxy/goproxy",    description = "Go module proxy — version info, go.mod, and zip downloads"),
        (name = "proxy/terraform",  description = "Terraform registry — provider and module proxy, private module/provider publishing"),
        (name = "proxy/rubygems",   description = "RubyGems registry — gem downloads, version listing, and private gem publishing"),
        (name = "proxy/pypi",       description = "PyPI registry — simple index proxy with URL rewriting, wheel/sdist downloads, and twine-compatible publish"),
        (name = "proxy/conda",      description = "Conda channel proxy — repodata.json, package downloads, and private channel publishing"),
        (name = "proxy/nuget",      description = "NuGet registry — service index, flat container, registration metadata, .nupkg download, and private package publishing"),
        (name = "proxy/jetbrains-marketplace", description = "JetBrains Marketplace — IDE-facing plugin API (search, compatible updates, meta.json, downloads), updatePlugins.xml custom repository, and marketplace-compatible plugin publishing"),
        (name = "proxy/generic",    description = "Generic file mirror — path-addressed proxy cache for upstreams with no package protocol (toolchain tarballs, vendor CDNs), restricted by a path_allow allowlist"),
        (name = "proxy/nodedist",   description = "Node distributions (nvm, fnm, n, mise) — the nodejs.org/dist tree as a typed registry: filtered index.tab/index.json listings, per-release tarballs and SHASUMS256.txt byte-exact"),
        (name = "proxy/sdkman",     description = "SDKMAN — the candidates API and the download broker as one registry: filtered versions/all, candidates/default and the rendered sdk list table, a blocked version answered `invalid` at candidates/validate, hook scripts relayed byte-exact, the broker's 302 followed server-side and cached"),
        (name = "front-office",     description = "User-facing package information"),
        (name = "user",             description = "Caller-scoped reads — quota, downloads and advisories for whoever holds the token, never for anyone else"),
        (name = "security",         description = "Supply-chain verdicts (RFC 0018) — what a held or warned version is held for, when it lifts, and a rescan request; what `batlehub why` and `batlehub wait` read"),
        (name = "explore",          description = "Package explorer — browse and search across registries"),
        (name = "back-office",    description = "Admin management (requires Admin role)"),
        (name = "notifications",  description = "Inbound webhook receiver — accepts events from external systems"),
    ),
    modifiers(&SecurityAddon),
)]
pub struct ApiDoc;

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer_token",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("token")
                    .build(),
            ),
        );
    }
}

fn collect_routes(cfg: &mut UtoipaServiceConfig) {
    use handlers::{
        auth::{
            oidc::{list_oidc_providers, oidc_callback, oidc_login, oidc_refresh},
            tokens::{create_token, list_tokens, revoke_token},
        },
        back_office::{
            access_check::admin_access_check,
            audit::{audit_log, audit_pulls, export_audit_log, purge_audit_log},
            authz_explain::admin_authz_explain,
            authz_shadow::authz_shadow,
            bulk::{
                bulk_delete, bulk_unyank, bulk_yank as bulk_yank_handler, deprecate, relist,
                undeprecate, unlist,
            },
            config::{
                apply_pending_reload, clear_banner, discard_pending_reload, get_config_content,
                get_config_warnings, get_pending_reload, list_config_changes,
                load_config_from_content, reload_config, set_banner, validate_config_content,
            },
            explore::invalidate_explore_cache,
            governance::{
                beta_channel::{add_beta_member, list_beta_members, remove_beta_member},
                grants::{delete_grant, list_grants, put_grant},
                ownership::{add_package_owner, list_package_owners, remove_package_owner},
                policy::{
                    delete_gate_exemption, delete_package_policy, delete_version_policy,
                    get_package_policy, get_version_policy, list_exemptions, put_package_policy,
                    put_version_policy, set_gate_exemption,
                },
                signing_keys::{
                    assign_plugin_channel, delete_signing_key, list_signing_keys, set_signing_key,
                },
                subjects::list_subjects,
                team_namespaces::{
                    claim_namespace, list_namespaces, my_namespace_packages, my_namespaces,
                    release_namespace,
                },
                user_block::{block_user, list_blocked_users, unblock_user},
            },
            health::{clear_registry_cache, registry_health},
            notification::{
                create_subscription, delete_subscription, get_subscription,
                list_notification_channels, list_subscriptions, test_subscription,
                update_subscription,
            },
            ops::{
                eviction::{coherence_sweep, delete_cached_artifact, evict_registry},
                ip_blocks::{block_ip, list_blocked_ips, unblock_ip},
                quota::{
                    get_quota_for_user, list_quota, list_quota_for_registry, reset_quota_for_user,
                },
                release_import::{import_registry, list_all_imports, list_registry_imports},
                upstream::{get_upstream_status, list_disappeared, recheck_upstream},
                warming::{get_warming_status, warm_registry},
            },
            packages::{
                block_package, bulk_block_packages, bulk_delete_packages, bulk_unblock_packages,
                delete_package, invalidate_package, list_packages as admin_list_packages,
                package_detail, unblock_package,
            },
            retention::{run_retention, set_retention_pin},
            sbom::{export_org_sbom, get_artifact_sbom},
            stats::admin_stats,
            stats_history::admin_stats_history,
            tombstones::{compact_tombstones, list_tombstones},
            visibility::{get_package_visibility, set_package_visibility},
        },
        front_office::{
            banner::get_banner,
            cli_download::download_cli,
            explore::{
                explore_fetch_version, explore_package_detail, explore_package_readme,
                explore_packages, explore_readme_image, explore_registry_stats,
                explore_upstream_search,
            },
            me::{me, my_advisories, my_downloads, my_quota},
            packages::{check_access, list_packages},
            registries::list_registries,
        },
        inbound_webhook::{list_inbound_events, receive_inbound_webhook},
        proxy::{
            cargo::{
                cargo_add_owners, cargo_owners, cargo_publish, cargo_registry_config,
                cargo_registry_index, cargo_remove_owners, cargo_unyank, cargo_yank,
                download_crate,
            },
            composer::{
                composer_dist, composer_p2_metadata, composer_packages_json,
                composer_security_advisories, composer_upload, composer_yank,
            },
            // Register most-specific patterns first so actix-web resolves correctly:
            // cargo api/v1 (literal "api" segment) > cargo index (literal "registry" segment) >
            // github (owner/repo/verb) > cargo download (literal "download") >
            // maven (literal "maven2" segment) >
            // vscode gallery (literal "vscode"/"api" prefixes) > openvsx vsix (literal
            // "vsix") > npm audit bulk/quick > npm tarball >
            // shared version metadata > shared packument
            // nuget: vuln page/index > registration > flat > search
            // composer: upload/yank > advisories (literal "api") > p2 > dist > packages.json
            // jetbrains-marketplace: api/updates/upload > api/search/* > api/plugins/* >
            //   plugins/list > plugin/download > pluginManager > updatePlugins.xml >
            //   literal files/*.json > files/{p}/{u}/meta.json > files/{p}/meta.json >
            //   files/{p}/{u}/{file} — all before the shared npm version/packument wildcards
            conda::{
                conda_channeldata, conda_current_repodata, conda_file_download, conda_publish,
                conda_repodata, conda_repodata_bz2, conda_repodata_zst,
            },
            forgejo::fj_packages,
            generic::generic_get,
            github::{
                download_asset, download_asset_by_name, download_raw, download_tarball,
                download_zipball, get_release, list_releases,
            },
            gitlab::{
                gl_download_archive, gl_download_link, gl_download_raw, gl_get_release,
                gl_list_releases, gl_packages,
            },
            goproxy::{
                goproxy_file, goproxy_latest, goproxy_list, goproxy_publish, goproxy_sumdb,
                goproxy_vuln_entry, goproxy_vuln_index, goproxy_vuln_query,
            },
            jetbrains::jetbrains_get,
            jetbrains_marketplace::{
                jbm_aggregation, jbm_broken_plugins, jbm_comments, jbm_compatible_updates,
                jbm_feature_implementations, jbm_file_download, jbm_ide_extensions,
                jbm_jb_plugins_xml_ids, jbm_plugin_download, jbm_plugin_info, jbm_plugin_manager,
                jbm_plugin_meta, jbm_plugin_updates, jbm_plugins_list, jbm_plugins_xml_ids,
                jbm_search_plugins, jbm_search_plugins_ide, jbm_update_meta,
                jbm_update_plugins_xml, jbm_upload,
            },
            maven::{maven_get, maven_put},
            nodedist::{nodedist_file, nodedist_index_json, nodedist_index_tab},
            npm::{
                audit_bulk, audit_bulk_legacy, audit_quick, audit_quick_legacy,
                download_tarball as npm_download_tarball, get_packument, get_version,
                npm_dist_tag_add, npm_dist_tag_remove, npm_dist_tags, npm_ping, npm_publish,
                npm_whoami,
            },
            nuget::{
                nuget_autocomplete, nuget_flat_download, nuget_flat_versions, nuget_publish,
                nuget_registration, nuget_search, nuget_service_index, nuget_symbol_publish,
                nuget_vuln_index, nuget_vuln_page, nuget_yank,
            },
            openvsx::{download_vsix, vsix_publish, vsix_signature_attach},
            pypi::{
                pypi_file_download, pypi_json, pypi_publish, pypi_simple_package, pypi_simple_root,
            },
            repo::{
                deb_get, pacman_get,
                publish::{deb_publish, pacman_publish, rpm_publish},
                rpm_get,
            },
            rubygems::{
                gem_compact_info, gem_compact_names, gem_compact_versions, gem_download,
                gem_gemspec, gem_info, gem_publish, gem_specs_full, gem_specs_latest,
                gem_specs_prerelease, gem_unyank, gem_versions, gem_yank,
            },
            sdkman::{
                sdkman_candidate_default, sdkman_candidates_all, sdkman_candidates_list,
                sdkman_download, sdkman_healthcheck, sdkman_hook, sdkman_selfupdate,
                sdkman_selfupdate_version, sdkman_validate, sdkman_versions_all,
                sdkman_versions_list,
            },
            search::{cargo_search, composer_list, composer_search, npm_search},
            terraform::{
                terraform_discovery, terraform_discovery_host_routed, terraform_mirror_index,
                terraform_mirror_version, terraform_module_artifact, terraform_module_download,
                terraform_module_metadata, terraform_module_unyank, terraform_module_upload,
                terraform_module_versions, terraform_module_yank, terraform_provider_artifact,
                terraform_provider_binary_upload, terraform_provider_download,
                terraform_provider_shasums, terraform_provider_shasums_sig,
                terraform_provider_unyank, terraform_provider_upload, terraform_provider_versions,
                terraform_provider_yank,
            },
            vsx::{
                openvsx_extension, openvsx_extension_version, openvsx_file, openvsx_namespace,
                openvsx_namespace_create, openvsx_public_key, openvsx_publish, openvsx_search,
                openvsx_version, vsx_asset, vsx_extension_query, vsx_item, vsx_unpkg,
                vsx_vspackage,
            },
        },
    };

    cfg.service(list_oidc_providers);
    cfg.service(oidc_login);
    cfg.service(oidc_callback);
    cfg.service(oidc_refresh);
    cfg.service(create_token);
    cfg.service(list_tokens);
    cfg.service(revoke_token);
    // Cargo publish API (literal "api/v1" sub-path — most specific, must precede download)
    //
    // `cargo_search` is here rather than with the other search routes because
    // openvsx's `api/{namespace}/{extension}` is greedy enough to claim
    // `api/v1/crates` — which is exactly what it did before this route existed
    // (RFC 0009 §7.7). Registered above it, not after.
    cfg.service(cargo_search); // GET …/api/v1/crates
    cfg.service(cargo_publish);
    cfg.service(cargo_yank);
    cfg.service(cargo_unyank);
    cfg.service(cargo_owners);
    // `cargo owner --add` / `--remove` (RFC 0009 §7.6). Same path as the GET,
    // different methods, so ordering between them does not matter.
    cfg.service(cargo_add_owners);
    cfg.service(cargo_remove_owners);
    // Cargo index (literal "registry" sub-path)
    cfg.service(cargo_registry_config);
    cfg.service(cargo_registry_index);
    // Forgejo/GitLab package registries: literal `api/…` prefix — register before
    // the GitHub `{owner}/{repo}` routes so it isn't captured as owner="api".
    cfg.service(fj_packages); // GET …/api/packages/{path}  (Forgejo/Gitea)
    cfg.service(gl_packages); // GET …/api/v4/{path}         (GitLab)
                              // GitHub (owner/repo structure, multi-segment) — also serves Forgejo releases.
    cfg.service(list_releases);
    cfg.service(get_release);
    cfg.service(download_asset_by_name);
    cfg.service(download_asset);
    cfg.service(download_tarball);
    cfg.service(download_zipball);
    cfg.service(download_raw);
    // GitLab (distinct `/-/` delimiter; most-specific first)
    // RFC 0019 §4.1 `[api_reads]` — typed, read-only, opt-in. Before the
    // archive and raw routes so `/tags` is not read as a ref.
    cfg.service(crate::handlers::proxy::forge_api::forge_tags); // …/{o}/{r}/tags
    cfg.service(crate::handlers::proxy::forge_api::forge_commit); // …/{o}/{r}/commits/{sha}
    cfg.service(crate::handlers::proxy::forge_api::forge_branch); // …/{o}/{r}/branches/{name}
    cfg.service(gl_download_link); // …/-/releases/{tag}/downloads/{name}
    cfg.service(gl_get_release); // …/-/releases/{tag}
    cfg.service(gl_list_releases); // …/-/releases
    cfg.service(gl_download_archive); // …/-/archive/{tag}/{filename}
    cfg.service(gl_download_raw); // …/-/raw/{ref}/{path}
                                  // Deb / RPM repositories: publish (PUT) before the catch-all read (GET).
    cfg.service(deb_publish); // PUT …/deb/pool/{dist}/{component}/upload
    cfg.service(rpm_publish); // PUT …/rpm/upload
    cfg.service(deb_get); // GET …/deb/{path}
    cfg.service(rpm_get); // GET …/rpm/{path}
    cfg.service(pacman_publish); // PUT …/pacman/upload
    cfg.service(pacman_get); // GET …/pacman/{path}
    cfg.service(jetbrains_get); // GET …/jetbrains/{path} (proxy-only cache)
    cfg.service(generic_get); // GET …/generic/{path}   (proxy-only cache)
                              // Node dist tree (RFC 0010): the two listing documents before the
                              // `{version}/{file}` artifact route, so `index.tab` is a document
                              // and never a file; all three before the npm catch-alls below.
    cfg.service(nodedist_index_tab); // GET …/nodedist/index.tab   (filtered document)
    cfg.service(nodedist_index_json); // GET …/nodedist/index.json  (filtered document)
    cfg.service(nodedist_file); // GET …/nodedist/{version}/{file}
                                // SDKMAN (RFC 0010 phase 6). The literal `candidates/all`,
                                // `candidates/list`, `candidates/default/{c}` and
                                // `candidates/validate/…` routes before the
                                // `candidates/{c}/{plat}/…` ones, so a candidate named
                                // `default` or `validate` cannot shadow them; every one
                                // before the npm catch-alls below.
    cfg.service(sdkman_candidates_all); // GET …/sdkman/candidates/all      (relayed)
    cfg.service(sdkman_candidates_list); // GET …/sdkman/candidates/list     (relayed)
    cfg.service(sdkman_candidate_default); // GET …/sdkman/candidates/default/{c}  (filtered, composed)
    cfg.service(sdkman_validate); // GET …/sdkman/candidates/validate/{c}/{v}/{plat}  (blocked ⇒ invalid)
    cfg.service(sdkman_versions_all); // GET …/sdkman/candidates/{c}/{plat}/versions/all   (filtered)
    cfg.service(sdkman_versions_list); // GET …/sdkman/candidates/{c}/{plat}/versions/list  (filtered)
    cfg.service(sdkman_hook); // GET …/sdkman/hooks/{phase}/{c}/{v}/{plat}  (relayed, byte-exact)
    cfg.service(sdkman_healthcheck); // GET …/sdkman/healthcheck             (relayed)
    cfg.service(sdkman_selfupdate_version); // GET …/sdkman/broker/version/sdkman/{component}/{channel}
    cfg.service(sdkman_selfupdate); // GET …/sdkman/selfupdate/{channel}/{plat}  (relayed)
    cfg.service(sdkman_download); // GET …/sdkman/broker/download/{c}/{v}/{plat}  (artifact)
                                  // Cargo download (literal "download" suffix)
    cfg.service(download_crate);
    // Go module proxy (multi-segment module paths — must precede generic packument routes)
    // Vuln DB passthrough: literal /v1/ paths registered before the module wildcard routes.
    cfg.service(goproxy_vuln_index); // GET …/v1/index.json
    cfg.service(goproxy_vuln_entry); // GET …/v1/ID/{id}.json
    cfg.service(goproxy_vuln_query); // POST …/v1/query
                                     // The checksum-database half of GOPROXY (RFC 0009 §7.4). Literal `sumdb/`
                                     // prefix, registered before the module wildcards below — a module path
                                     // regex of `[^@]+` would otherwise claim it.
    cfg.service(goproxy_sumdb); // GET …/sumdb/{path}
                                // PUT goproxy_publish must come before GET goproxy_file (same path pattern, different method)
    cfg.service(goproxy_publish);
    cfg.service(goproxy_latest);
    cfg.service(goproxy_list);
    cfg.service(goproxy_file);
    // Maven — PUT before GET (same path pattern, different method)
    cfg.service(maven_put);
    cfg.service(maven_get);
    // NuGet: publish (PUT) and yank (DELETE) before read routes; literal paths before wildcards
    cfg.service(nuget_symbol_publish); // PUT .../api/v2/symbolpackage (before api/v2/package)
    cfg.service(nuget_publish); // PUT  .../api/v2/package
    cfg.service(nuget_yank); // DELETE .../v2/package/{id}/{version}
    cfg.service(nuget_service_index); // GET .../v3/index.json
    cfg.service(nuget_vuln_page); // GET .../v3/vulnerabilities/page/{page}
    cfg.service(nuget_vuln_index); // GET .../v3/vulnerabilities/index.json
    cfg.service(nuget_registration); // GET .../v3/registration5/{id}/index.json
    cfg.service(nuget_flat_versions); // GET .../v3/flat/{id}/index.json
    cfg.service(nuget_search); // GET .../v3/query
    cfg.service(nuget_autocomplete); // GET .../v3/autocomplete
    cfg.service(nuget_flat_download); // GET .../v3/flat/{id}/{version}/{filename}
                                      // Terraform modules — longer paths first (unyank > yank > artifact > upload > download > versions)
    cfg.service(terraform_module_unyank); // POST …/versions/{ver}/unyank
    cfg.service(terraform_module_yank); // DELETE …/versions/{ver}
    cfg.service(terraform_module_artifact); // GET …/{ver}/artifact
    cfg.service(terraform_module_upload); // POST …/{ver}
    cfg.service(terraform_module_download); // GET …/{ver}/download
    cfg.service(terraform_module_versions); // GET …/versions
                                            // Module metadata is the *shortest* module path, so it goes last of the
                                            // module routes or it would claim `…/{ver}/download` with ver="{ver}"
                                            // and name="download" (RFC 0009 §7.2).
    cfg.service(terraform_module_metadata); // GET …/{ver}
                                            // Terraform providers — binary PUT/GET before download, unyank/yank before upload/versions
    cfg.service(terraform_provider_unyank); // POST …/versions/{ver}/unyank
    cfg.service(terraform_provider_yank); // DELETE …/versions/{ver}
    cfg.service(terraform_provider_binary_upload); // PUT …/{ver}/artifact/{os}/{arch}
    cfg.service(terraform_provider_artifact); // GET …/{ver}/artifact/{os}/{arch}
                                              // `shasums.sig` before `shasums`: the shorter literal is a prefix of the
                                              // longer one only in reading order, but registering the specific one first
                                              // keeps that independent of actix's matching order.
    cfg.service(terraform_provider_shasums_sig); // GET …/{ver}/shasums.sig
    cfg.service(terraform_provider_shasums); // GET …/{ver}/shasums
    cfg.service(terraform_provider_download); // GET …/{ver}/download/{os}/{arch}
    cfg.service(terraform_provider_upload); // POST …/versions (write)
    cfg.service(terraform_provider_versions); // GET …/versions
                                              // Terraform service discovery, at the host root rather than under
                                              // `/proxy/{registry}/` — that is where the protocol looks for it. Answers
                                              // only on a host bound to one registry (RFC 0009 §7.2).
    cfg.service(terraform_discovery); // GET /.well-known/terraform.json
                                      // ...and the path the host-routing middleware rewrites that to, which is
                                      // the only path it can arrive on: registered here, above the npm/cargo
                                      // catch-all that used to claim it (RFC 0009 §12.11).
    cfg.service(terraform_discovery_host_routed); // GET /proxy/{registry}/.well-known/terraform.json
                                                  // The provider network mirror. Four- and five-segment patterns ending in a
                                                  // literal `.json`, so they must precede the shared npm wildcards below —
                                                  // and they are registered after every literal `/v1/…` route above, because
                                                  // `{hostname}/{namespace}/{ptype}/index.json` would otherwise claim
                                                  // `v1/providers/{ns}/index.json`-shaped paths.
                                                  // `index.json` first: `index` is a perfectly good `{version}` capture, so
                                                  // the version route claims the index path if it is registered first — which
                                                  // it was, and `protocol_conformance.rs` is what said so.
    cfg.service(terraform_mirror_index); // GET …/{host}/{ns}/{type}/index.json
    cfg.service(terraform_mirror_version); // GET …/{host}/{ns}/{type}/{ver}.json
                                           // RubyGems — yank/unyank/publish before download (same /api/v1/gems prefix, different methods)
    cfg.service(gem_yank);
    cfg.service(gem_unyank);
    cfg.service(gem_publish);
    // RubyGems compact index — what Bundler actually resolves from (RFC 0009
    // §7.3). All three are literal-prefix routes and must precede the shared
    // npm `{package}` / `{package}/{version}` wildcards below, which otherwise
    // answer `/versions` and `/names` as two-segment packument requests and
    // `/info/{gem}` as a version lookup. That is not hypothetical: it is what
    // they did before these routes existed, and
    // `protocol_conformance.rs` pins each one against the catch-all that ate it.
    cfg.service(gem_compact_versions); // GET …/versions
    cfg.service(gem_compact_names); // GET …/names
    cfg.service(gem_compact_info); // GET …/info/{gem}
                                   // gemspec (literal "quick/Marshal.4.8") before generic gem download
    cfg.service(gem_gemspec);
    cfg.service(gem_download);
    cfg.service(gem_info);
    cfg.service(gem_versions);
    cfg.service(gem_specs_full);
    cfg.service(gem_specs_latest);
    cfg.service(gem_specs_prerelease);
    // Composer: literal "api" routes before "p2" before "dist" before "packages.json"
    cfg.service(composer_upload); // POST …/api/upload
    cfg.service(composer_yank); // DELETE …/api/packages/{vendor}/{package}/versions/{version}
    cfg.service(composer_security_advisories); // GET …/api/security-advisories/
    cfg.service(composer_search); // GET …/search.json  (literal, before packages.json)
    cfg.service(composer_list); // GET …/list.json
    cfg.service(composer_p2_metadata); // GET …/p2/{path:.*}
    cfg.service(composer_dist); // GET …/dist/{vendor}/{package}/{version}
    cfg.service(composer_packages_json); // GET …/packages.json
                                         // PyPI: publish (POST /legacy/) before simple package (GET /simple/{pkg}/) before root (GET /simple/) before file download
    cfg.service(pypi_publish); // POST …/legacy/
    cfg.service(pypi_json); // GET …/pypi/{package}/json (literal, before the npm wildcards)
    cfg.service(pypi_simple_package); // GET …/simple/{package}/
    cfg.service(pypi_simple_root); // GET …/simple/
    cfg.service(pypi_file_download); // GET …/packages/{filename}
                                     // Conda: literal repodata routes before wildcard file download; publish (POST) before GET
    cfg.service(conda_publish); // POST …/{platform}/
                                // Compressed variants before the plain one: `repodata.json.zst` would
                                // otherwise be matched by nothing at all and fall through to the npm
                                // three-segment catch-all, which is what it did before these existed
                                // (RFC 0009 §7.5). `channeldata.json` is channel-root, so it must precede
                                // the two-segment npm catch-all as well.
    cfg.service(conda_channeldata); // GET …/channeldata.json
    cfg.service(conda_repodata_zst); // GET …/{platform}/repodata.json.zst
    cfg.service(conda_repodata_bz2); // GET …/{platform}/repodata.json.bz2
    cfg.service(conda_repodata); // GET …/{platform}/repodata.json
    cfg.service(conda_current_repodata); // GET …/{platform}/current_repodata.json
    cfg.service(conda_file_download); // GET …/{platform}/{filename}
                                      // VS Code gallery and OpenVSX API — literal-prefix routes, most specific
                                      // first. All of them must precede the shared npm version/packument
                                      // wildcards below, which would otherwise swallow `vscode/item` as
                                      // {name}/{version}. `api/-/search` must precede `api/{namespace}/…` or `-`
                                      // is taken for a publisher.
    cfg.service(vsx_extension_query); // POST …/vscode/gallery/extensionquery
    cfg.service(vsx_vspackage); // GET  …/vscode/gallery/publishers/{p}/vsextensions/{n}/{v}/vspackage
    cfg.service(vsx_asset); // GET  …/vscode/asset/{p}/{n}/{v}/{assetType}
    cfg.service(vsx_unpkg); // GET  …/vscode/unpkg/{p}/{n}/{v}/{path}
    cfg.service(vsx_item); // GET  …/vscode/item
                           // OpenVSX/VSCode VSIX publish (PUT) and download (GET) — same path, different method
    cfg.service(vsix_publish);
    cfg.service(vsix_signature_attach); // PUT …/{ext}/{version}/vsix/signature (RFC 0020 §13.6)
    cfg.service(download_vsix);
    // JetBrains Marketplace — literal-prefix routes, most-specific first; must all
    // precede the shared npm version/packument wildcards below, which would
    // otherwise swallow e.g. "plugins/list" as {name}/{version}.
    cfg.service(jbm_upload); // POST …/api/updates/upload
    cfg.service(jbm_compatible_updates); // POST …/api/search/updates/compatible
    cfg.service(jbm_aggregation); // GET …/api/search/aggregation/{field}
    cfg.service(jbm_search_plugins); // GET …/api/search/plugins
    cfg.service(jbm_search_plugins_ide); // GET …/api/searchPlugins
    cfg.service(jbm_comments); // GET …/api/products/intellij/plugins/{id}/comments
    cfg.service(jbm_plugin_updates); // GET …/api/plugins/{id}/updates
    cfg.service(jbm_plugin_info); // GET …/api/plugins/{id}
    cfg.service(jbm_plugins_list); // GET …/plugins/list
    cfg.service(jbm_plugin_download); // GET …/plugin/download
    cfg.service(jbm_plugin_manager); // GET …/pluginManager
    cfg.service(jbm_update_plugins_xml); // GET …/updatePlugins.xml
    cfg.service(jbm_feature_implementations); // GET …/feature/getImplementations
    cfg.service(jbm_plugins_xml_ids); // GET …/files/pluginsXMLIds.json
    cfg.service(jbm_jb_plugins_xml_ids); // GET …/files/jbPluginsXMLIds.json
    cfg.service(jbm_broken_plugins); // GET …/files/brokenPlugins.json
    cfg.service(jbm_ide_extensions); // GET …/files/IDE/extensions.json
    cfg.service(jbm_update_meta); // GET …/files/{p}/{u}/meta.json
    cfg.service(jbm_plugin_meta); // GET …/files/{p}/meta.json
    cfg.service(jbm_file_download); // GET …/files/{p}/{u}/{file}
                                    // OpenVSX REST API. `api/{namespace}/{extension}` is greedy — it matches
                                    // `api/plugins/{id}`, `api/search/updates/compatible`, `api/v4/{path}` and
                                    // every other two-segment `api/…` route — so it must be registered after
                                    // all of them. `api/-/search` comes before its own siblings, or `-` is
                                    // taken for a publisher name. `require_vsx` would 404 a misrouted request
                                    // rather than answer it wrongly, but a 404 on JetBrains' plugin API is a
                                    // broken registry all the same.
                                    // `ovsx publish` (RFC 0009 §12.6). Literal `api/-/` prefix, so it must
                                    // precede the greedy `api/{namespace}/{extension}` below — `-` would
                                    // otherwise be taken for a publisher name.
                                    // `api/version` is a literal two-segment path and must precede
                                    // `api/{namespace}`, which would otherwise take `version` for a publisher.
    cfg.service(openvsx_version); // GET …/api/version
    cfg.service(openvsx_publish); // POST …/api/-/publish
    cfg.service(openvsx_search); // GET …/api/-/search
                                 // RFC 0020 §4.2: `/api/-/public-key/{id}` sits under the same `-` segment
                                 // as search, and for the same reason is registered before the greedy
                                 // `api/{ns}/{ext}` route below.
    cfg.service(openvsx_public_key);
    cfg.service(openvsx_file); // GET …/api/{ns}/{ext}/{v}/file/{name}
    cfg.service(openvsx_extension_version); // GET …/api/{ns}/{ext}/{v}
    cfg.service(openvsx_extension); // GET …/api/{ns}/{ext}
                                    // Shortest of the `api/…` family, so last: it would otherwise claim every
                                    // two-segment `api/{x}` path including `api/version`.
    cfg.service(openvsx_namespace); // GET …/api/{namespace}
                                    // Search (RFC 0009 §7.7). All three are literal-prefix routes that must
                                    // precede the shared npm `{package}` wildcards below; `api/v1/crates` must
                                    // also precede openvsx's `api/{namespace}/{extension}`, which ate it before
                                    // this route existed.
    cfg.service(npm_search); // GET …/-/v1/search
                             // The rest of npm's CLI surface (RFC 0009 §7.1). All literal `-/` prefixes,
                             // registered before the shared `{package}/{version}` catch-all — which
                             // until now answered `-/whoami` and `-/ping` with **200 and a package
                             // document**, taking `-` for a package name.
    cfg.service(npm_whoami); // GET …/-/whoami
    cfg.service(npm_ping); // GET …/-/ping
    cfg.service(npm_dist_tag_add); // PUT …/-/package/{pkg}/dist-tags/{tag}
    cfg.service(npm_dist_tag_remove); // DELETE …/-/package/{pkg}/dist-tags/{tag}
    cfg.service(npm_dist_tags); // GET …/-/package/{pkg}/dist-tags
                                // npm audit pass-through. The first two are the paths npm actually sends
                                // (RFC 0009 §7.1); the `_legacy` pair are the invented ones we shipped,
                                // kept as aliases until a release that can drop them. Bulk before quick in
                                // each pair, as the longer literal.
    cfg.service(audit_bulk);
    cfg.service(audit_quick);
    cfg.service(audit_bulk_legacy);
    cfg.service(audit_quick_legacy);
    // npm tarball (literal "tarball" suffix)
    cfg.service(npm_download_tarball);
    // npm publish (PUT same path as packument — different method, registered before GET)
    cfg.service(npm_publish);
    // Shared npm/cargo: version metadata then packument (more specific first)
    cfg.service(get_version);
    cfg.service(get_packument);
    cfg.service(me);
    // `/api/v1/me/*` — caller-scoped reads (RFC 0004 §5.3). Registered before
    // `me` would matter only if it were a prefix route; it is exact, but the
    // grouping keeps them together.
    cfg.service(my_quota);
    cfg.service(my_downloads);
    cfg.service(my_advisories);
    // RFC 0018 phase 2: the verdict endpoint.
    cfg.service(crate::handlers::security::get_verdict); // GET  /api/v1/verdicts/{registry}/{name}/{version}
    cfg.service(crate::handlers::security::rescan_verdict); // POST /api/v1/verdicts/{registry}/{name}/{version}/rescan
    cfg.service(crate::handlers::security::list_pullers); // GET  /api/v1/verdicts/{registry}/{name}/{version}/pullers
    cfg.service(crate::handlers::security::list_verdicts); // GET  /api/v1/admin/verdicts
    cfg.service(crate::handlers::security::bulk_rescan); // POST /api/v1/admin/verdicts/rescan
    cfg.service(crate::handlers::security::backfill_verdicts); // POST /api/v1/admin/verdicts/backfill
    cfg.service(download_cli);
    cfg.service(list_registries);
    // Explore: detail path before list (more specific first); upstream before
    // list. The README route is more specific than the detail route it extends,
    // so it comes first — otherwise `/{registry}/{name}/readme` would match the
    // detail path with `name = "{name}/readme"` on the registries whose names
    // contain a slash.
    cfg.service(explore_package_readme);
    // Likewise more specific than the detail route, and than the README route:
    // it carries the version and the index as their own segments.
    cfg.service(explore_readme_image);
    // Same shape, same reason: version and action as their own segments, so it
    // must not be shadowed by the detail route.
    cfg.service(explore_fetch_version);
    cfg.service(explore_package_detail);
    cfg.service(explore_upstream_search);
    cfg.service(explore_packages);
    cfg.service(crate::handlers::front_office::explore::explore_forge_refs); // RFC 0019 §6.5
    cfg.service(explore_registry_stats);
    cfg.service(list_packages);
    cfg.service(check_access);
    cfg.service(invalidate_explore_cache);
    cfg.service(admin_list_packages);
    cfg.service(package_detail);
    cfg.service(block_package);
    cfg.service(unblock_package);
    cfg.service(delete_package);
    cfg.service(bulk_delete_packages);
    cfg.service(bulk_block_packages);
    cfg.service(bulk_unblock_packages);
    cfg.service(invalidate_package);
    cfg.service(registry_health);
    cfg.service(clear_registry_cache);
    cfg.service(export_audit_log); // specific path before parameterised handlers
    cfg.service(audit_log);
    cfg.service(purge_audit_log);
    // The identity-scoped half of RFC 0018 §4.2, transposing `verdicts pullers`.
    cfg.service(audit_pulls); // GET /api/v1/audit/pulls
                              // RFC 0002 (recast): pushed flags and the exposure report.
    cfg.service(crate::handlers::back_office::exposure::export_exposure); // GET /api/v1/admin/exposure/export
    cfg.service(crate::handlers::back_office::exposure::exposure_report); // GET /api/v1/admin/exposure
    cfg.service(crate::handlers::back_office::flags::list_flags); // GET /api/v1/admin/flags
                                                                  // RFC 0008 §6.4 — the air gap's record, and the sink for a host nothing
                                                                  // rewrites.
    cfg.service(crate::handlers::air_gap::list_missing); // GET    /api/v1/admin/air-gap/missing
    cfg.service(crate::handlers::air_gap::purge_missing); // DELETE /api/v1/admin/air-gap/missing
    cfg.service(crate::handlers::air_gap::unmirrored_sink); // GET  /_air-gap/unmirrored/{tail}
    cfg.service(crate::handlers::air_gap::import_bundle); // POST /api/v1/admin/bundle/import
    cfg.service(crate::handlers::air_gap::list_bundles); // GET  /api/v1/admin/bundle
    cfg.service(crate::handlers::flags::push_flags); // POST   /api/v1/flags/{source}
    cfg.service(crate::handlers::flags::revoke_flag); // DELETE /api/v1/flags/{source}/{external_id}
    cfg.service(get_warming_status);
    cfg.service(warm_registry);
    cfg.service(list_all_imports); // GET, what the console reads
    cfg.service(list_registry_imports); // GET, one registry
    cfg.service(import_registry);
    cfg.service(recheck_upstream); // POST /api/v1/admin/upstream/recheck (RFC 0014 §4.6)
    cfg.service(list_disappeared); // GET  /api/v1/admin/upstream/disappeared
    cfg.service(get_upstream_status); // GET  /api/v1/admin/upstream/status/{registry}/{name}
    cfg.service(evict_registry);
    cfg.service(coherence_sweep);
    cfg.service(delete_cached_artifact);
    // More specific first: `/stats/history` before `/stats`.
    cfg.service(admin_stats_history);
    cfg.service(admin_stats);
    cfg.service(admin_access_check);
    cfg.service(admin_authz_explain);
    cfg.service(authz_shadow);
    // Quota admin (specific user route before registry-level route)
    cfg.service(reset_quota_for_user);
    cfg.service(get_quota_for_user);
    cfg.service(list_quota_for_registry);
    cfg.service(list_quota);
    // RFC 0017 §4.1 — the package/version grants editor. Registered before the
    // `{name:.*}` visibility routes below for the same reason ownership is:
    // a wildcard tail would otherwise swallow `/grants`.
    cfg.service(list_grants);
    cfg.service(put_grant);
    cfg.service(delete_grant);
    // Ownership admin
    cfg.service(list_package_owners);
    cfg.service(add_package_owner);
    cfg.service(remove_package_owner);
    // Package visibility admin (wildcard {name:.*} — registered after literal-suffix /owners routes)
    cfg.service(get_package_visibility);
    cfg.service(set_package_visibility);
    // RFC 0015 §6.3 — the package and version policy tiers.
    //
    // The version routes go first: both families end in a wildcard
    // (`{package:.*}`), and a package route registered before them would swallow
    // `…/policy/version/pkg/1.0.0` as a package named `version/pkg/1.0.0` —
    // the same ordering hazard the visibility routes above carry a note about.
    // The exemption routes are more specific than the version-policy ones they
    // sit under (`…/{version}/rules/{gate}`), so they go first — actix matches
    // in registration order and `{version}` would otherwise be free to swallow
    // the remaining segments.
    cfg.service(list_exemptions);
    cfg.service(set_gate_exemption);
    // RFC 0015 §4.2 — `terraform:signing-keys:write`.
    cfg.service(list_signing_keys);
    cfg.service(set_signing_key);
    cfg.service(delete_signing_key);
    // RFC 0015 §4.2 — `jetbrains:channel:assign`.
    cfg.service(assign_plugin_channel);
    // RFC 0015 §4.2 — `openvsx:namespace:claim`.
    cfg.service(openvsx_namespace_create);
    cfg.service(delete_gate_exemption);
    cfg.service(get_version_policy);
    cfg.service(put_version_policy);
    cfg.service(delete_version_policy);
    cfg.service(get_package_policy);
    cfg.service(put_package_policy);
    cfg.service(delete_package_policy);
    // Team namespace admin
    cfg.service(list_namespaces);
    cfg.service(claim_namespace);
    cfg.service(release_namespace); // wildcard {prefix:.*}
                                    // Team namespace user-facing
    cfg.service(my_namespaces);
    cfg.service(my_namespace_packages); // wildcard {prefix:.*}
                                        // Bulk operations admin
    cfg.service(bulk_yank_handler);
    cfg.service(bulk_unyank);
    cfg.service(bulk_delete);
    // Tombstones: what bulk_delete left behind, and the compaction of its detail
    cfg.service(run_retention);
    cfg.service(set_retention_pin);
    cfg.service(list_tombstones);
    cfg.service(compact_tombstones);
    // Deprecation & unlisting admin (single version)
    cfg.service(deprecate);
    cfg.service(undeprecate);
    cfg.service(unlist);
    cfg.service(relist);
    // Beta channel admin
    cfg.service(list_beta_members);
    cfg.service(add_beta_member);
    cfg.service(remove_beta_member);
    // IP block admin
    cfg.service(list_blocked_ips);
    cfg.service(block_ip);
    cfg.service(unblock_ip);
    // User block admin (specific /blocked list before parameterised /{user_id}/block)
    cfg.service(list_blocked_users);
    cfg.service(list_subjects);
    cfg.service(block_user);
    cfg.service(unblock_user);
    // Config reload admin (pending/apply before pending/delete — more specific first)
    cfg.service(reload_config);
    cfg.service(apply_pending_reload);
    cfg.service(get_pending_reload);
    cfg.service(discard_pending_reload);
    cfg.service(list_config_changes);
    cfg.service(get_config_warnings);
    // Config content (editor) endpoints
    cfg.service(get_config_content);
    cfg.service(validate_config_content);
    cfg.service(load_config_from_content);
    // Banner admin + public
    cfg.service(set_banner);
    cfg.service(clear_banner);
    cfg.service(get_banner);
    // SBOM: export (literal "export") before per-artifact (parameterised path)
    cfg.service(export_org_sbom);
    cfg.service(get_artifact_sbom);
    // Notifications admin (subscriptions/{id}/test before subscriptions/{id} — more specific first)
    cfg.service(list_notification_channels);
    cfg.service(list_subscriptions);
    cfg.service(create_subscription);
    cfg.service(test_subscription);
    cfg.service(get_subscription);
    cfg.service(update_subscription);
    cfg.service(delete_subscription);
    cfg.service(list_inbound_events);
    // Inbound webhooks (public-facing — no admin auth required)
    cfg.service(receive_inbound_webhook);
}

/// Return the raw OpenAPI JSON spec (auto-collected from route registrations).
pub fn openapi_spec() -> utoipa::openapi::OpenApi {
    let (_, openapi) = actix_web::App::new()
        .into_utoipa_app()
        .openapi(ApiDoc::openapi())
        .configure(collect_routes)
        .split_for_parts();
    openapi
}

/// Where the self-hosted Scalar bundle lives, relative to this origin.
///
/// Written there by `ui/build/copy-scalar.mjs` from the `@scalar/api-reference`
/// devDependency, and served by the console's static-file mount. Under
/// `assets/`, which [`crate::spa`] treats as build-owned: a missing file stays a
/// `404` rather than falling through to `index.html`, which is what lets
/// [`scalar_bundle_present`] detect its absence rather than the browser
/// receiving HTML where a `.js` was expected.
///
/// # Why this is not a CDN URL any more
///
/// `utoipa-scalar`'s stock template requests
/// `https://cdn.jsdelivr.net/npm/@scalar/api-reference` — **no version, no
/// integrity** — so every future release of a third-party package executed
/// automatically on the origin the console keeps its bearer *and refresh* tokens
/// on. Pinning the URL with an SRI hash closed the integrity half. Self-hosting
/// closes the rest:
///
/// - a private registry is frequently run with **no egress at all**, and
///   `/scalar` was a blank page in every such deployment;
/// - every load leaked the operator's IP and referrer to a third party;
/// - the page no longer depends on a CDN's uptime for this server's own docs;
/// - `script-src` drops to `'self'` — no third-party script origin remains, and
///   the SRI hash is unnecessary because same-origin needs no vouching;
/// - the bundle's supply chain (25 direct dependencies, ~190 transitive) is now
///   declared in `ui/pnpm-lock.yaml`, so `pnpm audit`, postmortem and the SBOM
///   cover it. That code was always shipped to the browser; it was simply not
///   declared anywhere a scanner could see.
pub const SCALAR_BUNDLE_PATH: &str = "assets/scalar/standalone.js";

/// Whether the built bundle is present under `static_dir`.
///
/// `static_dir` is optional, and a deployment that serves the API without the
/// console has no bundle to load. Rather than fall back to the CDN — which would
/// silently reinstate everything above, on exactly the air-gapped deployments
/// least able to reach it — `/scalar` degrades honestly. See
/// [`scalar_unavailable_html`].
pub fn scalar_bundle_present(static_dir: Option<&std::path::Path>) -> bool {
    static_dir.is_some_and(|dir| dir.join(SCALAR_BUNDLE_PATH).is_file())
}

/// The `/scalar` document.
///
/// The `$spec` placeholder is substituted by `Scalar::to_html`. The block
/// holding it is `type="application/json"`, so it is data the bundle reads, not
/// script the browser executes — which is why the CSP on this route needs no
/// `'unsafe-inline'` for scripts.
///
/// # `data-configuration`
///
/// The bundle reads `#api-reference[data-configuration]` as JSON. Rendering this
/// page in a real browser showed it reaching **three** third-party origins, not
/// the one the script tag named, so two of them are turned off here:
///
/// - `withDefaultFonts: false` — the default theme `@font-face`s fourteen
///   `.woff2` files from `fonts.scalar.com`. A font carries no integrity
///   attribute, so that origin could never have been pinned the way the bundle
///   was; it is dropped instead and the system font stack renders the page.
/// - `proxyUrl: ""` — "Test Request" otherwise routes through
///   `proxy.scalar.com`, which would send an operator's request — URL, headers,
///   and any token pasted into the explorer — through a third party. Empty means
///   the browser calls the API directly, which is same-origin here: this page
///   documents the server serving it.
///
/// The third origin, `api.scalar.com/vector/registry/*`, is **not** fixable
/// here or by self-hosting: those URLs are hardcoded in the bundle and ignore
/// the `apiBaseUrl` setting that looks like it should govern them (tried, no
/// effect). They are stopped one level out, by `connect-src 'self'` in
/// [`API_DOCS_CSP`] — which is why that directive is not the `*` an API explorer
/// superficially wants.
fn scalar_html() -> String {
    format!(
        r#"<!doctype html>
<html>
<head>
    <title>BatleHub API</title>
    <meta charset="utf-8"/>
    <meta name="viewport" content="width=device-width, initial-scale=1"/>
</head>
<body>
<script id="api-reference"
        type="application/json"
        data-configuration='{{"withDefaultFonts":false,"proxyUrl":""}}'>
    $spec
</script>
<script src="/{SCALAR_BUNDLE_PATH}"></script>
</body>
</html>
"#
    )
}

/// The `/scalar` document when the bundle is not on disk.
///
/// Honest degradation, chosen over a CDN fallback: falling back would reinstate
/// the third-party script exactly where it is least wanted, and would do it
/// silently — the page would look fine to anyone with egress and fail for
/// everyone else, which is how the original problem stayed unnoticed.
///
/// The spec is still embedded, in the same `type="application/json"` block the
/// working page uses, so `curl <this url>` remains a way to get the OpenAPI
/// document out of a server with no console assets. The page says so.
///
/// No script and no external reference of any kind, so it renders identically
/// under [`API_DOCS_CSP`].
fn scalar_unavailable_html() -> String {
    format!(
        r#"<!doctype html>
<html>
<head>
    <title>BatleHub API</title>
    <meta charset="utf-8"/>
    <meta name="viewport" content="width=device-width, initial-scale=1"/>
</head>
<body>
<h1>API reference unavailable</h1>
<p>
    The interactive reference is served from this origin rather than a CDN, and
    its bundle is part of the console's build output. This server is running
    without it &mdash; either <code>[server] static_dir</code> is not configured,
    or the directory it points at does not contain
    <code>{SCALAR_BUNDLE_PATH}</code>.
</p>
<p>
    Build the console (<code>pnpm --dir ui install &amp;&amp; pnpm --dir ui run
    build</code>) and point <code>static_dir</code> at <code>ui/dist</code>.
</p>
<p>
    The OpenAPI document itself is unaffected: it is embedded in this page, so
    <code>curl</code> on this URL still yields it, and
    <code>batlehub dump-spec</code> writes it to a file.
</p>
<script id="api-reference" type="application/json">
    $spec
</script>
</body>
</html>
"#
    )
}

/// Return a Scalar API docs service using the provided OpenAPI spec.
///
/// `static_dir` is the configured console directory, if any; it decides which of
/// the two templates is served. See [`SCALAR_BUNDLE_PATH`] for why the bundle is
/// local, and [`scalar_unavailable_html`] for why there is no CDN fallback. The
/// route's `Content-Security-Policy` is applied by
/// [`middleware::protocol_document_csp`], which sends [`API_DOCS_CSP`] on this
/// prefix.
pub fn scalar(
    openapi: utoipa::openapi::OpenApi,
    static_dir: Option<&std::path::Path>,
) -> Scalar<utoipa::openapi::OpenApi> {
    let html = if scalar_bundle_present(static_dir) {
        scalar_html()
    } else {
        scalar_unavailable_html()
    };
    Scalar::with_url("/scalar", openapi).custom_html(html)
}

#[cfg(test)]
mod scalar_tests {
    use super::*;

    /// The finding this replaces: an unpinned, unchecked third-party script on
    /// the origin that holds the console's tokens. Now there is no third-party
    /// script at all.
    #[test]
    fn the_reference_loads_nothing_from_a_third_party() {
        for html in [scalar_html(), scalar_unavailable_html()] {
            assert!(
                !html.contains("//"),
                "no protocol-relative or absolute external URL may appear: {html}"
            );
            assert!(!html.contains("cdn.jsdelivr.net"));
            assert!(!html.contains("scalar.com"));
            assert!(!html.contains("http:") && !html.contains("https:"));
        }
    }

    /// The bundle is same-origin, so it needs no `integrity`/`crossorigin` — and
    /// must not silently regain an absolute `src`.
    #[test]
    fn the_bundle_is_referenced_by_a_root_relative_path() {
        let html = scalar_html();
        assert!(html.contains(&format!(r#"<script src="/{SCALAR_BUNDLE_PATH}">"#)));
        assert!(!html.contains("integrity="));
        assert!(!html.contains("crossorigin="));
    }

    /// `withDefaultFonts` and `proxyUrl` are what stop `fonts.scalar.com` and
    /// `proxy.scalar.com`; both were confirmed in a browser.
    #[test]
    fn the_reference_turns_off_the_two_configurable_third_party_calls() {
        let html = scalar_html();
        assert!(html.contains(r#""withDefaultFonts":false"#));
        assert!(html.contains(r#""proxyUrl":"""#));
    }

    /// `Scalar::to_html` substitutes `$spec`; losing the placeholder would serve
    /// an empty reference that still looks fine. Both templates carry it — the
    /// degraded one deliberately, so `curl` still yields the document.
    #[test]
    fn both_templates_keep_the_spec_placeholder() {
        assert!(scalar_html().contains("$spec"));
        assert!(scalar_unavailable_html().contains("$spec"));
    }

    /// The degraded page must not be a blank or misleading success — it has to
    /// say what is missing and how to fix it.
    #[test]
    fn the_degraded_page_explains_itself() {
        let html = scalar_unavailable_html();
        assert!(html.contains("static_dir"));
        assert!(html.contains(SCALAR_BUNDLE_PATH));
        assert!(
            !html.contains("<script src="),
            "the degraded page must load no script at all"
        );
    }

    /// Absence must be detected, not assumed: an unconfigured `static_dir`, a
    /// directory without the bundle, and a directory with it are three different
    /// answers.
    #[test]
    fn bundle_presence_follows_the_file_not_the_config() {
        let dir = tempfile::tempdir().expect("tempdir");

        assert!(
            !scalar_bundle_present(None),
            "no static_dir means no bundle"
        );
        assert!(
            !scalar_bundle_present(Some(dir.path())),
            "a static_dir without the bundle is not a bundle"
        );

        let bundle = dir.path().join(SCALAR_BUNDLE_PATH);
        std::fs::create_dir_all(bundle.parent().expect("parent")).expect("mkdir");
        std::fs::write(&bundle, b"/* bundle */").expect("write");
        assert!(scalar_bundle_present(Some(dir.path())));
    }

    /// A directory at the bundle's path is not a bundle, and `is_file` is what
    /// makes that true — `exists()` would have been satisfied by it.
    #[test]
    fn a_directory_at_the_bundle_path_is_not_a_bundle() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(SCALAR_BUNDLE_PATH)).expect("mkdir");
        assert!(!scalar_bundle_present(Some(dir.path())));
    }

    /// The service picks its template from the same predicate.
    #[test]
    fn the_service_serves_the_degraded_page_without_a_bundle() {
        let spec = openapi_spec();
        let rendered = scalar(spec, None).to_html();
        assert!(rendered.contains("API reference unavailable"));
        assert!(!rendered.contains("<script src="));
    }
}

/// Configure all application routes on a `UtoipaApp`.
///
/// Static file serving (SPA fallback) is intentionally excluded — register it on
/// the plain `actix_web::App` returned by `split_for_parts()` after this configure
/// call, so that `actix_files::Files` (which is not an `OpenApiFactory`) does not
/// interfere with path collection.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn configure_app(
    proxy_svc: Arc<ProxyService>,
    admin_svc: Arc<AdminService>,
    token_repo: Arc<dyn UserTokenRepository>,
    pool: Option<PgPool>,
    access_config: Arc<tokio::sync::RwLock<AccessConfig>>,
    registry_map: RegistryMap,
    upstream_map: UpstreamMap,
    oidc_sso_flows: Vec<OidcSsoFlow>,
    // Names of the configured OIDC providers — the allow-list `create_token`
    // checks the caller against. Distinct from `oidc_sso_flows`, which holds
    // only the providers that also have `redirect_uri` set for browser login.
    oidc_provider_names: OidcProviderNames,
    // One-time store for in-flight authorization requests: PKCE verifier, nonce,
    // provider and the caller's own CSRF value.
    login_states: Arc<dyn batlehub_core::ports::LoginStateStore>,
    warming_map: WarmingServiceMap,
    // Target registry → the `[[release_imports]]` configured into it
    // (RFC 0021). Empty in a deployment that configures none, which is every
    // deployment until an operator writes the block.
    release_imports: handlers::back_office::ops::release_import::ReleaseImportMap,
    // Where a run's history goes. `None` in every in-process test and in a
    // deployment with no database: a history that is not recorded must not turn
    // a working import into a failed request.
    import_history: handlers::back_office::ops::release_import::ImportHistoryHandle,
    eviction_map: EvictionServiceMap,
    proxy_metrics: Arc<ProxyMetrics>,
    prometheus_handle: Option<PrometheusHandle>,
    sbom_svc: Option<Arc<SbomService>>,
    notification_svc: Option<Arc<services::NotificationService>>,
    notification_store: Arc<dyn batlehub_core::ports::NotificationPort + 'static>,
    notifications_config: Option<batlehub_config::schema::NotificationsConfig>,
    storage_admin_repo: Option<Arc<dyn StorageAdminRepository>>,
    // `[search] readmes`. Hot-reloadable, so an operator can turn prose search
    // off without restarting (RFC 0007-bis §4.1).
    search_config: SearchConfigLock,
) -> impl Fn(&mut UtoipaServiceConfig) + Clone + 'static {
    let audit_client = reqwest::Client::builder()
        .user_agent("batlehub/0.1")
        .build()
        .expect("audit HTTP client");
    // Per-process, and it has to be said in code rather than in a comment:
    // `configure_app` is called from inside `HttpServer::new(move || …)`, which
    // actix invokes **once per worker thread**. A limiter built here plainly got
    // one bucket map per worker, so the 30-attempts-a-minute ceiling was really
    // `30 × num_workers` per IP — 480 on a 16-core box — and actix spreading
    // connections across workers meant no single bucket ever filled first. A
    // process-wide `OnceLock` keeps the one map every worker shares without
    // changing this function's signature or its callers.
    static REFRESH_LIMITER: std::sync::OnceLock<Arc<handlers::auth::oidc::RefreshRateLimiter>> =
        std::sync::OnceLock::new();
    let refresh_limiter = Arc::clone(
        REFRESH_LIMITER
            .get_or_init(|| Arc::new(handlers::auth::oidc::RefreshRateLimiter::default())),
    );
    move |cfg| {
        // RFC 0015 §4.2 — the control-surface verbs are resolved by the engine,
        // and the engine needs the grant hierarchy. Registered once as app data
        // rather than reached through whichever service a handler happens to
        // hold: `require_verb` is now on about thirty admin handlers, and making
        // each of them depend on `ProxyService` to borrow its `hot` field would
        // be a dependency invented by the authorization check rather than by the
        // handler's own work.
        cfg.app_data(web::Data::new(proxy_svc.hot.clone()));
        cfg.app_data(web::Data::new(proxy_svc.clone()));
        cfg.app_data(web::Data::new(admin_svc.clone()));
        cfg.app_data(web::Data::new(token_repo.clone()));
        cfg.app_data(web::Data::new(Arc::clone(&access_config)));
        cfg.app_data(web::Data::new(Arc::clone(&search_config)));
        cfg.app_data(web::Data::new(registry_map.clone()));
        cfg.app_data(web::Data::new(upstream_map.clone()));
        cfg.app_data(web::Data::new(audit_client.clone()));
        cfg.app_data(web::Data::new(oidc_sso_flows.clone()));
        cfg.app_data(web::Data::new(oidc_provider_names.clone()));
        cfg.app_data(web::Data::new(login_states.clone()));
        cfg.app_data(web::Data::new(Arc::clone(&refresh_limiter)));
        cfg.app_data(web::Data::new(warming_map.clone()));
        cfg.app_data(web::Data::new(release_imports.clone()));
        cfg.app_data(web::Data::new(import_history.clone()));
        cfg.app_data(web::Data::new(eviction_map.clone()));
        cfg.app_data(web::Data::new(proxy_metrics.clone()));
        if let Some(ref h) = prometheus_handle {
            cfg.app_data(web::Data::new(h.clone()));
        }
        if let Some(ref p) = pool {
            cfg.app_data(web::Data::new(p.clone()));
        }
        if let Some(ref s) = sbom_svc {
            cfg.app_data(web::Data::new(s.clone()));
        }
        // Always register as Option so handlers can extract without a 500 when disabled.
        cfg.app_data(web::Data::new(notification_svc.clone()));
        cfg.app_data(web::Data::new(notification_store.clone()));
        cfg.app_data(web::Data::new(notifications_config.clone()));
        if let Some(ref r) = storage_admin_repo {
            cfg.app_data(web::Data::new(r.clone()));
        }
        collect_routes(cfg);
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── RegistryHostMap ───────────────────────────────────────────────────────

    /// A validated `AppConfig` with `extra` appended. `[server]` comes last so
    /// `extra` may start with bare keys that land in it.
    fn config(extra: &str) -> batlehub_config::schema::AppConfig {
        let raw = format!(
            r#"
            [database]
            type = "postgresql"
            url = "postgresql://localhost/test"

            [storage]
            type = "filesystem"
            path = "/tmp/batlehub-test"

            [server]
            trusted_proxies = ["10.0.0.0/8"]
{extra}
            "#
        );
        let cfg: batlehub_config::schema::AppConfig = toml::from_str(&raw).expect("parses");
        cfg.validate().expect("valid config");
        cfg
    }

    const WILDCARD: &str = r#"
            [subdomain_routing]
            enabled = true
            base_domain = "hub.example.com"
"#;

    fn npm(name: &str, extra: &str) -> String {
        format!(
            "
            [[registries]]
            type = \"npm\"
            name = \"{name}\"
{extra}"
        )
    }

    #[test]
    fn an_unconfigured_map_is_empty_and_routes_nothing() {
        let map = RegistryHostMap::from_app_config(&config(&npm("npm1", "")));
        assert!(map.is_empty());
        assert_eq!(map.registry_for("npm1.hub.example.com"), None);
        assert_eq!(map.public_url_for("npm1"), None);
        assert!(!map.is_host_only("npm1"));
    }

    #[test]
    fn wildcard_hosts_are_materialised_for_every_registry() {
        let map = RegistryHostMap::from_app_config(&config(&format!(
            "{WILDCARD}{}{}",
            npm("npm1", ""),
            npm("npm2", "")
        )));
        assert!(!map.is_empty());
        assert_eq!(
            map.registry_for("npm1.hub.example.com").as_deref(),
            Some("npm1")
        );
        assert_eq!(
            map.registry_for("npm2.hub.example.com").as_deref(),
            Some("npm2")
        );
    }

    #[test]
    fn explicit_and_wildcard_hosts_both_resolve_to_the_same_registry() {
        let map = RegistryHostMap::from_app_config(&config(&format!(
            "{WILDCARD}{}",
            npm("npm1", "            hosts = [\"npm.acme.io\"]")
        )));
        assert_eq!(map.registry_for("npm.acme.io").as_deref(), Some("npm1"));
        assert_eq!(
            map.registry_for("npm1.hub.example.com").as_deref(),
            Some("npm1")
        );
    }

    #[test]
    fn the_explicit_host_is_the_advertised_public_url() {
        let map = RegistryHostMap::from_app_config(&config(&format!(
            "{WILDCARD}{}",
            npm(
                "npm1",
                "            hosts = [\"npm.acme.io\", \"npm2.acme.io\"]"
            )
        )));
        assert_eq!(
            map.public_url_for("npm1").as_deref(),
            Some("https://npm.acme.io"),
            "the first explicit host wins over the wildcard"
        );
    }

    #[test]
    fn the_wildcard_host_is_advertised_when_there_is_no_explicit_one() {
        let map =
            RegistryHostMap::from_app_config(&config(&format!("{WILDCARD}{}", npm("npm1", ""))));
        assert_eq!(
            map.public_url_for("npm1").as_deref(),
            Some("https://npm1.hub.example.com")
        );
    }

    #[test]
    fn lookups_are_by_normalised_host() {
        let map = RegistryHostMap::from_app_config(&config(&npm(
            "npm1",
            "            hosts = [\"NPM.Acme.io\"]",
        )));
        assert_eq!(map.registry_for("npm.acme.io").as_deref(), Some("npm1"));
        // The caller normalises; a raw header value is deliberately not matched.
        assert_eq!(map.registry_for("NPM.Acme.io:8443"), None);
    }

    #[test]
    fn an_unknown_host_misses() {
        let map =
            RegistryHostMap::from_app_config(&config(&format!("{WILDCARD}{}", npm("npm1", ""))));
        assert_eq!(map.registry_for("hub.example.com"), None);
        assert_eq!(map.registry_for("evil.example.com"), None);
    }

    #[test]
    fn host_only_registries_are_flagged() {
        let map = RegistryHostMap::from_app_config(&config(&format!(
            "{}{}",
            npm(
                "npm1",
                "            hosts = [\"npm.acme.io\"]\n            path_routing = false"
            ),
            npm("npm2", "            hosts = [\"npm2.acme.io\"]")
        )));
        assert!(map.is_host_only("npm1"));
        assert!(!map.is_host_only("npm2"), "path_routing defaults to true");
        assert!(!map.is_host_only("nonexistent"));
    }

    #[test]
    fn replace_from_swaps_every_table() {
        let live = RegistryHostMap::from_app_config(&config(&npm(
            "npm1",
            "            hosts = [\"npm.acme.io\"]",
        )));
        let pending = RegistryHostMap::from_app_config(&config(&npm(
            "npm2",
            "            hosts = [\"npm2.acme.io\"]\n            path_routing = false",
        )));

        live.replace_from(&pending);

        assert_eq!(live.registry_for("npm.acme.io"), None, "old host is gone");
        assert_eq!(live.registry_for("npm2.acme.io").as_deref(), Some("npm2"));
        assert_eq!(
            live.public_url_for("npm2").as_deref(),
            Some("https://npm2.acme.io")
        );
        assert!(live.is_host_only("npm2"));
        assert!(!live.is_host_only("npm1"));
    }
}
