//! Tests spanning `client.rs` (RegistryClient impl) and `models.rs` (XML
//! parsing) — sibling-file layout like composer/conda.

use futures::TryStreamExt;
use mockito::Server;

use batlehub_core::{
    entities::{PackageId, RegistryKind},
    error::CoreError,
    ports::RegistryClient,
};

use super::models::parse_plugin_list;
use super::JetbrainsMarketplaceRegistryClient;

fn pkg(name: &str, version: &str) -> PackageId {
    PackageId::new("jbm", name, version)
}

const LIST_BODY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plugin-repository>
  <category name="Languages">
    <idea-plugin downloads="100" size="2048" date="1700000000000">
      <name>Rust</name>
      <id>org.rust.lang</id>
      <version>1.2.0</version>
      <idea-version since-build="233.0" until-build="241.*"/>
      <vendor email="dev@example.com" url="https://example.com">Example Vendor</vendor>
      <description>Rust language &amp; tooling</description>
      <change-notes>Bug fixes</change-notes>
      <depends>com.intellij.modules.platform</depends>
      <depends>org.toml.lang</depends>
    </idea-plugin>
    <idea-plugin downloads="90" size="1024" date="1600000000000">
      <name>Rust</name>
      <id>org.rust.lang</id>
      <version>1.1.0</version>
      <idea-version since-build="223.0" until-build="233.*"/>
      <vendor>Example Vendor</vendor>
      <description>Older release</description>
    </idea-plugin>
  </category>
</plugin-repository>"#;

// ── parse_plugin_list ─────────────────────────────────────────────────────────

#[test]
fn parse_plugin_list_extracts_all_fields() {
    let entries = parse_plugin_list(LIST_BODY.as_bytes()).unwrap();
    assert_eq!(entries.len(), 2);

    let e = &entries[0];
    assert_eq!(e.id.as_deref(), Some("org.rust.lang"));
    assert_eq!(e.name.as_deref(), Some("Rust"));
    assert_eq!(e.version.as_deref(), Some("1.2.0"));
    assert_eq!(e.vendor.as_deref(), Some("Example Vendor"));
    assert_eq!(e.description.as_deref(), Some("Rust language & tooling"));
    assert_eq!(e.change_notes.as_deref(), Some("Bug fixes"));
    assert_eq!(
        e.depends,
        vec!["com.intellij.modules.platform", "org.toml.lang"]
    );
    assert_eq!(e.since_build.as_deref(), Some("233.0"));
    assert_eq!(e.until_build.as_deref(), Some("241.*"));
    assert_eq!(e.date_ms, Some(1_700_000_000_000));
    assert_eq!(e.size, Some(2048));
}

#[test]
fn parse_plugin_list_idea_version_as_open_element() {
    // Some producers emit <idea-version ...></idea-version> instead of a
    // self-closing tag.
    let xml = r#"<plugin-repository><category>
      <idea-plugin><id>x</id><version>1.0</version>
        <idea-version since-build="241.0" until-build="242.*"></idea-version>
      </idea-plugin></category></plugin-repository>"#;
    let entries = parse_plugin_list(xml.as_bytes()).unwrap();
    assert_eq!(entries[0].since_build.as_deref(), Some("241.0"));
    assert_eq!(entries[0].until_build.as_deref(), Some("242.*"));
}

#[test]
fn parse_plugin_list_empty_repository() {
    let entries = parse_plugin_list(b"<plugin-repository/>").unwrap();
    assert!(entries.is_empty());
}

#[test]
fn parse_plugin_list_truncated_drops_the_unclosed_entry() {
    // quick-xml treats EOF with unclosed elements as end-of-input, not an
    // error; an `<idea-plugin>` whose `End` never arrives is never pushed, so
    // a truncated document yields an empty list (which the client maps to
    // NotFound) rather than a half-parsed entry.
    let entries = parse_plugin_list(b"<plugin-repository><idea-plugin><id>x</id>").unwrap();
    assert!(entries.is_empty());
    // Mismatched tags, by contrast, are a hard parse error.
    assert!(matches!(
        parse_plugin_list(b"<a></b>"),
        Err(CoreError::Registry(_))
    ));
}

// ── resolve_metadata ──────────────────────────────────────────────────────────

#[tokio::test]
async fn resolve_metadata_latest_picks_newest_by_date() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/plugins/list?pluginId=org.rust.lang")
        .with_status(200)
        .with_header("content-type", "application/xml")
        .with_body(LIST_BODY)
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let meta = client
        .resolve_metadata(&pkg("org.rust.lang", "latest"))
        .await
        .unwrap();

    assert_eq!(meta.id.version, "1.2.0");
    assert!(meta.published_at.is_some());
    assert_eq!(meta.extra["resolved_version"], "1.2.0");
    assert_eq!(meta.extra["name"], "Rust");
    assert_eq!(meta.extra["vendor"], "Example Vendor");
    let versions = meta.extra["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["version"], "1.2.0");
    assert_eq!(versions[0]["since_build"], "233.0");
    assert_eq!(versions[0]["until_build"], "241.*");
    assert_eq!(versions[1]["version"], "1.1.0");
}

#[tokio::test]
async fn resolve_metadata_pinned_version() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/plugins/list?pluginId=org.rust.lang")
        .with_status(200)
        .with_body(LIST_BODY)
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let meta = client
        .resolve_metadata(&pkg("org.rust.lang", "1.1.0"))
        .await
        .unwrap();

    assert_eq!(meta.id.version, "1.1.0");
}

#[tokio::test]
async fn resolve_metadata_unknown_version_is_not_found() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/plugins/list?pluginId=org.rust.lang")
        .with_status(200)
        .with_body(LIST_BODY)
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let result = client
        .resolve_metadata(&pkg("org.rust.lang", "9.9.9"))
        .await;
    assert!(matches!(result, Err(CoreError::NotFound(_))));
}

#[tokio::test]
async fn resolve_metadata_404_is_not_found() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/plugins/list?pluginId=missing")
        .with_status(404)
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let result = client.resolve_metadata(&pkg("missing", "latest")).await;
    assert!(matches!(result, Err(CoreError::NotFound(_))));
}

#[tokio::test]
async fn resolve_metadata_empty_repository_is_not_found() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/plugins/list?pluginId=missing")
        .with_status(200)
        .with_body("<plugin-repository/>")
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let result = client.resolve_metadata(&pkg("missing", "latest")).await;
    assert!(matches!(result, Err(CoreError::NotFound(_))));
}

#[tokio::test]
async fn resolve_metadata_malformed_xml_is_registry_error() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/plugins/list?pluginId=broken")
        .with_status(200)
        .with_body("<plugin-repository><oops></plugin-repository>")
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let result = client.resolve_metadata(&pkg("broken", "latest")).await;
    assert!(matches!(result, Err(CoreError::Registry(_))));
}

#[tokio::test]
async fn resolve_metadata_server_error_is_registry_error() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/plugins/list?pluginId=org.rust.lang")
        .with_status(500)
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let result = client
        .resolve_metadata(&pkg("org.rust.lang", "latest"))
        .await;
    assert!(matches!(result, Err(CoreError::Registry(_))));
}

// ── list_versions ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_versions_returns_all_versions_oldest_first() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/plugins/list?pluginId=org.rust.lang")
        .with_status(200)
        .with_body(LIST_BODY)
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let versions = client.list_versions("org.rust.lang").await.unwrap();
    // The upstream body lists 1.2.0 before 1.1.0; the port contract (and cache
    // warming, which takes the tail) wants oldest-first.
    assert_eq!(versions, vec!["1.1.0", "1.2.0"]);
}

#[tokio::test]
async fn list_versions_without_dates_inverts_the_upstream_order() {
    let body = r#"<plugin-repository><category>
      <idea-plugin><id>x</id><version>2.0.0</version></idea-plugin>
      <idea-plugin><id>x</id><version>1.0.0</version></idea-plugin>
    </category></plugin-repository>"#;
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/plugins/list?pluginId=x")
        .with_status(200)
        .with_body(body)
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let versions = client.list_versions("x").await.unwrap();
    assert_eq!(versions, vec!["1.0.0", "2.0.0"]);
}

#[test]
fn warm_artifact_is_the_plugin_sub_coordinate() {
    // Cache warming stores the plugin archive under this coordinate; the
    // `plugin/download` handler reads it back from the same one.
    assert_eq!(
        RegistryKind::JetbrainsMarketplace.warm_artifact(),
        Some(batlehub_core::entities::FetchArtifact::Fixed("plugin"))
    );
}

// ── fetch_artifact ────────────────────────────────────────────────────────────

#[tokio::test]
async fn fetch_artifact_follows_redirect_and_streams() {
    let mut server = Server::new_async().await;
    let cdn_path = "/cdn/files/rust-1.2.0.zip";
    let _dl = server
        .mock(
            "GET",
            "/plugin/download?pluginId=org.rust.lang&version=1.2.0",
        )
        .with_status(302)
        .with_header("location", &format!("{}{}", server.url(), cdn_path))
        .create_async()
        .await;
    let _cdn = server
        .mock("GET", cdn_path)
        .with_status(200)
        .with_body("fake plugin zip")
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let fetched = client
        .fetch_artifact(&pkg("org.rust.lang", "1.2.0"))
        .await
        .unwrap();
    let chunks: Vec<bytes::Bytes> = fetched.stream.try_collect().await.unwrap();
    let content: Vec<u8> = chunks.into_iter().flat_map(|b| b.to_vec()).collect();
    assert_eq!(content, b"fake plugin zip");
}

#[tokio::test]
async fn fetch_artifact_channel_suffix_adds_channel_param() {
    let mut server = Server::new_async().await;
    let _dl = server
        .mock(
            "GET",
            "/plugin/download?pluginId=org.rust.lang&version=1.2.0&channel=eap",
        )
        .with_status(200)
        .with_body("eap build")
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let fetched = client
        .fetch_artifact(&pkg("org.rust.lang", "1.2.0").with_artifact("plugin@eap"))
        .await
        .unwrap();
    let chunks: Vec<bytes::Bytes> = fetched.stream.try_collect().await.unwrap();
    assert_eq!(chunks.concat(), b"eap build");
}

#[tokio::test]
async fn fetch_artifact_file_passthrough_url_shape() {
    let mut server = Server::new_async().await;
    // name/version carry upstream numeric ids verbatim on this path.
    let _dl = server
        .mock("GET", "/files/12345/67890/plugin-1.2.0.zip")
        .with_status(200)
        .with_body("by-file-id")
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let fetched = client
        .fetch_artifact(&pkg("12345", "67890").with_artifact("file/plugin-1.2.0.zip"))
        .await
        .unwrap();
    let chunks: Vec<bytes::Bytes> = fetched.stream.try_collect().await.unwrap();
    assert_eq!(chunks.concat(), b"by-file-id");
}

#[tokio::test]
async fn fetch_artifact_404_is_not_found() {
    let mut server = Server::new_async().await;
    let _dl = server
        .mock("GET", "/plugin/download?pluginId=missing&version=1.0")
        .with_status(404)
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let result = client.fetch_artifact(&pkg("missing", "1.0")).await;
    assert!(matches!(result, Err(CoreError::NotFound(_))));
}

#[tokio::test]
async fn fetch_artifact_unsupported_artifact_is_registry_error() {
    let client =
        JetbrainsMarketplaceRegistryClient::new("http://unused", &Default::default()).unwrap();
    let result = client
        .fetch_artifact(&pkg("org.rust.lang", "1.0").with_artifact("weird"))
        .await;
    assert!(matches!(result, Err(CoreError::Registry(_))));
}

// ── search_packages ───────────────────────────────────────────────────────────

#[tokio::test]
async fn search_packages_happy_path() {
    let mut server = Server::new_async().await;
    let body = r#"{"plugins":[
        {"id":1,"xmlId":"org.rust.lang","name":"Rust","preview":"Rust language support"},
        {"id":2,"xmlId":"org.toml.lang","name":"TOML","preview":null}
    ],"total":2}"#;
    let _m = server
        .mock("GET", "/api/searchPlugins?search=rust&max=20")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body)
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let hits = client.search_packages("rust", 20).await.unwrap();

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].name, "org.rust.lang");
    assert_eq!(hits[0].latest_version, "latest");
    assert_eq!(
        hits[0].description.as_deref(),
        Some("Rust language support")
    );
    assert_eq!(hits[1].name, "org.toml.lang");
}

#[tokio::test]
async fn search_packages_upstream_error_is_empty() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/api/searchPlugins?search=rust&max=20")
        .with_status(503)
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let hits = client.search_packages("rust", 20).await.unwrap();
    assert!(hits.is_empty());
}

// ── The numeric-id alias (`files/{pluginId}/{updateId}/…`) ───────────────────

/// The IDE reads `{"id":1149038,"pluginId":164}` out of
/// `api/search/updates/compatible` and addresses the update by that pair. It has
/// to resolve to the coordinate this server publishes under, or the archive is
/// cached beside itself and a block on `IdeaVIM@2.46.2` never matches.
#[tokio::test]
async fn a_numeric_pair_resolves_to_the_published_coordinate() {
    let mut server = Server::new_async().await;
    let _updates = server
        .mock("GET", "/api/plugins/164/updates")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"[{"id":1149038,"version":"2.46.2"},{"id":1100000,"version":"2.45.0"}]"#)
        .create_async()
        .await;
    let _plugin = server
        .mock("GET", "/api/plugins/164")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"id":164,"xmlId":"IdeaVIM","name":"IdeaVim"}"#)
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let resolved = client
        .canonical_coordinate(&pkg("164", "1149038"))
        .await
        .unwrap()
        .expect("the numeric pair is an alias");
    assert_eq!(resolved.name, "IdeaVIM");
    assert_eq!(resolved.version, "2.46.2");
    assert_eq!(resolved.registry, "jbm");
}

/// The artifact selector is part of the request, not of the spelling, so it
/// survives the translation.
#[tokio::test]
async fn resolving_an_alias_keeps_the_artifact_selector() {
    let mut server = Server::new_async().await;
    let _updates = server
        .mock("GET", "/api/plugins/164/updates")
        .with_status(200)
        .with_body(r#"[{"id":1149038,"version":"2.46.2"}]"#)
        .create_async()
        .await;
    let _plugin = server
        .mock("GET", "/api/plugins/164")
        .with_status(200)
        .with_body(r#"{"xmlId":"IdeaVIM"}"#)
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let mut requested = pkg("164", "1149038");
    requested.artifact = Some("plugin".to_owned());
    let resolved = client
        .canonical_coordinate(&requested)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.artifact.as_deref(), Some("plugin"));
}

/// A published coordinate is already canonical, and answering so must cost no
/// upstream call at all — `mockito` fails the test if one is made.
#[tokio::test]
async fn a_published_coordinate_is_already_canonical() {
    let server = Server::new_async().await;
    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    assert!(client
        .canonical_coordinate(&pkg("org.rust.lang", "1.2.0"))
        .await
        .unwrap()
        .is_none());
    // Half a pair is not a pair: a plugin whose xmlId happens to be digits is
    // still addressed by version, and a numeric version is not an update id.
    assert!(client
        .canonical_coordinate(&pkg("164", "2.46.2"))
        .await
        .unwrap()
        .is_none());
}

/// An update id the plugin does not have is a `404`, not a bad gateway and not
/// a silent fall-through to a coordinate that means something else.
#[tokio::test]
async fn an_unknown_update_id_is_not_found() {
    let mut server = Server::new_async().await;
    let _updates = server
        .mock("GET", "/api/plugins/164/updates")
        .with_status(200)
        .with_body(r#"[{"id":1149038,"version":"2.46.2"}]"#)
        .create_async()
        .await;

    let client =
        JetbrainsMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
    let err = client
        .canonical_coordinate(&pkg("164", "9999999"))
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)), "got {err:?}");
}
