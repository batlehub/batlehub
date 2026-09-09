//! Shared test infrastructure for the split integration test suite.
//! See the module-level docs on any sibling test file for context.
//!
//! `mod common;` is compiled independently into every sibling test binary, and
//! each binary only exercises a subset of these helpers — hence `dead_code` is
//! allowed wholesale here rather than per-item.
#![allow(dead_code, unused_imports)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use actix_web::test::init_service;
use actix_web::App;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use futures::stream;
use utoipa_actix_web::AppExt;

use batlehub_adapters::auth::StaticTokenAuthProvider;
use batlehub_adapters::cache::InMemoryCacheStore;
pub use batlehub_adapters::db::InMemoryUserBlockRepository;
pub use batlehub_adapters::in_memory::InMemoryTeamNamespaceStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStatsHistory,
    InMemoryStorageBackend as InMemoryStorage, NoopArtifactMetaRepository as NoopArtifactMeta,
    NullUserTokenRepository as NullTokenRepository,
};
use batlehub_adapters::local_registry::InMemoryLocalRegistry;
use batlehub_adapters::notification::InMemoryNotificationStore;
pub use batlehub_adapters::rate_limit::InMemoryIpBlockStore;
use batlehub_config::schema::{NotificationsConfig, RegistryMode};
use batlehub_core::entities::{NamespacePackage, TeamNamespace, Visibility};
use batlehub_core::ports::BannerPort;
use batlehub_core::ports::NotificationPort;
use batlehub_core::ports::{
    DocumentKind, IpBlockStore, StatsHistoryRepository, TeamNamespacePort, UserBlockRepository,
    VersionDocument,
};
use batlehub_core::{
    entities::{PackageId, PackageMetadata, Role},
    error::CoreError,
    ports::{
        AuthProvider, CacheStore, FetchedArtifact, LocalRegistryBackend, PackageRepository,
        RegistryClient, StorageBackend, UserTokenRepository,
    },
    rules::{BlockListRule, RbacRule},
    services::{
        new_hot_lock, AdminService, HotConfig, HotConfigLock, LocalRegistryService, ProxyMetrics,
        ProxyService, ReadmeService, RegistryPolicy, SbomService,
    },
};
use batlehub_web::handlers::back_office::ops::eviction::EvictionServiceMap;
use batlehub_web::handlers::back_office::ops::warming::WarmingServiceMap;
use batlehub_web::services::NotificationService;
use batlehub_web::{
    configure_app, new_access_lock, AuthMiddlewareFactory, RegistryModeMap, RepoSignerMap,
};

pub struct FixedRegistry {
    registry_type: String,
}

impl FixedRegistry {
    pub fn new(registry_type: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            registry_type: registry_type.into(),
        })
    }
}

#[async_trait]
impl RegistryClient for FixedRegistry {
    fn registry_type(&self) -> &str {
        &self.registry_type
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Ok(PackageMetadata {
            id: pkg.clone(),
            // Old enough to pass any age gate
            published_at: Some(Utc::now() - chrono::Duration::days(30)),
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::json!({"registry": self.registry_type, "name": pkg.name}),
            cache_control: None,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let body = format!("artifact:{}:{}", self.registry_type, pkg.cache_key());
        let bytes = Bytes::from(body);
        Ok(FetchedArtifact {
            stream: Box::pin(stream::once(async move { Ok::<Bytes, CoreError>(bytes) })),
            cache_control: None,
        })
    }

    /// A minimal but realistically-shaped listing document in whichever
    /// protocol this fixture is standing in for.
    ///
    /// Every one advertises the same three versions — `1.0.0`, `1.1.0` and the
    /// pre-release `2.0.0-beta.1` — so a blocked-versions test reads the same
    /// way across ecosystems and the difference under test is the *document
    /// shape*, not the fixture data. Download URLs point at the *upstream*, so a
    /// test can tell whether the proxy rewrote them.
    /// Upstream search results, so a `must_find` assertion can tell an
    /// implemented collection endpoint from one stubbed to an empty `200`
    /// (RFC 0009 §5.1). The default impl returns an empty list, which is
    /// exactly the signal that cannot distinguish the two.
    async fn search_packages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<batlehub_core::ports::UpstreamPackage>, CoreError> {
        let all = [
            ("fixed-alpha", "1.1.0"),
            ("fixed-beta", "1.0.0"),
            ("other-gamma", "2.0.0"),
        ];
        Ok(all
            .iter()
            .filter(|(name, _)| query.is_empty() || name.contains(query))
            .take(limit)
            .map(|(name, version)| batlehub_core::ports::UpstreamPackage {
                name: (*name).to_owned(),
                latest_version: (*version).to_owned(),
                description: Some(format!("{name} from FixedRegistry")),
            })
            .collect())
    }

    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        let unsupported = || {
            Err(CoreError::NotSupported(format!(
                "{} has no '{kind}' version document",
                self.registry_type
            )))
        };
        match (self.registry_type.as_str(), kind) {
            ("npm", DocumentKind::Versions) => {
                let tarball = |v: &str| {
                    serde_json::json!({
                        "version": v,
                        // Per-version READMEs, because a packument carries them
                        // and the discovery read's whole argument is that one
                        // cached fetch answers both halves of the page
                        // (RFC 0007 §2.3).
                        "readme": format!("# {package} {v}"),
                        "dist": { "tarball": format!("https://upstream.invalid/{package}/-/{package}-{v}.tgz") }
                    })
                };
                Ok(VersionDocument::json(serde_json::json!({
                    "name": package,
                    // In npm's canonical spelling, which is not a URL a browser
                    // opens — the page must show the rewritten form.
                    "repository": { "type": "git", "url": format!("git+https://github.com/acme/{package}.git") },
                    "homepage": format!("https://acme.example/{package}"),
                    "dist-tags": { "latest": "1.1.0", "next": "2.0.0-beta.1" },
                    "versions": {
                        "1.0.0": tarball("1.0.0"),
                        "1.1.0": tarball("1.1.0"),
                        "2.0.0-beta.1": tarball("2.0.0-beta.1"),
                    },
                    "time": {
                        "created": "2020-01-01T00:00:00.000Z",
                        "1.0.0": "2020-01-02T00:00:00.000Z",
                        "1.1.0": "2020-02-01T00:00:00.000Z",
                        "2.0.0-beta.1": "2020-03-01T00:00:00.000Z",
                    }
                })))
            }

            ("nuget", DocumentKind::Versions) => Ok(VersionDocument::json(serde_json::json!({
                "versions": ["1.0.0", "1.1.0", "2.0.0-beta.1"]
            }))),
            ("nuget", DocumentKind::REGISTRATION) => {
                let leaf = |v: &str| {
                    serde_json::json!({
                        "catalogEntry": { "id": package, "version": v }
                    })
                };
                Ok(VersionDocument::json(serde_json::json!({
                    "count": 1,
                    "items": [{
                        "count": 3,
                        "lower": "1.0.0",
                        "upper": "2.0.0-beta.1",
                        "items": [leaf("1.0.0"), leaf("1.1.0"), leaf("2.0.0-beta.1")]
                    }]
                })))
            }

            // Shape follows the package prefix, exactly as the real client's
            // `artifact_url` reads it.
            ("terraform", DocumentKind::Versions) => {
                let entries = serde_json::json!([
                    { "version": "1.0.0" }, { "version": "1.1.0" }, { "version": "2.0.0-beta.1" }
                ]);
                if package.starts_with("providers/") {
                    Ok(VersionDocument::json(serde_json::json!({
                        "id": package, "versions": entries
                    })))
                } else {
                    Ok(VersionDocument::json(serde_json::json!({
                        "modules": [{ "source": package, "versions": entries }]
                    })))
                }
            }

            // The provider *download* document — one platform of one version,
            // a different shape from the listing above (RFC 0009 §12.12).
            // Its three URLs point at the upstream on purpose, like every other
            // download URL in this fixture: repointing them at this host is the
            // handler's job, and a test can only tell it happened if the
            // fixture did not do it first.
            ("terraform", k) if k == DocumentKind::PROVIDER_DOWNLOAD => {
                Ok(VersionDocument::json(serde_json::json!({
                    "protocols": ["5.0"],
                    "os": "linux",
                    "arch": "amd64",
                    "filename": "terraform-provider-aws_1.0.0_linux_amd64.zip",
                    "download_url": "https://upstream.invalid/terraform-provider-aws_1.0.0_linux_amd64.zip",
                    "shasums_url": "https://upstream.invalid/terraform-provider-aws_1.0.0_SHA256SUMS",
                    "shasums_signature_url": "https://upstream.invalid/terraform-provider-aws_1.0.0_SHA256SUMS.sig",
                    "shasum": "0000000000000000000000000000000000000000000000000000000000000000",
                    "signing_keys": { "gpg_public_keys": [] }
                })))
            }

            ("rubygems", DocumentKind::Versions) => Ok(VersionDocument::json(serde_json::json!([
                { "number": "2.0.0-beta.1", "sha": "ccc" },
                { "number": "1.1.0", "sha": "bbb" },
                { "number": "1.0.0", "sha": "aaa" }
            ]))),
            ("rubygems", DocumentKind::GEM) => Ok(VersionDocument::json(serde_json::json!({
                "name": package,
                "version": "1.1.0",
                "sha": "bbb",
                "gem_uri": format!("https://upstream.invalid/gems/{package}-1.1.0.gem"),
                "homepage_uri": "https://example.invalid"
            }))),

            // The compact index — plain text, and the documents Bundler
            // actually resolves from (RFC 0009 §7.3). The same three versions
            // as the JSON APIs above, so one block can be asserted against
            // both and the two cannot silently disagree.
            ("rubygems", DocumentKind::COMPACT_VERSIONS) => Ok(VersionDocument::text(
                "text/plain; charset=utf-8",
                "created_at: 2020-01-01T00:00:00Z\n---\n\
                 rails 1.0.0,1.1.0,2.0.0-beta.1 aaabbbcccdddeeefff0011223344556677\n\
                 rack 1.0.0 99887766554433221100ffeeddccbbaa\n",
            )),
            ("rubygems", DocumentKind::COMPACT_NAMES) => Ok(VersionDocument::text(
                "text/plain; charset=utf-8",
                "---\nrack\nrails\n",
            )),
            ("rubygems", DocumentKind::COMPACT_INFO) => Ok(VersionDocument::text(
                "text/plain; charset=utf-8",
                "---\n\
                 1.0.0 |checksum:aaa\n\
                 1.1.0 rack:>= 1.0|checksum:bbb,ruby:>= 2.5\n\
                 2.0.0-beta.1 |checksum:ccc\n",
            )),

            // Minified, as Packagist actually serves it: `1.1.0` inherits
            // `name`/`license` from `2.0.0-beta.1` and `1.0.0` inherits
            // `require` from `1.1.0`, so a naive middle-entry removal corrupts
            // what the entries after it mean.
            ("composer", DocumentKind::Versions) => Ok(VersionDocument::json(serde_json::json!({
                "minified": "composer/2.0",
                "packages": {
                    package: [
                        { "name": package, "version": "2.0.0-beta.1", "license": ["MIT"],
                          "require": { "php": ">=8.1" },
                          "dist": { "type": "zip", "url": "https://cdn.invalid/2.0.0-beta.1.zip" } },
                        { "version": "1.1.0", "require": { "php": ">=7.4" },
                          "dist": { "type": "zip", "url": "https://cdn.invalid/1.1.0.zip" } },
                        { "version": "1.0.0",
                          "dist": { "type": "zip", "url": "https://cdn.invalid/1.0.0.zip" } }
                    ]
                }
            }))),

            // Keyed by *platform*, not by package: a conda listing is scoped
            // to a subdir and describes the whole channel.
            // The channel summary `conda search` reads: one entry per package,
            // naming its newest release, with no version list (RFC 0009 §7.5).
            ("conda", DocumentKind::CHANNELDATA) => Ok(VersionDocument::json(serde_json::json!({
                "channeldata_version": 1,
                "packages": {
                    "numpy": { "version": "1.1.0", "subdirs": ["linux-64"] },
                    "scipy": { "version": "1.0.0", "subdirs": ["linux-64"] }
                }
            }))),

            ("conda", DocumentKind::Versions | DocumentKind::CURRENT_REPODATA) => {
                Ok(VersionDocument::json(serde_json::json!({
                    "info": { "subdir": package },
                    "packages": {
                        "numpy-1.0.0-py311_0.tar.bz2": { "name": "numpy", "version": "1.0.0" },
                        "numpy-1.1.0-py311_0.tar.bz2": { "name": "numpy", "version": "1.1.0" },
                        "scipy-1.1.0-py311_0.tar.bz2": { "name": "scipy", "version": "1.1.0" }
                    },
                    "packages.conda": {
                        "numpy-1.1.0-py311_0.conda": { "name": "numpy", "version": "1.1.0" }
                    },
                    "repodata_version": 1
                })))
            }

            ("github" | "forgejo" | "gitlab", DocumentKind::Versions) => {
                Ok(VersionDocument::json(serde_json::json!([
                    { "id": 3, "tag_name": "v2.0.0-beta.1" },
                    { "id": 2, "tag_name": "v1.1.0" },
                    { "id": 1, "tag_name": "v1.0.0" }
                ])))
            }

            // The `~dev` variant is a *different document* for the same
            // package — branch aliases rather than tagged releases — so it gets
            // its own arm rather than sharing the tagged one.
            ("composer", DocumentKind::P2_DEV) => Ok(VersionDocument::json(serde_json::json!({
                "packages": {
                    package: [
                        { "name": package, "version": "dev-main",
                          "dist": { "type": "zip", "url": "https://cdn.invalid/dev-main.zip" } }
                    ]
                }
            }))),

            ("maven", DocumentKind::Versions) => Ok(VersionDocument::text(
                "text/xml; charset=utf-8",
                concat!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                    "<metadata>\n",
                    "  <groupId>com.example</groupId>\n",
                    "  <versioning>\n",
                    "    <latest>2.0.0-beta.1</latest>\n",
                    "    <release>1.1.0</release>\n",
                    "    <versions>\n",
                    "      <version>1.0.0</version>\n",
                    "      <version>1.1.0</version>\n",
                    "      <version>2.0.0-beta.1</version>\n",
                    "    </versions>\n",
                    "  </versioning>\n",
                    "</metadata>",
                ),
            )),

            ("pypi", DocumentKind::Versions) => Ok(VersionDocument::text(
                "text/html; charset=utf-8",
                format!(
                    concat!(
                        "<!DOCTYPE html>\n<html><body>\n",
                        "<a href=\"https://files.invalid/{p}-1.0.0.tar.gz\">{p}-1.0.0.tar.gz</a><br/>\n",
                        "<a href=\"https://files.invalid/{p}-1.1.0.tar.gz\">{p}-1.1.0.tar.gz</a><br/>\n",
                        "<a href=\"https://files.invalid/{p}-2.0.0b1-py3-none-any.whl\">{p}-2.0.0b1-py3-none-any.whl</a><br/>\n",
                        "</body></html>\n",
                    ),
                    p = package
                ),
            )),
            ("pypi", DocumentKind::SIMPLE_JSON) => Ok(VersionDocument {
                content_type: "application/vnd.pypi.simple.v1+json".to_owned(),
                body: batlehub_core::ports::DocumentBody::Json(serde_json::json!({
                    "name": package,
                    "versions": ["1.0.0", "1.1.0", "2.0.0b1"],
                    "files": [
                        { "filename": format!("{package}-1.0.0.tar.gz"),
                          "url": format!("https://files.invalid/{package}-1.0.0.tar.gz") },
                        { "filename": format!("{package}-1.1.0.tar.gz"),
                          "url": format!("https://files.invalid/{package}-1.1.0.tar.gz") },
                        { "filename": format!("{package}-2.0.0b1-py3-none-any.whl"),
                          "url": format!("https://files.invalid/{package}-2.0.0b1-py3-none-any.whl") }
                    ]
                })),
                synthesised: None,
            }),

            ("cargo", DocumentKind::Versions) => Ok(VersionDocument::text(
                "text/plain; charset=utf-8",
                format!(
                    concat!(
                        r#"{{"name":"{p}","vers":"1.0.0","deps":[],"cksum":"aaa","yanked":false}}"#,
                        "\n",
                        r#"{{"name":"{p}","vers":"1.1.0","deps":[],"cksum":"bbb","yanked":false}}"#,
                        "\n",
                    ),
                    p = package
                ),
            )),

            ("goproxy", DocumentKind::Versions) => Ok(VersionDocument::text(
                "text/plain; charset=utf-8",
                "v1.0.0\nv1.1.0\nv2.0.0-beta.1\n",
            )),
            ("goproxy", DocumentKind::LATEST) => Ok(VersionDocument::json(serde_json::json!({
                "Version": "v1.1.0",
                "Time": "2020-02-01T00:00:00Z"
            }))),

            // The `nodejs.org/dist` release table, in both encodings, with the
            // tree's own spellings: `v`-prefixed versions, newest first, eleven
            // columns, `-` for "no LTS codename" in the TSV and `false` in the
            // JSON. The same three versions as everywhere else; `v1.1.0` and
            // `v1.0.0` share an LTS line so a block on the newer one visibly
            // moves the alias nvm derives from the `lts` column.
            ("nodedist", DocumentKind::Versions) => Ok(VersionDocument::text(
                "text/plain; charset=utf-8",
                "version\tdate\tfiles\tnpm\tv8\tuv\tzlib\topenssl\tmodules\tlts\tsecurity\n\
                 v2.0.0-beta.1\t2020-03-01\theaders,linux-x64,src\t7.0.0\t9.0\t1.40.0\t1.2.11\t1.1.1\t90\t-\t-\n\
                 v1.1.0\t2020-02-01\theaders,linux-x64,src\t6.14.0\t8.4\t1.34.0\t1.2.11\t1.1.1\t83\tArgon\t-\n\
                 v1.0.0\t2020-01-02\theaders,linux-x64,src\t6.13.0\t8.4\t1.34.0\t1.2.11\t1.1.1\t83\tArgon\ttrue\n",
            )),
            ("nodedist", DocumentKind::INDEX_JSON) => Ok(VersionDocument::json(serde_json::json!([
                { "version": "v2.0.0-beta.1", "date": "2020-03-01", "files": ["headers", "linux-x64", "src"],
                  "npm": "7.0.0", "lts": false, "security": false },
                { "version": "v1.1.0", "date": "2020-02-01", "files": ["headers", "linux-x64", "src"],
                  "npm": "6.14.0", "lts": "Argon", "security": false },
                { "version": "v1.0.0", "date": "2020-01-02", "files": ["headers", "linux-x64", "src"],
                  "npm": "6.13.0", "lts": "Argon", "security": true }
            ]))),

            // SDKMAN's text documents (RFC 0010 phase 6). The same three
            // versions as everywhere else. `java` renders the vendor-table
            // layout of `sdk list` and every other candidate the grid, so
            // both filters are exercised; the rendered list echoes the package
            // string on a comment line so a test can see the query the
            // handler forwarded. Relayed documents answer by path.
            ("sdkman", DocumentKind::Versions) => Ok(VersionDocument::text(
                "text/plain; charset=utf-8",
                "1.0.0,1.1.0,2.0.0-beta.1",
            )),
            ("sdkman", DocumentKind::SDKMAN_DEFAULT) => Ok(VersionDocument::text(
                "text/plain; charset=utf-8",
                "1.1.0",
            )),
            ("sdkman", DocumentKind::SDKMAN_VERSIONS_LIST) => {
                let body = if package.starts_with("java/") {
                    format!(
                        "# {package}\n\
                         ================================================================================\n\
                         Available Java Versions for Linux 64bit\n\
                         ================================================================================\n\
                         \x20Vendor         | Use | Version            | Identifier\n\
                         --------------------------------------------------------------------------------\n\
                         \x20Fixture        |     | 2.0.0-beta.1       | 2.0.0-beta.1\n\
                         \x20               |     | 1.1.0              | 1.1.0\n\
                         \x20               |     | 1.0.0              | 1.0.0\n\
                         ================================================================================\n"
                    )
                } else {
                    format!(
                        "# {package}\n\
                         ================================================================================\n\
                         Available Fixture Versions\n\
                         ================================================================================\n\
                         \x20    2.0.0-beta.1        1.1.0               1.0.0\n\
                         ================================================================================\n"
                    )
                };
                Ok(VersionDocument::text("text/plain; charset=utf-8", body))
            }
            ("sdkman", DocumentKind::RELAYED) => {
                let body = match package {
                    "candidates/all" => "fixture,java,maven".to_owned(),
                    "candidates/list" => "Available Candidates\nfixture (1.1.0)\n".to_owned(),
                    "healthcheck" => "000000000000000000000000".to_owned(),
                    p if p.starts_with("candidates/validate/") => "valid".to_owned(),
                    p if p.starts_with("hooks/") => {
                        format!("#!/bin/bash\n#Hook: {p}\nfunction __sdkman_post_installation_hook {{ :; }}\n")
                    }
                    p => format!("relayed:{p}"),
                };
                Ok(VersionDocument::text("text/plain; charset=utf-8", body))
            }

            _ => unsupported(),
        }
    }
}
/// The bound every in-process test app satisfies.
///
/// Spelling the `actix_web::dev::Service<…>` triple out is six lines of `where`
/// clause, it is identical every time, and a helper that takes an app needs it
/// — so it lives here once and the helpers name it instead.
pub trait TestService:
    actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
>
{
}

impl<S> TestService for S where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >
{
}

/// `GET uri` carrying the admin token — the request nearly every read-path
/// test starts with.
pub fn admin_get(uri: &str) -> actix_http::Request {
    actix_web::test::TestRequest::get()
        .uri(uri)
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request()
}

/// The JSON body of `GET uri`, read as the admin.
pub async fn get_json<S: TestService>(app: &S, uri: &str) -> serde_json::Value {
    let resp = actix_web::test::call_service(app, admin_get(uri)).await;
    actix_web::test::read_body_json(resp).await
}

/// The text body of `GET uri`, read as the admin.
///
/// For the protocols whose listings are not JSON: cargo's NDJSON sparse index,
/// goproxy's `@v/list`, a PyPI simple page.
pub async fn get_text<S: TestService>(app: &S, uri: &str) -> String {
    String::from_utf8(get_bytes(app, uri).await).expect("the response body is UTF-8")
}

/// The raw body of `GET uri`, read as the admin — for the compressed listings
/// (conda's zstd `repodata.json.zst`) and for artifacts.
pub async fn get_bytes<S: TestService>(app: &S, uri: &str) -> Vec<u8> {
    let resp = actix_web::test::call_service(app, admin_get(uri)).await;
    assert_eq!(resp.status(), 200, "{uri} should be served");
    actix_web::test::read_body(resp).await.to_vec()
}

/// `GET uri` answers `200` with a `Content-Type` beginning `prefix`.
///
/// The header is the half of the contract a body assertion cannot see: a
/// client that is handed `application/json` for a plain-text sparse index
/// parses nothing, however correct the bytes are.
pub async fn assert_content_type<S: TestService>(app: &S, uri: &str, prefix: &str) {
    let resp = actix_web::test::call_service(app, admin_get(uri)).await;
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .expect("content-type set")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(ct.starts_with(prefix), "content-type was {ct}");
}

/// A `ProxyService` over in-memory stores serving exactly one registry:
/// `name`, speaking `kind`.
///
/// `policy` is handed the repository so it can build a rule chain against it —
/// [`rbac_policy`] is the usual argument, and the stale-metadata suites pass a
/// closure that tweaks it. The repository and the `LocalRegistryService` come
/// back too, because whatever finishes wiring the app needs both, and the
/// `AdminService` beside it has to share the same store.
pub fn one_registry_proxy(
    name: &str,
    kind: &str,
    policy: impl FnOnce(Arc<dyn PackageRepository>) -> (RegistryPolicy, RbacFixture),
) -> (
    Arc<ProxyService>,
    Arc<dyn PackageRepository>,
    Arc<LocalRegistryService>,
) {
    let repo: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());
    let registries: HashMap<String, Arc<dyn RegistryClient>> = [(
        name.to_owned(),
        FixedRegistry::new(kind) as Arc<dyn RegistryClient>,
    )]
    .into();
    let policies: HashMap<String, Arc<RegistryPolicy>> =
        [(name.to_owned(), Arc::new(policy(repo.clone()).0))].into();
    // Permissive, and safe *here* specifically: `one_registry_proxy` takes a
    // closure rather than a named fixture, so its permissions cannot be
    // recovered — and its only user (`vuln_proxy_endpoints.rs`) asserts no
    // denial at all. Anywhere that does assert one uses
    // `local_only_app_parts_with_policy`, which derives grants from the same
    // permissions its rule chain was built from.
    let grants = [(name.to_owned(), Arc::new(permissive_grants(name, kind)))].into();
    let hot = new_hot_lock(HotConfig {
        // RFC 0015 §4.2's instance tier, wired exactly as production wires it:
        // `instance_node` is §10 rule 5's own translation, so the fixture's admin
        // holds the control verbs and nobody else does. Without it every
        // `require_verb` on a control endpoint refuses, including the admin the
        // suite is asserting about — a fixture that does not build the model
        // tests a server nobody runs (§13.5).
        instance: Some(std::sync::Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        registries,
        policies,
        grants,
        ..Default::default()
    });
    let local_svc = make_local_svc(hot.clone(), storage.clone());
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage,
        cache,
        repo: repo.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[name.to_owned()])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    });
    (proxy_svc, repo, local_svc)
}

/// A single-registry app whose upstream for `name` is `upstream_url`.
///
/// For the endpoints that *forward* rather than answer — npm's audit bulk, the
/// NuGet vulnerability index — where the assertion is about what reaches the
/// upstream and what comes back.
pub async fn upstream_forwarding_app(
    name: &str,
    kind: &str,
    upstream_url: String,
    policy: impl FnOnce(Arc<dyn PackageRepository>) -> (RegistryPolicy, RbacFixture),
) -> impl TestService {
    let (proxy_svc, repo, local_svc) = one_registry_proxy(name, kind, policy);
    let upstream_map =
        batlehub_web::UpstreamMap::from(HashMap::from([(name.to_owned(), upstream_url)]));
    finish_test_app(
        proxy_svc,
        Arc::new(AdminService::new(repo)),
        Arc::new(NullTokenRepository),
        access_config_for(&[name]),
        registry_map_for(&[(name, kind)]),
        local_svc,
        RegistryModeMap::default(),
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults {
            upstream_map,
            ..Default::default()
        },
        test_auth_providers(),
    )
    .await
}

/// An app serving exactly one registry: `registry`, of `kind`, in `mode`.
pub async fn registry_app(registry: &str, kind: &str, mode: RegistryMode) -> impl TestService {
    // The cargo index route treats an absent entry as "no cargo registry
    // configured" and 404s before it authorises anything, so that one kind
    // needs a map for its routes to be reachable at all.
    let cargo_indexes = if kind == "cargo" {
        cargo_index_map(registry)
    } else {
        batlehub_web::CargoIndexMap::default()
    };
    build_local_registry_app(
        local_registry_app_parts(registry, kind, mode, None),
        cargo_indexes,
        None,
    )
    .await
}

/// [`registry_app`] in proxy mode — what most of the read-path suites want.
pub async fn proxy_registry_app(registry: &str, kind: &str) -> impl TestService {
    registry_app(registry, kind, RegistryMode::Proxy).await
}

/// Block `name@version` through the admin API, as an operator would.
///
/// Deliberately goes through the HTTP endpoint rather than seeding the
/// repository: the blocked-versions tests are about what a block *does* on the
/// read paths, and a block that only exists because a test wrote it directly
/// would not prove the admin path and the listing path agree on the coordinate.
pub async fn block_version<S: TestService>(app: &S, registry: &str, name: &str, version: &str) {
    let req = actix_web::test::TestRequest::post()
        .uri("/api/v1/admin/packages/block")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": registry,
            "name": name,
            "version": version,
            "reason": "CVE-2024-0001",
        }))
        .to_request();
    let resp = actix_web::test::call_service(app, req).await;
    assert!(
        resp.status().is_success(),
        "blocking {name}@{version} failed: {}",
        resp.status()
    );
}

/// A `CargoIndexMap` naming `registry` as having a sparse index.
///
/// The cargo index route treats an absent entry as "no cargo registry
/// configured" and 404s before it authorises anything, so a test that wants to
/// exercise the index needs one. The URL is never dialled: the fetch goes
/// through `FixedRegistry::fetch_version_document`, which is the point of the
/// route having moved behind `ProxyService`.
pub fn cargo_index_map(registry: &str) -> batlehub_web::CargoIndexMap {
    batlehub_web::CargoIndexMap::new(HashMap::from([(
        registry.to_owned(),
        batlehub_web::CargoIndexProxy {
            http: reqwest::Client::new(),
            index_url: "https://index.invalid".to_owned(),
        },
    )]))
}

pub const ADMIN_TOKEN: &str = "admin-token";
pub const USER_TOKEN: &str = "user-token";
pub const TEAM_A_TOKEN: &str = "team-a-token";
pub const TEAM_B_TOKEN: &str = "team-b-token";
pub const TEAM_AB_TOKEN: &str = "team-ab-token";

pub fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}
/// `AccessConfig` for registries reachable anonymously and by user/admin.
///
/// **Explore follows proxy access here, because that is what a server does.**
/// `rbac.explore.{anonymous,user,admin}` all default to `true`, so
/// `build_access_config` puts a registry in the explore set of every tier that
/// can reach it unless an operator says otherwise — an unconfigured deployment
/// browses everything it can proxy. This helper used to leave the three explore
/// sets empty, which is not a weaker default but a *different* server: one where
/// no registry is browsable by anyone. That was invisible for as long as the
/// catalogue's only reader was `ExploreFilter::registries`, where an empty
/// vector means "no restriction" rather than "nothing"; it stopped being
/// invisible once endpoints began refusing on the set itself, and a suite that
/// wants explore denied should build the denial explicitly rather than inherit
/// it from a helper's silence.
pub fn access_config(anonymous: &[&str], user_admin: &[&str]) -> batlehub_web::AccessConfigLock {
    let to_set = |names: &[&str]| -> std::collections::HashSet<String> {
        names.iter().map(ToString::to_string).collect()
    };
    new_access_lock(batlehub_web::AccessConfig {
        anonymous: to_set(anonymous),
        user: to_set(user_admin),
        admin: to_set(user_admin),
        groups: std::collections::HashMap::new(),
        explore_anonymous: to_set(anonymous),
        explore_user: to_set(user_admin),
        explore_admin: to_set(user_admin),
    })
}

/// `AccessConfig` granting anonymous/user/admin access to exactly `names`,
/// with empty groups.
pub fn access_config_for(names: &[&str]) -> batlehub_web::AccessConfigLock {
    access_config(names, names)
}

/// [`access_config_for`], spelled out.
///
/// Identical to it now that [`access_config`] no longer leaves the explore sets
/// empty. Kept as its own name because the suites that call it — README search,
/// the catalogue scopes — are the ones that read the explore set *directly*, and
/// the call site saying so is worth a line of indirection.
pub fn access_config_with_explore(names: &[&str]) -> batlehub_web::AccessConfigLock {
    let set: std::collections::HashSet<String> = names.iter().map(ToString::to_string).collect();
    new_access_lock(batlehub_web::AccessConfig {
        anonymous: set.clone(),
        user: set.clone(),
        admin: set.clone(),
        groups: std::collections::HashMap::new(),
        explore_anonymous: set.clone(),
        explore_user: set.clone(),
        explore_admin: set,
    })
}
/// Full proxy access to `names`, and no explore access to any of them.
///
/// What `[registries.rbac.explore] anonymous = false, user = false, admin =
/// false` produces: a registry package managers may pull from and the console
/// may not browse. Spelled out here rather than left to a helper's default, so a
/// test asserting the catalogue's refusal says which setting it is asserting.
pub fn access_config_explore_denied(names: &[&str]) -> batlehub_web::AccessConfigLock {
    let set: std::collections::HashSet<String> = names.iter().map(ToString::to_string).collect();
    new_access_lock(batlehub_web::AccessConfig {
        anonymous: set.clone(),
        user: set.clone(),
        admin: set,
        groups: std::collections::HashMap::new(),
        explore_anonymous: std::collections::HashSet::new(),
        explore_user: std::collections::HashSet::new(),
        explore_admin: std::collections::HashSet::new(),
    })
}

pub fn registry_map_for(pairs: &[(&str, &str)]) -> batlehub_web::RegistryMap {
    batlehub_web::RegistryMap::from(
        pairs
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect::<std::collections::HashMap<String, String>>(),
    )
}
pub fn test_auth_providers() -> Vec<Arc<dyn AuthProvider>> {
    vec![Arc::new(StaticTokenAuthProvider::new([
        (
            ADMIN_TOKEN.to_owned(),
            Some("admin".to_owned()),
            Role::Admin,
        ),
        (USER_TOKEN.to_owned(), Some("user-1".to_owned()), Role::User),
    ]))]
}
pub fn make_local_svc(
    hot: HotConfigLock,
    storage: Arc<dyn StorageBackend>,
) -> Arc<LocalRegistryService> {
    make_local_svc_with_repo(hot, storage, None)
}

/// `make_local_svc` with the admin package store attached, as `server/src/main.rs`
/// wires it in production.
///
/// Without it the local service cannot see administrative blocks, so version
/// listings would happily advertise a blocked version. Note this does **not**
/// merge the two in-memory stores (`InMemoryLocalRegistry` holds published
/// packages, `InMemoryPackageRepository` holds statuses — see CLAUDE.md); it
/// only lets the local service ask the second one which versions are blocked.
pub fn make_local_svc_with_repo(
    hot: HotConfigLock,
    storage: Arc<dyn StorageBackend>,
    package_repo: Option<Arc<dyn PackageRepository>>,
) -> Arc<LocalRegistryService> {
    make_local_svc_with_readme(hot, storage, package_repo, None)
}

/// [`make_local_svc_with_repo`] with a README store wired in.
///
/// Separate rather than a fourth parameter on the common helper: only the
/// README suite reads the store back, and every other caller would have to pass
/// a `None` that means nothing to it.
pub fn make_local_svc_with_readme(
    hot: HotConfigLock,
    storage: Arc<dyn StorageBackend>,
    package_repo: Option<Arc<dyn PackageRepository>>,
    readme: Option<Arc<ReadmeService>>,
) -> Arc<LocalRegistryService> {
    Arc::new(LocalRegistryService {
        backend: Arc::new(InMemoryLocalRegistry::new()),
        storage,
        hot,
        quota: None,
        ownership: None,
        team_namespace: None,
        sbom: None,
        explore_cache: None,
        package_repo,
        readme,
    })
}

/// The permissions a fixture policy was built from.
///
/// Returned alongside the policy so the grant hierarchy can be derived from the
/// **same** source rather than restated. RFC 0015 phase 3 took `RbacRule` out of
/// the chain that production assembles (§5.1) and put grant resolution in its
/// place — so a fixture that kept building the rule while production resolved
/// grants would go on passing while testing a path nobody runs. That is not a
/// hypothetical: it is what this suite did for the length of one commit, and
/// `authz_matrix.rs` was green throughout.
#[derive(Clone, Default)]
pub struct RbacFixture {
    pub roles: HashMap<Role, Vec<String>>,
    pub groups: HashMap<String, Vec<String>>,
}

/// A registry that grants every verb to everyone.
///
/// For fixtures whose subject is not authorization. Never for one that asserts a
/// denial — under RFC 0015 phase 3 the grant hierarchy is what refuses, so a
/// permissive one turns an authorization test into a test of nothing.
pub fn permissive_grants(name: &str, kind: &str) -> batlehub_core::entities::RegistryGrants {
    use batlehub_core::entities::{
        Action, GrantMap, Node, RegistryGrants, RegistryKind, SubjectMatcher, Tier,
    };
    RegistryGrants {
        kind: kind
            .parse::<RegistryKind>()
            .unwrap_or(RegistryKind::Generic),
        registry: Node::new(
            Tier::Registry,
            format!("registry:{name}"),
            Some(GrantMap::new().grant(SubjectMatcher::Anyone, Action::ALL.to_vec())),
        ),
        namespaces: Vec::new(),
    }
}

/// The registry-tier grant hierarchy a fixture's permissions imply.
///
/// The same `build_grants` production calls, so the two cannot drift.
pub fn fixture_grants(
    name: &str,
    kind: &str,
    mode: &RegistryMode,
    fixture: &RbacFixture,
) -> batlehub_core::entities::RegistryGrants {
    fixture_grants_with_explore(name, kind, mode, fixture, true)
}

/// [`fixture_grants`] with `[registries.rbac.explore]` set for every role.
///
/// RFC 0015 §4.2 — `catalogue:browse` is resolved from grants now, so a fixture
/// that wants "this registry is for package managers, not for browsing" has to
/// say it in the hierarchy rather than only in `AccessConfig`. §10 rule 2's
/// conjunction is what turns the flag into the grant, and `build_grants` applies
/// it, so a fixture built this way and a config built by the server agree by
/// construction.
pub fn fixture_grants_with_explore(
    name: &str,
    kind: &str,
    mode: &RegistryMode,
    fixture: &RbacFixture,
    explore: bool,
) -> batlehub_core::entities::RegistryGrants {
    use batlehub_core::entities::{expand_patterns, RegistryKind, WildcardScope};
    use batlehub_core::services::authz::translate::{
        build_grants, ExploreFlags, RbacSnapshot, WriteMode,
    };

    let expand = |v: &Vec<String>| {
        expand_patterns(v, WildcardScope::Legacy).expect("fixture patterns are valid")
    };
    let get = |r: &Role| fixture.roles.get(r).map(&expand).unwrap_or_default();

    let snapshot = RbacSnapshot {
        anonymous: get(&Role::Anonymous),
        user: get(&Role::User),
        admin: get(&Role::Admin),
        groups: fixture
            .groups
            .iter()
            .map(|(k, v)| (k.clone(), expand(v)))
            .collect(),
        // The fixtures never set `[registries.rbac.explore]`, and its config
        // default is "on for any role with proxy access" — which is what
        // `build_grants`'s conjunction then narrows.
        explore: ExploreFlags {
            anonymous: explore,
            user: explore,
            admin: explore,
        },
    };
    let write_mode = match mode {
        RegistryMode::Proxy => WriteMode::Refuses,
        _ => WriteMode::Accepts,
    };
    build_grants(
        name,
        kind.parse::<RegistryKind>()
            .unwrap_or(RegistryKind::Generic),
        &snapshot,
        None,
        &[],
        write_mode,
        // No shadow: a fixture that served what it should refuse would make
        // every denial assertion in the matrix pass for the wrong reason.
        None,
    )
    .expect("fixture grants build")
}

/// The permissions [`rbac_policy`] builds its rule from.
pub fn rbac_policy_perms() -> RbacFixture {
    let own = |v: &[&str]| -> Vec<String> { v.iter().map(|s| (*s).to_owned()).collect() };
    RbacFixture {
        roles: HashMap::from([
            (Role::Anonymous, own(&["releases:read"])),
            (Role::User, own(&["releases:read", "source:read"])),
            (Role::Admin, vec!["*".to_owned()]),
        ]),
        groups: HashMap::new(),
    }
}

pub fn rbac_policy(repo: Arc<dyn PackageRepository>) -> (RegistryPolicy, RbacFixture) {
    let policy = RegistryPolicy {
        metadata_ttl: Some(Duration::from_secs(300)),
        firewall_only: false,
        serve_stale_metadata: false,
        artifact_ttl: None,
        // No `RbacRule`, mirroring `build_policy`: RFC 0015 §5.1 replaced it
        // with grant resolution, and a fixture that kept it would let the chain
        // supply a denial production gets from grants — so every negative
        // assertion in this suite would pass without testing the new path.
        // `perms` is still what `fixture_grants` derives the hierarchy from.
        rules: vec![Box::new(BlockListRule::new(repo))],
    };
    (policy, rbac_policy_perms())
}

/// Like [`rbac_policy`] but also grants anonymous `source:read`. Use this for
/// tests that isolate the per-package *visibility* axis (public/internal/team)
/// on registries whose reads require `source:read` (e.g. cargo `download`): the
/// registry RBAC then allows the read so visibility is the only gate under test.
/// The permissions [`rbac_policy_anon_source`] builds its rule from.
pub fn rbac_policy_anon_source_perms() -> RbacFixture {
    let own = |v: &[&str]| -> Vec<String> { v.iter().map(|s| (*s).to_owned()).collect() };
    RbacFixture {
        roles: HashMap::from([
            (Role::Anonymous, own(&["releases:read", "source:read"])),
            (Role::User, own(&["releases:read", "source:read"])),
            (Role::Admin, vec!["*".to_owned()]),
        ]),
        groups: HashMap::new(),
    }
}

pub fn rbac_policy_anon_source(repo: Arc<dyn PackageRepository>) -> (RegistryPolicy, RbacFixture) {
    let policy = RegistryPolicy {
        metadata_ttl: Some(Duration::from_secs(300)),
        firewall_only: false,
        serve_stale_metadata: false,
        artifact_ttl: None,
        // No `RbacRule`, mirroring `build_policy`: RFC 0015 §5.1 replaced it
        // with grant resolution, and a fixture that kept it would let the chain
        // supply a denial production gets from grants — so every negative
        // assertion in this suite would pass without testing the new path.
        // `perms` is still what `fixture_grants` derives the hierarchy from.
        rules: vec![Box::new(BlockListRule::new(repo))],
    };
    (policy, rbac_policy_anon_source_perms())
}
/// Like [`rbac_policy`] but grants anonymous **nothing**. Use this for tests
/// that isolate the *rule chain* axis: the package stays at the default `Public`
/// visibility, so `[registries.rbac]` is the only thing that can refuse.
/// The permissions [`rbac_policy_deny_anonymous`] builds its rule from.
pub fn rbac_policy_deny_anonymous_perms() -> RbacFixture {
    let own = |v: &[&str]| -> Vec<String> { v.iter().map(|s| (*s).to_owned()).collect() };
    RbacFixture {
        roles: HashMap::from([
            (Role::Anonymous, own(&[])),
            (Role::User, own(&["releases:read", "source:read"])),
            (Role::Admin, vec!["*".to_owned()]),
        ]),
        groups: HashMap::new(),
    }
}

pub fn rbac_policy_deny_anonymous(
    repo: Arc<dyn PackageRepository>,
) -> (RegistryPolicy, RbacFixture) {
    let policy = RegistryPolicy {
        metadata_ttl: Some(Duration::from_secs(300)),
        firewall_only: false,
        serve_stale_metadata: false,
        artifact_ttl: None,
        // No `RbacRule`, mirroring `build_policy`: RFC 0015 §5.1 replaced it
        // with grant resolution, and a fixture that kept it would let the chain
        // supply a denial production gets from grants — so every negative
        // assertion in this suite would pass without testing the new path.
        // `perms` is still what `fixture_grants` derives the hierarchy from.
        rules: vec![Box::new(BlockListRule::new(repo))],
    };
    (policy, rbac_policy_deny_anonymous_perms())
}
pub struct ConfigureAppDefaults {
    pub upstream_map: batlehub_web::UpstreamMap,
    pub proxy_metrics: Arc<ProxyMetrics>,
    pub sbom_svc: Option<Arc<SbomService>>,
    pub notification_svc: Option<Arc<NotificationService>>,
    pub notification_store: Arc<dyn NotificationPort + 'static>,
    pub notifications_config: Option<NotificationsConfig>,
    pub warming_map: WarmingServiceMap,
    /// The `[[release_imports]]` configured into each registry (RFC 0021).
    /// Empty by default: a test app configures none, which is every deployment
    /// until an operator writes the block.
    pub release_imports: batlehub_web::handlers::back_office::ops::release_import::ReleaseImportMap,
    /// Where a run's history goes (RFC 0021 §6.5). `None` by default: an app
    /// that records no history still imports, and a test that needs the last
    /// run supplies `InMemoryImportHistory`.
    pub import_history:
        batlehub_web::handlers::back_office::ops::release_import::ImportHistoryHandle,
    pub eviction_map: EvictionServiceMap,
    /// The two block stores the *middleware* enforces and `access-check` now
    /// consults (RFC 0004-bis A1). Registered on every test app, empty by
    /// default, so a handler that reads them is exercised rather than 500ing —
    /// and so a test can seed one and assert the simulator changes its answer.
    pub user_block_repo: Arc<dyn UserBlockRepository>,
    pub ip_block_store: Arc<dyn IpBlockStore>,
    /// `[search] readmes`. **Off**, matching the shipped default, so every
    /// existing explore assertion keeps meaning what it meant; a file that is
    /// about prose search turns it on for its own app.
    pub readme_search: bool,
    /// The configured OIDC provider names `POST /api/v1/auth/tokens` accepts.
    /// **Empty by default** — no OIDC configured means nobody mints a PAT, which
    /// is what an app that isn't about token creation should model. The token
    /// suite sets it to whatever its OIDC-style provider calls itself.
    pub oidc_provider_names: batlehub_web::OidcProviderNames,
    /// One-time store for in-flight OIDC logins. Process-local by default; the
    /// SSO suite keeps its own handle so it can seed and inspect entries.
    pub login_states: Arc<dyn batlehub_core::ports::LoginStateStore>,
    /// RFC 0014's audit, registered the way `server_factory` registers it —
    /// only when present, so the admin routes answer `503` without it, as a
    /// proxy-only process does. `None` by default.
    pub upstream_audit: Option<Arc<batlehub_core::services::UpstreamAuditService>>,
    /// Browser-login flows. Empty by default, so `/auth/oidc/*` answers 503 in
    /// every suite that is not about SSO; the SSO suite points one at a mock IdP.
    pub sso_flows: Vec<batlehub_adapters::auth::OidcSsoFlow>,
    /// RFC 0002 (recast): the flag store. `None` builds one over the app's
    /// own access log; a suite that also puts `FlagsRule` in a policy chain
    /// passes the same store here so the rule and the endpoints agree.
    pub advisory_repo: Option<Arc<dyn batlehub_core::ports::AdvisoryRepository>>,
    /// The `[[flag_sources]]` the push endpoint accepts. Empty by default:
    /// no source means every push is a `404`.
    pub flag_sources: batlehub_web::FlagSources,
}

impl Default for ConfigureAppDefaults {
    fn default() -> Self {
        Self {
            upstream_map: batlehub_web::UpstreamMap::default(),
            proxy_metrics: Arc::new(ProxyMetrics::new(&[])),
            sbom_svc: None,
            notification_svc: None,
            notification_store: Arc::new(InMemoryNotificationStore::new()),
            notifications_config: None,
            warming_map: WarmingServiceMap::default(),
            release_imports: Default::default(),
            import_history: None,
            eviction_map: EvictionServiceMap::default(),
            user_block_repo: Arc::new(InMemoryUserBlockRepository::new()),
            ip_block_store: Arc::new(InMemoryIpBlockStore::new()),
            readme_search: false,
            oidc_provider_names: batlehub_web::OidcProviderNames::default(),
            login_states: batlehub_adapters::in_memory::InMemoryLoginStateStore::arc(),
            upstream_audit: None,
            sso_flows: Vec::new(),
            advisory_repo: None,
            flag_sources: batlehub_web::FlagSources::default(),
        }
    }
}
pub fn configure_test_app(
    proxy_svc: Arc<ProxyService>,
    admin_svc: Arc<AdminService>,
    token_repo: Arc<dyn UserTokenRepository>,
    access_config: batlehub_web::AccessConfigLock,
    registry_map: batlehub_web::RegistryMap,
    defaults: ConfigureAppDefaults,
) -> impl Fn(&mut utoipa_actix_web::service_config::ServiceConfig) + Clone + 'static {
    configure_app(
        proxy_svc,
        admin_svc,
        token_repo,
        None,
        access_config,
        registry_map,
        defaults.upstream_map,
        defaults.sso_flows,
        defaults.oidc_provider_names,
        defaults.login_states,
        defaults.warming_map,
        defaults.release_imports,
        defaults.import_history,
        defaults.eviction_map,
        defaults.proxy_metrics,
        None,
        defaults.sbom_svc,
        defaults.notification_svc,
        defaults.notification_store,
        defaults.notifications_config,
        None, // storage_admin_repo
        // Prose search off, matching the shipped default. A file that wants it
        // on builds its own app — leaving it on here would change what every
        // other explore test is asserting about.
        batlehub_web::new_search_lock(defaults.readme_search),
    )
}
#[allow(clippy::too_many_arguments)]
pub async fn finish_test_app(
    proxy_svc: Arc<ProxyService>,
    admin_svc: Arc<AdminService>,
    token_repo: Arc<dyn UserTokenRepository>,
    access_config: batlehub_web::AccessConfigLock,
    registry_map: batlehub_web::RegistryMap,
    local_svc: Arc<LocalRegistryService>,
    mode_map: RegistryModeMap,
    cargo_indexes: batlehub_web::CargoIndexMap,
    defaults: ConfigureAppDefaults,
    auth_providers: Vec<Arc<dyn AuthProvider>>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let user_block_repo = Arc::clone(&defaults.user_block_repo);
    let ip_block_store = Arc::clone(&defaults.ip_block_store);
    let upstream_audit = defaults.upstream_audit.clone();
    // RFC 0002 (recast): the flag store joins the exposure report against
    // this app's own access log, and the push funnel judges over its hot
    // config. Present on every app so the routes answer `403`/`404` rather
    // than `500` for a missing extractor; a test that seeds flags passes its
    // own store as `extra`, which wins.
    let advisory_repo: Arc<dyn batlehub_core::ports::AdvisoryRepository> =
        defaults.advisory_repo.clone().unwrap_or_else(|| {
            Arc::new(
                batlehub_adapters::in_memory::InMemoryAdvisoryRepository::with_events(Arc::clone(
                    &admin_svc.repo,
                )),
            )
        });
    let flag_sources = defaults.flag_sources.clone();
    let flag_svc = Arc::new(batlehub_core::services::FlagService::new(
        Arc::clone(&advisory_repo),
        local_svc.hot.clone(),
    ));
    let (app, _) = App::new()
        .into_utoipa_app()
        .configure(configure_test_app(
            proxy_svc,
            admin_svc,
            token_repo,
            access_config,
            registry_map,
            defaults,
        ))
        .split_for_parts();
    let app = app
        .app_data(actix_web::web::Data::new(user_block_repo))
        .app_data(actix_web::web::Data::new(ip_block_store))
        .app_data(actix_web::web::Data::new(cargo_indexes))
        // RFC 0014: present only when the suite runs the audit, as in
        // production; the handlers extract it as `Option<Data<_>>`.
        .configure(move |cfg| {
            if let Some(audit) = upstream_audit.clone() {
                cfg.app_data(actix_web::web::Data::new(audit));
            }
        })
        // RFC 0018 phase 5: what `backfill` walks. Empty here — the proxy
        // path records nothing in these apps — so a backfill queues nothing
        // and says so, rather than 500ing on a missing extractor.
        .app_data(actix_web::web::Data::new(
            NoopArtifactMeta::arc() as Arc<dyn batlehub_core::ports::ArtifactInventory>
        ))
        // RFC 0017 §4.1 — the grants editor, assembled from the same handles
        // `server_factory` uses. Wired unconditionally so a suite that never
        // touches it pays nothing and a suite that does needs no second factory;
        // the service answers `503` on its own when `hot.grant_repo` is absent.
        .app_data(actix_web::web::Data::new(Arc::new(
            batlehub_core::services::GrantAdminService::new(
                local_svc.hot.clone(),
                Some(
                    Arc::new(batlehub_core::services::BackendVersions(Arc::clone(
                        &local_svc.backend,
                    ))) as Arc<dyn batlehub_core::services::VersionLookup>,
                ),
                local_svc.ownership.clone(),
            ),
        )))
        .app_data(actix_web::web::Data::new(local_svc))
        .app_data(actix_web::web::Data::new(mode_map))
        .app_data(actix_web::web::Data::new(RepoSignerMap::default()))
        .app_data(actix_web::web::Data::new(batlehub_web::VulnDbMap::default()))
        // Empty by default: absence means the `/sumdb/{path}` route answers 404,
        // which is the contract a registry with no checksum database wants
        // (RFC 0009 §7.4). A test that needs one passes it as `extra`.
        .app_data(actix_web::web::Data::new(batlehub_web::SumDbMap::default()))
        // RFC 0015 §6.3's policy store. Present by default rather than
        // opt-in: without it the policy routes answer `500` for a missing
        // extractor, which a test asserting a `403` would read as a pass.
        .app_data(actix_web::web::Data::new(
            batlehub_adapters::in_memory::InMemoryPolicyRepository::new()
                as Arc<dyn batlehub_core::ports::PolicyRepository>,
        ))
        .app_data(actix_web::web::Data::new(
            InMemoryStatsHistory::new() as Arc<dyn StatsHistoryRepository>
        ))
        .app_data(actix_web::web::Data::new(
            batlehub_adapters::in_memory::InMemoryBundleHistory::new()
                as Arc<dyn batlehub_core::ports::BundleHistory>,
        ))
        .app_data(actix_web::web::Data::new(advisory_repo))
        .app_data(actix_web::web::Data::new(flag_svc))
        .app_data(actix_web::web::Data::new(flag_sources))
        .app_data(actix_web::web::Data::new(
            batlehub_web::ExposureConfig::default(),
        ));

    init_service(app.wrap(AuthMiddlewareFactory::new(auth_providers))).await
}
#[allow(clippy::too_many_arguments)]
pub async fn finish_test_app_with_extra<E: 'static>(
    proxy_svc: Arc<ProxyService>,
    admin_svc: Arc<AdminService>,
    token_repo: Arc<dyn UserTokenRepository>,
    access_config: batlehub_web::AccessConfigLock,
    registry_map: batlehub_web::RegistryMap,
    local_svc: Arc<LocalRegistryService>,
    mode_map: RegistryModeMap,
    cargo_indexes: batlehub_web::CargoIndexMap,
    defaults: ConfigureAppDefaults,
    extra: E,
    auth_providers: Vec<Arc<dyn AuthProvider>>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let user_block_repo = Arc::clone(&defaults.user_block_repo);
    let ip_block_store = Arc::clone(&defaults.ip_block_store);
    // RFC 0002 (recast): the flag store joins the exposure report against
    // this app's own access log, and the push funnel judges over its hot
    // config. Present on every app so the routes answer `403`/`404` rather
    // than `500` for a missing extractor; a test that seeds flags passes its
    // own store as `extra`, which wins.
    let advisory_repo: Arc<dyn batlehub_core::ports::AdvisoryRepository> =
        defaults.advisory_repo.clone().unwrap_or_else(|| {
            Arc::new(
                batlehub_adapters::in_memory::InMemoryAdvisoryRepository::with_events(Arc::clone(
                    &admin_svc.repo,
                )),
            )
        });
    let flag_sources = defaults.flag_sources.clone();
    let flag_svc = Arc::new(batlehub_core::services::FlagService::new(
        Arc::clone(&advisory_repo),
        local_svc.hot.clone(),
    ));
    let (app, _) = App::new()
        .into_utoipa_app()
        .configure(configure_test_app(
            proxy_svc,
            admin_svc,
            token_repo,
            access_config,
            registry_map,
            defaults,
        ))
        .split_for_parts();
    let app = app
        .app_data(actix_web::web::Data::new(user_block_repo))
        .app_data(actix_web::web::Data::new(ip_block_store))
        .app_data(actix_web::web::Data::new(cargo_indexes))
        // RFC 0017 §4.1 — the grants editor, assembled from the same handles
        // `server_factory` uses. Wired unconditionally so a suite that never
        // touches it pays nothing and a suite that does needs no second factory;
        // the service answers `503` on its own when `hot.grant_repo` is absent.
        .app_data(actix_web::web::Data::new(Arc::new(
            batlehub_core::services::GrantAdminService::new(
                local_svc.hot.clone(),
                Some(
                    Arc::new(batlehub_core::services::BackendVersions(Arc::clone(
                        &local_svc.backend,
                    ))) as Arc<dyn batlehub_core::services::VersionLookup>,
                ),
                local_svc.ownership.clone(),
            ),
        )))
        .app_data(actix_web::web::Data::new(local_svc))
        .app_data(actix_web::web::Data::new(mode_map))
        .app_data(actix_web::web::Data::new(RepoSignerMap::default()))
        .app_data(actix_web::web::Data::new(batlehub_web::VulnDbMap::default()))
        // Empty by default: absence means the `/sumdb/{path}` route answers 404,
        // which is the contract a registry with no checksum database wants
        // (RFC 0009 §7.4). A test that needs one passes it as `extra`.
        .app_data(actix_web::web::Data::new(batlehub_web::SumDbMap::default()))
        // RFC 0015 §6.3's policy store. Present by default rather than
        // opt-in: without it the policy routes answer `500` for a missing
        // extractor, which a test asserting a `403` would read as a pass.
        .app_data(actix_web::web::Data::new(
            batlehub_adapters::in_memory::InMemoryPolicyRepository::new()
                as Arc<dyn batlehub_core::ports::PolicyRepository>,
        ))
        .app_data(actix_web::web::Data::new(
            batlehub_adapters::in_memory::InMemoryBundleHistory::new()
                as Arc<dyn batlehub_core::ports::BundleHistory>,
        ))
        .app_data(actix_web::web::Data::new(advisory_repo))
        .app_data(actix_web::web::Data::new(flag_svc))
        .app_data(actix_web::web::Data::new(flag_sources))
        .app_data(actix_web::web::Data::new(
            batlehub_web::ExposureConfig::default(),
        ))
        .app_data(actix_web::web::Data::new(extra));

    init_service(app.wrap(AuthMiddlewareFactory::new(auth_providers))).await
}
pub async fn make_app(
    repo: Arc<InMemoryRepo>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    make_app_ext(repo, Arc::new(ProxyMetrics::new(&[]))).await
}

/// `make_app` with the two block stores the middleware enforces and
/// `access-check` consults (RFC 0004-bis A1), so a test can seed a block and
/// assert the simulator's answer changes.
pub async fn make_app_with_blocks(
    user_block_repo: Arc<dyn UserBlockRepository>,
    ip_block_store: Arc<dyn IpBlockStore>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    make_app_with_defaults(
        InMemoryRepo::new(),
        ConfigureAppDefaults {
            user_block_repo,
            ip_block_store,
            ..Default::default()
        },
    )
    .await
}

/// Variant of `make_app` that accepts a caller-supplied `proxy_metrics` so
/// that tests can inspect or mutate counters and verify the stats endpoint.
pub async fn make_app_ext(
    repo: Arc<InMemoryRepo>,
    proxy_metrics: Arc<ProxyMetrics>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    make_app_with_defaults(
        repo,
        ConfigureAppDefaults {
            proxy_metrics,
            ..Default::default()
        },
    )
    .await
}

/// The full-registry test app, with every knob in `ConfigureAppDefaults` open.
pub async fn make_app_with_defaults(
    repo: Arc<InMemoryRepo>,
    defaults: ConfigureAppDefaults,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    make_app_with_defaults_and_access(repo, defaults, None).await
}

/// [`make_app_with_defaults`] with the `AccessConfig` substituted.
///
/// The default fixture grants every registry it wires to every tier, so **"a
/// caller entitled to nothing" was not expressible** — which is a large part of
/// why survey finding 2 (an empty accessible set scoping to the whole
/// catalogue) shipped. `None` keeps the permissive default.
pub async fn make_app_with_defaults_and_access(
    repo: Arc<InMemoryRepo>,
    defaults: ConfigureAppDefaults,
    access: Option<batlehub_web::AccessConfigLock>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let proxy_metrics = Arc::clone(&defaults.proxy_metrics);
    let repo_dyn: Arc<dyn PackageRepository> = repo.clone();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

    let registries: HashMap<String, Arc<dyn RegistryClient>> = [
        (
            "github".to_owned(),
            FixedRegistry::new("github") as Arc<dyn RegistryClient>,
        ),
        (
            "npm".to_owned(),
            FixedRegistry::new("npm") as Arc<dyn RegistryClient>,
        ),
        (
            "cargo".to_owned(),
            FixedRegistry::new("cargo") as Arc<dyn RegistryClient>,
        ),
        (
            "openvsx".to_owned(),
            FixedRegistry::new("openvsx") as Arc<dyn RegistryClient>,
        ),
        (
            "go".to_owned(),
            FixedRegistry::new("goproxy") as Arc<dyn RegistryClient>,
        ),
        (
            "vscode".to_owned(),
            FixedRegistry::new("vscode-marketplace") as Arc<dyn RegistryClient>,
        ),
        (
            "fj".to_owned(),
            FixedRegistry::new("forgejo") as Arc<dyn RegistryClient>,
        ),
        (
            "gl".to_owned(),
            FixedRegistry::new("gitlab") as Arc<dyn RegistryClient>,
        ),
        (
            "jb".to_owned(),
            FixedRegistry::new("jetbrains") as Arc<dyn RegistryClient>,
        ),
        (
            "jbm".to_owned(),
            FixedRegistry::new("jetbrains-marketplace") as Arc<dyn RegistryClient>,
        ),
        // Added for RFC 0009 §5.1's `must_find` conformance case: NuGet's
        // `/v3/query` is the stub that class exists to catch, and asserting it
        // answers needs the registry to exist.
        (
            "nuget".to_owned(),
            FixedRegistry::new("nuget") as Arc<dyn RegistryClient>,
        ),
        (
            "composer".to_owned(),
            FixedRegistry::new("composer") as Arc<dyn RegistryClient>,
        ),
        // RFC 0010: the conformance fixture asserts nvm's request lines reach
        // the nodedist routes, which needs a registry of that kind to exist.
        (
            "nodedist".to_owned(),
            FixedRegistry::new("nodedist") as Arc<dyn RegistryClient>,
        ),
        (
            "sdkman".to_owned(),
            FixedRegistry::new("sdkman") as Arc<dyn RegistryClient>,
        ),
    ]
    .into();

    let policies: HashMap<String, Arc<RegistryPolicy>> = [
        (
            "github".to_owned(),
            Arc::new(rbac_policy(repo_dyn.clone()).0),
        ),
        ("npm".to_owned(), Arc::new(rbac_policy(repo_dyn.clone()).0)),
        (
            "cargo".to_owned(),
            Arc::new(rbac_policy(repo_dyn.clone()).0),
        ),
        (
            "openvsx".to_owned(),
            Arc::new(rbac_policy(repo_dyn.clone()).0),
        ),
        ("go".to_owned(), Arc::new(rbac_policy(repo_dyn.clone()).0)),
        (
            "vscode".to_owned(),
            Arc::new(rbac_policy(repo_dyn.clone()).0),
        ),
        ("fj".to_owned(), Arc::new(rbac_policy(repo_dyn.clone()).0)),
        ("gl".to_owned(), Arc::new(rbac_policy(repo_dyn.clone()).0)),
        ("jb".to_owned(), Arc::new(rbac_policy(repo_dyn.clone()).0)),
        ("jbm".to_owned(), Arc::new(rbac_policy(repo_dyn.clone()).0)),
        (
            "nuget".to_owned(),
            Arc::new(rbac_policy(repo_dyn.clone()).0),
        ),
        (
            "composer".to_owned(),
            Arc::new(rbac_policy(repo_dyn.clone()).0),
        ),
        (
            "nodedist".to_owned(),
            Arc::new(rbac_policy(repo_dyn.clone()).0),
        ),
        (
            "sdkman".to_owned(),
            Arc::new(rbac_policy(repo_dyn.clone()).0),
        ),
    ]
    .into();
    // Every fixture registry gets a hierarchy, derived from the same
    // permissions `rbac_policy` was built from. Not `permissive_grants`: this
    // app backs `admin_access_check.rs`, which asserts that the simulator
    // *denies* — and a permissive hierarchy would make it answer "allow" for
    // every caller, which is the defect RFC 0004-bis B4 records on this exact
    // endpoint.
    let grants = policies
        .keys()
        .map(|n| {
            (
                n.clone(),
                Arc::new(fixture_grants(
                    n,
                    "generic",
                    &RegistryMode::Hybrid,
                    &rbac_policy_perms(),
                )),
            )
        })
        .collect();
    let hot = new_hot_lock(HotConfig {
        // RFC 0015 §4.2's instance tier, wired exactly as production wires it:
        // `instance_node` is §10 rule 5's own translation, so the fixture's admin
        // holds the control verbs and nobody else does. Without it every
        // `require_verb` on a control endpoint refuses, including the admin the
        // suite is asserting about — a fixture that does not build the model
        // tests a server nobody runs (§13.5).
        instance: Some(std::sync::Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        registries,
        policies,
        grants,
        ..Default::default()
    });
    let local_svc = make_local_svc_with_repo(hot.clone(), storage.clone(), Some(repo_dyn.clone()));
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage,
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: proxy_metrics.clone(),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn).with_hot_config(hot.clone()));

    let token_repo: Arc<dyn UserTokenRepository> = Arc::new(NullTokenRepository);
    let access_config = access.unwrap_or_else(|| {
        access_config_for(&[
            "github", "npm", "cargo", "openvsx", "go", "vscode", "fj", "gl", "jb", "jbm", "nuget",
            "composer",
        ])
    });
    let registry_map = registry_map_for(&[
        ("github", "github"),
        ("npm", "npm"),
        ("cargo", "cargo"),
        ("openvsx", "openvsx"),
        ("go", "goproxy"),
        ("vscode", "vscode-marketplace"),
        ("fj", "forgejo"),
        ("gl", "gitlab"),
        ("jb", "jetbrains"),
        ("jbm", "jetbrains-marketplace"),
        ("nuget", "nuget"),
        ("composer", "composer"),
        ("nodedist", "nodedist"),
        ("sdkman", "sdkman"),
    ]);
    let cargo_indexes = batlehub_web::CargoIndexMap::default();
    finish_test_app(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        RegistryModeMap::default(),
        cargo_indexes,
        defaults,
        test_auth_providers(),
    )
    .await
}
pub struct LocalRegistryAppParts {
    pub proxy_svc: Arc<ProxyService>,
    pub admin_svc: Arc<AdminService>,
    pub token_repo: Arc<dyn UserTokenRepository>,
    pub access_config: batlehub_web::AccessConfigLock,
    pub registry_map: batlehub_web::RegistryMap,
    pub local_svc: Arc<LocalRegistryService>,
    pub mode_map: RegistryModeMap,
}

pub fn local_registry_app_parts(
    name: &str,
    registry_type: &str,
    mode: RegistryMode,
    sbom_svc: Option<Arc<SbomService>>,
) -> LocalRegistryAppParts {
    local_registry_app_parts_with_readme(name, registry_type, mode, sbom_svc, None)
}

/// [`local_registry_app_parts`] with a README store wired into both services.
///
/// Both, because the two capture paths are different: publish records through
/// `LocalRegistryService`, a proxied resolve records through `ProxyService`, and
/// a test that wired only one would pass while the other stored nothing.
pub fn local_registry_app_parts_with_readme(
    name: &str,
    registry_type: &str,
    mode: RegistryMode,
    sbom_svc: Option<Arc<SbomService>>,
    readme_svc: Option<Arc<ReadmeService>>,
) -> LocalRegistryAppParts {
    local_registry_app_parts_with_artifact_meta(
        name,
        registry_type,
        mode,
        sbom_svc,
        readme_svc,
        NoopArtifactMeta::arc(),
    )
}

/// [`local_registry_app_parts_with_readme`] with the artifact-meta store
/// supplied — for a suite that wants to see what the proxy *records* about
/// cached artifacts (RFC 0014 §6.2: the local publish path must record
/// nothing, or the upstream audit would probe a version no upstream has).
pub fn local_registry_app_parts_with_artifact_meta(
    name: &str,
    registry_type: &str,
    mode: RegistryMode,
    sbom_svc: Option<Arc<SbomService>>,
    readme_svc: Option<Arc<ReadmeService>>,
    artifact_meta: Arc<dyn batlehub_core::ports::ArtifactMetaRepository>,
) -> LocalRegistryAppParts {
    let repo_dyn: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

    let registries: HashMap<String, Arc<dyn RegistryClient>> = [(
        name.to_owned(),
        FixedRegistry::new(registry_type) as Arc<dyn RegistryClient>,
    )]
    .into();
    let policies: HashMap<String, Arc<RegistryPolicy>> =
        [(name.to_owned(), Arc::new(rbac_policy(repo_dyn.clone()).0))].into();
    let grants = [(
        name.to_owned(),
        Arc::new(fixture_grants(
            name,
            registry_type,
            &mode,
            &rbac_policy_perms(),
        )),
    )]
    .into();
    let hot = new_hot_lock(HotConfig {
        // RFC 0015 §4.2's instance tier, wired exactly as production wires it:
        // `instance_node` is §10 rule 5's own translation, so the fixture's admin
        // holds the control verbs and nobody else does. Without it every
        // `require_verb` on a control endpoint refuses, including the admin the
        // suite is asserting about — a fixture that does not build the model
        // tests a server nobody runs (§13.5).
        instance: Some(std::sync::Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        registries,
        policies,
        grants,
        ..Default::default()
    });
    let local_svc = make_local_svc_with_readme(
        hot.clone(),
        storage.clone(),
        Some(repo_dyn.clone()),
        readme_svc.clone(),
    );
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage,
        cache,
        repo: repo_dyn.clone(),
        artifact_meta,
        // Registered by name, not empty: `ProxyMetrics` silently ignores
        // counters for a registry it has never heard of, so an empty map turns
        // every `record_*` in a test into a no-op and makes assertions on them
        // pass vacuously.
        metrics: Arc::new(ProxyMetrics::new(&[name.to_owned()])),
        sbom: sbom_svc,
        readme: readme_svc,
        discovery: Default::default(),
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn).with_hot_config(hot.clone()));

    let mode_map = RegistryModeMap::default();
    mode_map.insert(name.to_owned(), mode);

    LocalRegistryAppParts {
        proxy_svc,
        admin_svc,
        token_repo: Arc::new(NullTokenRepository),
        access_config: access_config(&[], &[name]),
        registry_map: registry_map_for(&[(name, registry_type)]),
        local_svc,
        mode_map,
    }
}

/// [`LocalRegistryAppParts`] for a registry that has **no upstream client
/// unless it asks for one**.
///
/// [`local_registry_app_parts`] always installs a `FixedRegistry`, which is
/// what the proxy-mode read suites want. A local-only registry has no upstream
/// at all — and a hybrid one must have it, or the fall-through has nothing to
/// fall through to — so `upstream` decides rather than the mode.
pub fn local_only_app_parts(
    name: &str,
    kind: &str,
    mode: RegistryMode,
    upstream: bool,
) -> LocalRegistryAppParts {
    let repo: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

    let mut registries: HashMap<String, Arc<dyn RegistryClient>> = HashMap::new();
    if upstream {
        registries.insert(
            name.to_owned(),
            FixedRegistry::new(kind) as Arc<dyn RegistryClient>,
        );
    }
    let policies: HashMap<String, Arc<RegistryPolicy>> =
        [(name.to_owned(), Arc::new(rbac_policy(repo.clone()).0))].into();
    let grants = [(
        name.to_owned(),
        Arc::new(fixture_grants(name, kind, &mode, &rbac_policy_perms())),
    )]
    .into();
    let hot = new_hot_lock(HotConfig {
        // RFC 0015 §4.2's instance tier, wired exactly as production wires it:
        // `instance_node` is §10 rule 5's own translation, so the fixture's admin
        // holds the control verbs and nobody else does. Without it every
        // `require_verb` on a control endpoint refuses, including the admin the
        // suite is asserting about — a fixture that does not build the model
        // tests a server nobody runs (§13.5).
        instance: Some(std::sync::Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        registries,
        policies,
        grants,
        ..Default::default()
    });
    let local_svc = make_local_svc(hot.clone(), storage.clone());
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage,
        cache,
        repo: repo.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[name.to_owned()])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    });
    let mode_map = RegistryModeMap::default();
    mode_map.insert(name.to_owned(), mode);

    LocalRegistryAppParts {
        proxy_svc,
        admin_svc: Arc::new(AdminService::new(repo)),
        token_repo: Arc::new(NullTokenRepository),
        access_config: access_config(&[], &[name]),
        registry_map: registry_map_for(&[(name, kind)]),
        local_svc,
        mode_map,
    }
}

/// [`local_only_app_parts`] with the registry's rule chain substituted.
///
/// The default fixture policy grants anonymous `releases:read`, which is exactly
/// the wrong thing for a test asking *whether the chain runs at all*: it allows,
/// so a route that skips the chain and a route that runs it answer alike. Pass
/// [`rbac_policy_deny_anonymous`] to isolate the chain, or
/// [`rbac_policy_anon_source`] to isolate per-package visibility instead.
///
/// The policy goes into the one `HotConfig` both services share, so it governs
/// the local read path and the proxy fall-through equally — which is the whole
/// property the authorization matrix measures.
pub async fn local_only_app_parts_with_policy(
    name: &str,
    kind: &str,
    mode: RegistryMode,
    upstream: bool,
    policy: fn(Arc<dyn PackageRepository>) -> (RegistryPolicy, RbacFixture),
) -> LocalRegistryAppParts {
    let mut parts = local_only_app_parts(name, kind, mode.clone(), upstream);
    let repo: Arc<dyn PackageRepository> = InMemoryRepo::new();
    // One call, both halves: the rule chain and the permissions its grant
    // hierarchy is derived from. Deriving them separately is how the two drift.
    let (policy, perms) = policy(repo);
    let policies: HashMap<String, Arc<RegistryPolicy>> =
        [(name.to_owned(), Arc::new(policy))].into();
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.policies = policies;
        hot.grants = [(
            name.to_owned(),
            Arc::new(fixture_grants(name, kind, &mode, &perms)),
        )]
        .into();
    }
    parts.access_config = access_config_for(&[name]);
    parts
}

/// Install RFC 0015 §4.1 policy tiers on an app's shared `HotConfig`.
///
/// The config-declared half — registry and namespace nodes — which is what
/// `server/src/grants.rs::build_policy_tiers` produces from TOML. A test that
/// needs the *stored* half (package and version) calls
/// [`with_policy_repo`] as well and writes through the port.
///
/// Goes into the one `HotConfig` both services share, exactly as
/// `local_only_app_parts_with_policy` does for grants, so the policy governs the
/// local publish path and the proxy fall-through alike.
pub async fn with_policy_tiers(
    parts: &LocalRegistryAppParts,
    registry: &str,
    tiers: batlehub_core::entities::RegistryPolicyTiers,
) {
    let mut hot = parts.proxy_svc.hot.write().await;
    hot.policy_tiers = [(registry.to_owned(), Arc::new(tiers))].into();
}

/// Give an app the package/version policy store, and hand it back so the test
/// can write rows.
///
/// Separate from [`with_policy_tiers`] because the two halves come from
/// different places by design: the config file cannot enumerate packages (§4.1),
/// so those tiers are a repository rather than a block.
pub async fn with_policy_repo(
    parts: &LocalRegistryAppParts,
) -> Arc<batlehub_adapters::in_memory::InMemoryPolicyRepository> {
    let repo = batlehub_adapters::in_memory::InMemoryPolicyRepository::new();
    let mut hot = parts.proxy_svc.hot.write().await;
    hot.policy_repo = Some(Arc::clone(&repo) as Arc<dyn batlehub_core::ports::PolicyRepository>);
    repo
}

/// A registry-tier policy node with `versioning` set, for the enforcement tests.
pub fn versioning_tiers(
    registry: &str,
    kind: batlehub_core::entities::RegistryKind,
    versioning: batlehub_core::entities::VersioningRules,
) -> batlehub_core::entities::RegistryPolicyTiers {
    let mut tiers = batlehub_core::entities::RegistryPolicyTiers::open(kind, registry);
    tiers.registry.versioning = Some(versioning);
    tiers
}

/// Finish wiring a `make_local_<type>_app` factory: configure the routes from `parts`
/// (with the given `cargo_indexes` and optional `sbom_svc`), attach `local_svc`/`mode_map`,
/// and wrap with the standard test auth providers.
pub async fn build_local_registry_app(
    parts: LocalRegistryAppParts,
    cargo_indexes: batlehub_web::CargoIndexMap,
    sbom_svc: Option<Arc<SbomService>>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    build_local_registry_app_with(parts, cargo_indexes, sbom_svc, false).await
}

/// [`build_local_registry_app`], with the whole `ConfigureAppDefaults` supplied.
///
/// For suites that need to configure something the two narrower entry points do
/// not expose — the SSO suite wiring a browser-login flow at a mock identity
/// provider, for instance. Everything else should keep using the narrow ones,
/// so a new default reaches every suite without each of them restating it.
pub async fn build_local_registry_app_with_defaults(
    parts: LocalRegistryAppParts,
    cargo_indexes: batlehub_web::CargoIndexMap,
    defaults: ConfigureAppDefaults,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let LocalRegistryAppParts {
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
    } = parts;

    finish_test_app(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
        cargo_indexes,
        defaults,
        test_auth_providers(),
    )
    .await
}

/// [`build_local_registry_app`], with `[search] readmes` set explicitly.
///
/// A separate entry point rather than a parameter on the common one: prose
/// search is off in every existing suite and should stay off there, so a file
/// that is about it opts in rather than every other file opting out.
pub async fn build_local_registry_app_with(
    parts: LocalRegistryAppParts,
    cargo_indexes: batlehub_web::CargoIndexMap,
    sbom_svc: Option<Arc<SbomService>>,
    readme_search: bool,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let LocalRegistryAppParts {
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
    } = parts;

    finish_test_app(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
        cargo_indexes,
        ConfigureAppDefaults {
            sbom_svc,
            readme_search,
            ..Default::default()
        },
        test_auth_providers(),
    )
    .await
}

/// Common building blocks for a fully-wired test app with no configured registries.
pub struct EmptyAppParts {
    pub proxy_svc: Arc<ProxyService>,
    pub admin_svc: Arc<AdminService>,
    pub token_repo: Arc<dyn UserTokenRepository>,
    pub access_config: batlehub_web::AccessConfigLock,
    pub registry_map: batlehub_web::RegistryMap,
    pub cargo_indexes: batlehub_web::CargoIndexMap,
    pub local_svc: Arc<LocalRegistryService>,
}

pub fn empty_app_parts() -> EmptyAppParts {
    let repo = InMemoryRepo::new();
    let repo_dyn: Arc<dyn PackageRepository> = repo.clone();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());
    // RFC 0015 §4.2's instance tier, as production wires it. `HotConfig::default()`
    // leaves it `None`, which is the right default for the type — a deployment
    // that has written no instance grant grants none — and the wrong fixture for
    // any suite that calls a control endpoint, because every one of them would
    // refuse the admin it is asserting about.
    let hot = new_hot_lock(HotConfig {
        instance: Some(std::sync::Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        ..Default::default()
    });
    let local_svc = make_local_svc_with_repo(hot.clone(), storage.clone(), Some(repo_dyn.clone()));
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage,
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    });
    EmptyAppParts {
        proxy_svc,
        admin_svc: Arc::new(AdminService::new(repo_dyn).with_hot_config(hot.clone())),
        token_repo: Arc::new(NullTokenRepository),
        access_config: access_config_for(&[]),
        registry_map: registry_map_for(&[]),
        cargo_indexes: batlehub_web::CargoIndexMap::default(),
        local_svc,
    }
}

/// Build a test app whose stats-history repository is caller-supplied, so a
/// test can seed rollup rows before reading `/api/v1/admin/stats/history`.
///
/// `make_app` registers a fresh, empty one instead — enough for every test that
/// does not read the series, and the reason those tests need no changes.
pub async fn make_app_with_stats_history(
    history: Arc<dyn StatsHistoryRepository>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let EmptyAppParts {
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        cargo_indexes,
        local_svc,
    } = empty_app_parts();

    // RFC 0015 §4.2 — `/admin/stats/history` is gated on `stats:read` now, and a
    // gate is only assertable against a hierarchy that grants it to somebody. The
    // rollup rows this fixture's callers seed name `npm` and `cargo`, so those are
    // the registries whose grants have to exist; `fixture_grants` derives them
    // from the same permissions production derives them from, which is what stops
    // this from becoming a fixture that tests a path nobody runs (§13.5).
    {
        let mut hot = proxy_svc.hot.write().await;
        hot.grants = ["npm", "cargo"]
            .into_iter()
            .map(|name| {
                (
                    name.to_owned(),
                    Arc::new(fixture_grants(
                        name,
                        name,
                        &RegistryMode::Proxy,
                        &rbac_policy_perms(),
                    )),
                )
            })
            .collect();
    }

    finish_test_app_with_extra(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        RegistryModeMap::default(),
        cargo_indexes,
        ConfigureAppDefaults::default(),
        history,
        test_auth_providers(),
    )
    .await
}

pub async fn make_app_with_ip_store(
    ip_store: Arc<dyn IpBlockStore>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let EmptyAppParts {
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        cargo_indexes,
        local_svc,
    } = empty_app_parts();

    finish_test_app_with_extra(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        RegistryModeMap::default(),
        cargo_indexes,
        ConfigureAppDefaults::default(),
        ip_store,
        test_auth_providers(),
    )
    .await
}

/// Build a test app with a single `npm` registry, exposing the raw storage
/// backend so tests can pre-seed cached artifacts, plus a caller-supplied
/// `eviction_map` so `/admin/registries/{registry}/evict` has something to run.
pub async fn make_app_with_eviction(
    eviction_map: EvictionServiceMap,
) -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    Arc<dyn StorageBackend>,
) {
    make_app_with_eviction_and_repo(eviction_map, InMemoryRepo::new()).await
}

/// [`make_app_with_eviction`] against a caller-supplied repository.
///
/// The repository is where the cache-eviction audit events land, and the
/// default factory builds one it never hands back — so a test about the trail
/// has to own it.
pub async fn make_app_with_eviction_and_repo(
    eviction_map: EvictionServiceMap,
    repo_dyn: Arc<dyn PackageRepository>,
) -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    Arc<dyn StorageBackend>,
) {
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());
    let hot = new_hot_lock(HotConfig {
        // §4.2's instance tier, as production wires it — see `empty_app_parts`.
        instance: Some(std::sync::Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        ..Default::default()
    });
    let local_svc = make_local_svc_with_repo(hot.clone(), storage.clone(), Some(repo_dyn.clone()));
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage: storage.clone(),
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn).with_hot_config(hot.clone()));
    let token_repo: Arc<dyn UserTokenRepository> = Arc::new(NullTokenRepository);
    let access_config = access_config_for(&["npm"]);
    let registry_map = registry_map_for(&[("npm", "npm")]);
    let cargo_indexes = batlehub_web::CargoIndexMap::default();

    let app = finish_test_app(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        RegistryModeMap::default(),
        cargo_indexes,
        ConfigureAppDefaults {
            eviction_map,
            ..Default::default()
        },
        test_auth_providers(),
    )
    .await;

    (app, storage)
}

/// Build a test app with a single `npm` registry, exposing the raw storage
/// backend so tests can assert on what warming stored, plus a caller-supplied
/// `warming_map` so `/admin/registries/{registry}/warm` has something to run.
pub async fn make_app_with_warming(
    warming_map: WarmingServiceMap,
) -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    Arc<dyn StorageBackend>,
) {
    let repo_dyn: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());
    let hot = new_hot_lock(HotConfig {
        // §4.2's instance tier, as production wires it — see `empty_app_parts`.
        instance: Some(std::sync::Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        ..Default::default()
    });
    let local_svc = make_local_svc_with_repo(hot.clone(), storage.clone(), Some(repo_dyn.clone()));
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage: storage.clone(),
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn).with_hot_config(hot.clone()));
    let token_repo: Arc<dyn UserTokenRepository> = Arc::new(NullTokenRepository);
    let access_config = access_config_for(&["npm"]);
    let registry_map = registry_map_for(&[("npm", "npm")]);
    let cargo_indexes = batlehub_web::CargoIndexMap::default();

    let app = finish_test_app(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        RegistryModeMap::default(),
        cargo_indexes,
        ConfigureAppDefaults {
            warming_map,
            ..Default::default()
        },
        test_auth_providers(),
    )
    .await;

    (app, storage)
}

pub async fn make_app_with_notifications(
    notification_svc: Option<Arc<NotificationService>>,
    notification_store: Arc<dyn NotificationPort>,
    notifications_config: Option<NotificationsConfig>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let EmptyAppParts {
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        cargo_indexes,
        local_svc,
    } = empty_app_parts();

    finish_test_app(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        RegistryModeMap::default(),
        cargo_indexes,
        ConfigureAppDefaults {
            notification_svc,
            notification_store,
            notifications_config,
            ..Default::default()
        },
        test_auth_providers(),
    )
    .await
}
/// A minimal RubyGems `.gem`: a tar holding one gzip'd YAML `metadata.gz`.
///
/// `dependencies` is spliced into the YAML verbatim, so a caller that wants a
/// dependency block writes it the way a gemspec does; pass `""` for none.
pub fn make_gem_with_deps(name: &str, version: &str, dependencies: &str) -> Vec<u8> {
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write as _;

    let yaml = format!(
        "name: {name}\nversion:\n  version: '{version}'\nplatform: ruby\n{dependencies}summary: s\n"
    );
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

/// [`make_gem_with_deps`] with no dependency block.
pub fn make_gem(name: &str, version: &str) -> Vec<u8> {
    make_gem_with_deps(name, version, "")
}

pub fn make_publish_payload(name: &str, version: &str) -> Vec<u8> {
    let meta = serde_json::json!({
        "name": name, "vers": version,
        "deps": [], "features": {}, "authors": [],
        "description": null, "documentation": null, "homepage": null,
        "readme": null, "readme_file": null, "keywords": [],
        "categories": [], "license": null, "license_file": null,
        "repository": null, "badges": {}, "links": null
    });
    let meta_bytes = serde_json::to_vec(&meta).unwrap();
    let crate_bytes: &[u8] = b"fake-crate-content";
    let mut buf = Vec::new();
    buf.extend_from_slice(&(meta_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&meta_bytes);
    buf.extend_from_slice(&(crate_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(crate_bytes);
    buf
}

/// Build a test app with a single Cargo registry in the given mode (Local or Hybrid).
/// Registry name is `"local-cargo"`, type `"cargo"`.
/// Auth: ADMIN_TOKEN = admin, USER_TOKEN = user-1 (same as `test_auth_providers`).
pub async fn make_local_registry_app(
    mode: RegistryMode,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    make_local_registry_app_with_sbom(mode, None).await
}

pub async fn make_local_registry_app_with_sbom(
    mode: RegistryMode,
    sbom_svc: Option<Arc<SbomService>>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let parts = local_registry_app_parts("local-cargo", "cargo", mode.clone(), sbom_svc.clone());
    // Hybrid mode requires an upstream index for config.json to succeed.
    // A dummy URL is sufficient — upstream fetches only happen on actual index lookups.
    let mut cargo_map: std::collections::HashMap<String, batlehub_web::CargoIndexProxy> =
        std::collections::HashMap::new();
    if matches!(mode, RegistryMode::Hybrid) {
        cargo_map.insert(
            "local-cargo".to_owned(),
            batlehub_web::CargoIndexProxy {
                http: reqwest::Client::new(),
                index_url: "https://index.crates.io".to_owned(),
            },
        );
    }
    let cargo_indexes = batlehub_web::CargoIndexMap::new(cargo_map);

    build_local_registry_app(parts, cargo_indexes, sbom_svc).await
}
/// [`make_local_registry_app`] for cargo, with an ownership store wired in, and
/// the store handed back so a test can seed and inspect it.
///
/// A separate entry point because the shared factory leaves `ownership: None`,
/// and that is not a neutral default here: with the port absent, the
/// `cargo owner --add`/`--remove` routes return `404` before reaching any
/// authorization at all. Every ownership test written against the shared
/// factory would therefore have passed without exercising a single check —
/// which is exactly how the unauthenticated-claim bug survived to be found by
/// review rather than by CI.
pub async fn make_local_cargo_ownership_app(
    mode: RegistryMode,
) -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    Arc<batlehub_adapters::in_memory::InMemoryOwnershipStore>,
    Arc<batlehub_adapters::in_memory::InMemoryGrantRepository>,
) {
    let mut parts = local_registry_app_parts("local-cargo", "cargo", mode, None);
    let ownership = batlehub_adapters::in_memory::InMemoryOwnershipStore::new();
    // RFC 0015 §10 rule 9 — the same wrapper production wires, for the same
    // reason `fixture_grants` calls the same `build_grants`: a fixture that
    // talked to the bare port would test a path nobody runs, and the projection
    // this covers is one that already went four call sites without being
    // noticed.
    let grant_repo = batlehub_adapters::in_memory::InMemoryGrantRepository::new();
    let owner_port = batlehub_core::services::ownership_grants::OwnershipGrants::wrap(
        ownership.clone() as Arc<dyn batlehub_core::ports::OwnershipPort>,
        grant_repo.clone() as Arc<dyn batlehub_core::ports::GrantRepository>,
    );

    let cur = parts.local_svc.clone();
    parts.local_svc = Arc::new(LocalRegistryService {
        backend: cur.backend.clone(),
        storage: cur.storage.clone(),
        hot: cur.hot.clone(),
        quota: cur.quota.clone(),
        ownership: Some(owner_port),
        team_namespace: cur.team_namespace.clone(),
        sbom: cur.sbom.clone(),
        explore_cache: cur.explore_cache.clone(),
        package_repo: cur.package_repo.clone(),
        readme: cur.readme.clone(),
    });

    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (app, ownership, grant_repo)
}

pub async fn make_local_composer_app(
    mode: RegistryMode,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    build_local_registry_app(
        local_registry_app_parts("local-composer", "composer", mode, None),
        batlehub_web::CargoIndexMap::default(),
        None,
    )
    .await
}

/// A token for a user who is a member of group "team-alpha".
pub const NS_MEMBER_TOKEN: &str = "ns-member-token";
/// A regular user with no group membership.
pub const NS_PLAIN_USER_TOKEN: &str = "ns-plain-user-token";

pub fn team_ns_auth_providers() -> Vec<Arc<dyn AuthProvider>> {
    vec![Arc::new(
        StaticTokenAuthProvider::new([
            (
                ADMIN_TOKEN.to_owned(),
                Some("admin".to_owned()),
                Role::Admin,
            ),
            (
                NS_PLAIN_USER_TOKEN.to_owned(),
                Some("plain-user".to_owned()),
                Role::User,
            ),
        ])
        .with_group_entries([(
            NS_MEMBER_TOKEN.to_owned(),
            Some("member-user".to_owned()),
            Role::User,
            vec!["team-alpha".to_owned()],
        )]),
    )]
}

pub async fn make_local_nuget_app(
    mode: RegistryMode,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let repo_dyn: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

    let mut registries: HashMap<String, Arc<dyn RegistryClient>> = HashMap::new();
    if matches!(mode, RegistryMode::Hybrid) {
        registries.insert(
            "local-nuget".to_owned(),
            FixedRegistry::new("nuget") as Arc<dyn RegistryClient>,
        );
    }
    let policies: HashMap<String, Arc<RegistryPolicy>> = [(
        "local-nuget".to_owned(),
        Arc::new(rbac_policy(repo_dyn.clone()).0),
    )]
    .into();
    // Every fixture registry gets a hierarchy, derived from the same
    // permissions `rbac_policy` was built from. Not `permissive_grants`: this
    // app backs `admin_access_check.rs`, which asserts that the simulator
    // *denies* — and a permissive hierarchy would make it answer "allow" for
    // every caller, which is the defect RFC 0004-bis B4 records on this exact
    // endpoint.
    let grants = policies
        .keys()
        .map(|n| {
            (
                n.clone(),
                Arc::new(fixture_grants(
                    n,
                    "generic",
                    &RegistryMode::Hybrid,
                    &rbac_policy_perms(),
                )),
            )
        })
        .collect();
    let hot = new_hot_lock(HotConfig {
        // RFC 0015 §4.2's instance tier, wired exactly as production wires it:
        // `instance_node` is §10 rule 5's own translation, so the fixture's admin
        // holds the control verbs and nobody else does. Without it every
        // `require_verb` on a control endpoint refuses, including the admin the
        // suite is asserting about — a fixture that does not build the model
        // tests a server nobody runs (§13.5).
        instance: Some(std::sync::Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        registries,
        policies,
        grants,
        ..Default::default()
    });
    let local_svc = make_local_svc_with_repo(hot.clone(), storage.clone(), Some(repo_dyn.clone()));
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage,
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn).with_hot_config(hot.clone()));
    let mode_map = RegistryModeMap::default();
    mode_map.insert("local-nuget".to_owned(), mode);

    let parts = LocalRegistryAppParts {
        proxy_svc,
        admin_svc,
        token_repo: Arc::new(NullTokenRepository),
        access_config: access_config(&[], &["local-nuget"]),
        registry_map: registry_map_for(&[("local-nuget", "nuget")]),
        local_svc,
        mode_map,
    };
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

pub fn make_composer_zip(name: &str, version: &str) -> Vec<u8> {
    use std::io::Write as _;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        writer.start_file("composer.json", opts).unwrap();
        let json = serde_json::json!({
            "name": name,
            "version": version,
            "description": "Test package",
            "type": "library",
        });
        writer.write_all(json.to_string().as_bytes()).unwrap();
        writer.finish().unwrap();
    }
    buf.into_inner()
}
