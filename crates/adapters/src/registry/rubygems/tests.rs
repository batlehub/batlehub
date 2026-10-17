use super::*;
use batlehub_core::ports::RegistryClient;
use client::{parse_gem_yaml, split_gem_stem};
use futures::TryStreamExt;
use mockito::Server;

fn client(url: &str) -> RubyGemsRegistryClient {
    RubyGemsRegistryClient::new(url, &Default::default()).unwrap()
}

#[test]
fn split_gem_stem_simple() {
    assert_eq!(split_gem_stem("rails-7.1.0"), Some(("rails", "7.1.0")));
}

#[test]
fn split_gem_stem_hyphenated_name() {
    assert_eq!(
        split_gem_stem("json-jwt-1.0.0"),
        Some(("json-jwt", "1.0.0"))
    );
}

#[test]
fn split_gem_stem_platform() {
    assert_eq!(
        split_gem_stem("nokogiri-1.10.0-x86_64-linux"),
        Some(("nokogiri", "1.10.0-x86_64-linux"))
    );
}

#[test]
fn split_gem_stem_no_version() {
    assert_eq!(split_gem_stem("rails"), None);
}

#[tokio::test]
async fn list_versions_ok() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/api/v1/versions/rails.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"[{"number":"7.1.0"},{"number":"7.0.8"},{"number":"6.1.7"}]"#)
        .create_async()
        .await;

    let c = client(&server.url());
    let versions = c.list_versions("rails").await.unwrap();
    // Should be reversed to oldest-first
    assert_eq!(versions, vec!["6.1.7", "7.0.8", "7.1.0"]);
}

#[tokio::test]
async fn list_versions_not_found() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/api/v1/versions/unknown-gem.json")
        .with_status(404)
        .create_async()
        .await;

    let c = client(&server.url());
    let versions = c.list_versions("unknown-gem").await.unwrap();
    assert!(versions.is_empty());
}

#[tokio::test]
async fn fetch_artifact_gem_download() {
    let body = b"fake-gem-bytes";
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/gems/rails-7.1.0.gem")
        .with_status(200)
        .with_header("content-type", "application/octet-stream")
        .with_body(body.as_slice())
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("rg", "rails", "7.1.0").with_artifact("gem");
    let fetched = c.fetch_artifact(&pkg).await.unwrap();
    let chunks: Vec<_> = fetched.stream.try_collect().await.unwrap();
    let content: Vec<u8> = chunks.into_iter().flat_map(|b| b.to_vec()).collect();
    assert_eq!(content, body);
}

#[tokio::test]
async fn fetch_artifact_not_found() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/gems/missing-1.0.0.gem")
        .with_status(404)
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("rg", "missing", "1.0.0").with_artifact("gem");
    assert!(matches!(
        c.fetch_artifact(&pkg).await,
        Err(CoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn fetch_artifact_index() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/specs.4.8.gz")
        .with_status(200)
        .with_body(b"gz-data".as_slice())
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("rg", "_index", "specs");
    let fetched = c.fetch_artifact(&pkg).await.unwrap();
    let chunks: Vec<_> = fetched.stream.try_collect().await.unwrap();
    let content: Vec<u8> = chunks.into_iter().flat_map(|b| b.to_vec()).collect();
    assert_eq!(content, b"gz-data");
}

#[tokio::test]
async fn resolve_metadata_parses_gem_info() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/api/v1/gems/rails.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_header("cache-control", "max-age=60")
        .with_body(
            r#"{"name":"rails","version":"7.1.0","created_at":"2023-10-04T12:00:00.000Z","sha":"abc123"}"#,
        )
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("rg", "rails", "info");
    let meta = c.resolve_metadata(&pkg).await.unwrap();
    assert_eq!(meta.checksum.as_deref(), Some("abc123"));
    assert_eq!(meta.cache_control.as_deref(), Some("max-age=60"));
    assert!(meta.published_at.is_some());
}

#[cfg(feature = "local-registry")]
#[test]
fn parse_gem_yaml_basic() {
    let yaml = r#"--- !ruby/object:Gem::Specification
name: rails
version: !ruby/object:Gem::Version
  version: '7.1.0'
platform: ruby
authors:
- David Heinemeier Hansson
summary: Full-stack web application framework.
"#;
    let meta = parse_gem_yaml(yaml).unwrap();
    assert_eq!(meta.name, "rails");
    assert_eq!(meta.version, "7.1.0");
    assert_eq!(meta.platform, "ruby");
    assert_eq!(
        meta.summary.as_deref(),
        Some("Full-stack web application framework.")
    );
    assert_eq!(meta.authors, vec!["David Heinemeier Hansson"]);
}

// ── gemspec dependencies ──────────────────────────────────────────────────

/// Runtime dependencies are captured; development ones are not.
///
/// The compact index carries what an installer resolves, and `:development`
/// gems are not that. Nothing parsed these at all until a local compact index
/// needed them (RFC 0009 §12.15).
#[test]
fn gemspec_dependencies_are_parsed_and_dev_ones_skipped() {
    let yaml = r#"--- !ruby/object:Gem::Specification
name: mygem
version: !ruby/object:Gem::Version
  version: 1.0.0
platform: ruby
authors:
- me
dependencies:
- !ruby/object:Gem::Dependency
  name: rake
  requirement: !ruby/object:Gem::Requirement
    requirements:
    - - "~>"
      - !ruby/object:Gem::Version
        version: '13.0'
  type: :runtime
- !ruby/object:Gem::Dependency
  name: json
  requirement: !ruby/object:Gem::Requirement
    requirements:
    - - ">="
      - !ruby/object:Gem::Version
        version: '2.0'
    - - "<"
      - !ruby/object:Gem::Version
        version: '3.0'
  type: :runtime
- !ruby/object:Gem::Dependency
  name: rspec
  requirement: !ruby/object:Gem::Requirement
    requirements:
    - - ">="
      - !ruby/object:Gem::Version
        version: '0'
  type: :development
summary: s
"#;
    let meta = super::client::parse_gem_yaml(yaml).unwrap();
    let deps: Vec<(String, String)> = meta
        .dependencies
        .iter()
        .map(|d| (d.name.clone(), d.requirement.clone()))
        .collect();
    assert_eq!(
        deps,
        vec![
            ("rake".to_owned(), "~> 13.0".to_owned()),
            ("json".to_owned(), ">= 2.0&< 3.0".to_owned()),
        ],
        "runtime deps in order, dev deps dropped"
    );
}

/// `dependencies: []` is written inline, and the key that follows must not be
/// read as part of the block.
#[test]
fn a_gem_with_no_dependencies_parses_to_none() {
    let yaml = r#"--- !ruby/object:Gem::Specification
name: mygem
version: !ruby/object:Gem::Version
  version: 1.0.0
platform: ruby
dependencies: []
description:
summary: s
"#;
    let meta = super::client::parse_gem_yaml(yaml).unwrap();
    assert!(meta.dependencies.is_empty());
}

/// `>= 0` is how a gemspec writes "any version", and the compact index omits it
/// rather than writing a constraint that constrains nothing.
#[test]
fn an_unconstrained_dependency_has_an_empty_requirement() {
    let yaml = r#"--- !ruby/object:Gem::Specification
name: mygem
version: !ruby/object:Gem::Version
  version: 1.0.0
dependencies:
- !ruby/object:Gem::Dependency
  name: thor
  requirement: !ruby/object:Gem::Requirement
    requirements:
    - - ">="
      - !ruby/object:Gem::Version
        version: '0'
  type: :runtime
summary: s
"#;
    let meta = super::client::parse_gem_yaml(yaml).unwrap();
    assert_eq!(meta.dependencies.len(), 1);
    assert_eq!(meta.dependencies[0].name, "thor");
    assert_eq!(meta.dependencies[0].requirement, "");
}

/// A gemspec field that would re-cut a compact-index line is refused at parse
/// time, before anything is stored (found by `fuzz_gem_compact_index`).
#[cfg(feature = "local-registry")]
#[test]
fn a_field_carrying_a_compact_index_delimiter_is_refused() {
    let gem = |name: &str, version: &str, dep: &str, op: &str, req: &str| {
        format!(
            "--- !ruby/object:Gem::Specification\nname: {name}\nversion: !ruby/object:Gem::Version\n  version: {version}\nplatform: ruby\ndependencies:\n- !ruby/object:Gem::Dependency\n  name: {dep}\n  requirement: !ruby/object:Gem::Requirement\n    requirements:\n    - - \"{op}\"\n      - !ruby/object:Gem::Version\n        version: '{req}'\n  type: :runtime\n"
        )
    };
    assert!(parse_gem_yaml(&gem("ok", "1.0.0", "rake", "~>", "13.0")).is_ok());
    for (name, version, dep, op, req) in [
        ("ok", "1.0.0", "x|checksum:00", "~>", "13.0"),
        ("ok", "1.0.0", "a,evil", "~>", "13.0"),
        ("ok", "1.0.0", "a:b", "~>", "13.0"),
        ("ok", "1.0.0", "a b", "~>", "13.0"),
        ("ok", "1.0.0", "rake", "~>", "13.0|checksum:00"),
        ("ok", "1.0.0", "rake", "~>", "1,evil:0"),
        ("ok", "1.0.0", "rake", "~> 1&", "0"),
        ("ok", "1.0.0", "rake", "=~", "1"),
        ("ok", "1.0 x", "rake", "~>", "13.0"),
        ("ok", "1.0|x", "rake", "~>", "13.0"),
        ("o|k", "1.0.0", "rake", "~>", "13.0"),
    ] {
        assert!(
            parse_gem_yaml(&gem(name, version, dep, op, req)).is_err(),
            "{name} {version} {dep} {op} {req} must be refused"
        );
    }
}
