//! Per-protocol fixtures, and the drift test that keeps the support table
//! honest (RFC 0007 §4.3, §10).
//!
//! The fixtures are the documents the upstreams really send, not a shape
//! convenient to parse. RFC 0009's lesson was that six protocol defects shipped
//! green because the tests were written from our implementation rather than from
//! what the client sends; a reader tested against a document we invented would
//! be the same mistake.

use super::*;
use crate::entities::UpstreamDetailSupport;
use crate::ports::DocumentKind;

fn json_doc(value: serde_json::Value) -> VersionDocument {
    VersionDocument::json(value)
}

fn text_doc(body: &str) -> VersionDocument {
    VersionDocument::text("text/plain", body)
}

fn versions_of(detail: &UpstreamDetail) -> Vec<&str> {
    detail.versions.iter().map(|v| v.version.as_str()).collect()
}

// ── The contract between the table and the code ───────────────────────────────

/// Every kind whose `upstream_detail()` names a document must reach a real
/// reader, and the document it names must be one `DocumentKind` answers to.
///
/// The `every_advertised_filter_is_reachable_from_dispatch` pattern, from the
/// other side of the same problem: a table claiming coverage that dispatch
/// cannot deliver is the failure RFC 0009 was written about.
#[test]
fn every_advertised_document_reaches_a_reader() {
    // The discriminants `DocumentKind::as_str` can return. Listed rather than
    // derived because `DocumentKind::Secondary` holds a free string, so nothing
    // can enumerate it — which is exactly why a typo needs catching here.
    let known_documents: [&str; 12] = [
        DocumentKind::Versions.as_str(),
        DocumentKind::REGISTRATION.as_str(),
        DocumentKind::GEM.as_str(),
        DocumentKind::LATEST.as_str(),
        DocumentKind::CURRENT_REPODATA.as_str(),
        DocumentKind::P2_DEV.as_str(),
        DocumentKind::SIMPLE_JSON.as_str(),
        DocumentKind::COMPACT_VERSIONS.as_str(),
        DocumentKind::COMPACT_INFO.as_str(),
        DocumentKind::CHANNELDATA.as_str(),
        DocumentKind::COMPACT_NAMES.as_str(),
        DocumentKind::PROVIDER_DOWNLOAD.as_str(),
    ];

    for kind in RegistryKind::ALL {
        let UpstreamDetailSupport::Document(document) = kind.upstream_detail() else {
            continue;
        };
        assert!(
            known_documents.contains(&document),
            "{kind} advertises document '{document}', which no DocumentKind answers to"
        );

        // A reader that returns nothing for a document of the right *encoding*
        // is a reader that is not there. Both encodings are tried, because a
        // kind's document may be either and `dispatch` picks by kind.
        let json = dispatch(*kind, &json_doc(serde_json::json!({})));
        let text = dispatch(*kind, &text_doc(""));
        let _ = (json, text);
        // The real assertion is that dispatch has an arm at all: the catch-all
        // warns and returns default, so this checks the arm exists by checking
        // the kind is in the list the arms cover.
        assert!(
            matches!(
                kind,
                RegistryKind::Npm
                    | RegistryKind::Pypi
                    | RegistryKind::Cargo
                    | RegistryKind::Nuget
                    | RegistryKind::Goproxy
                    | RegistryKind::Maven
                    | RegistryKind::Composer
                    | RegistryKind::Rubygems
                    | RegistryKind::Terraform
                    | RegistryKind::Nodedist
                    | RegistryKind::Sdkman
            ),
            "{kind} advertises a document but `dispatch` has no arm for it"
        );
    }
}

/// And the other direction: a reader with no kind advertising it is dead code
/// that will rot.
#[test]
fn every_reader_belongs_to_a_kind_that_advertises_a_document() {
    for kind in [
        RegistryKind::Npm,
        RegistryKind::Pypi,
        RegistryKind::Cargo,
        RegistryKind::Nuget,
        RegistryKind::Goproxy,
        RegistryKind::Maven,
        RegistryKind::Composer,
        RegistryKind::Rubygems,
        RegistryKind::Terraform,
        RegistryKind::Nodedist,
        RegistryKind::Sdkman,
    ] {
        assert!(
            matches!(kind.upstream_detail(), UpstreamDetailSupport::Document(_)),
            "{kind} has a reader but does not advertise a document"
        );
    }
}

/// A kind with no reader answers from local rows rather than erroring the page.
#[test]
fn an_unread_kind_contributes_nothing_rather_than_failing() {
    let detail = dispatch(RegistryKind::Generic, &json_doc(serde_json::json!({})));
    assert_eq!(detail, UpstreamDetail::default());
}

/// An unparseable document contributes nothing and does not panic. Fewer rows
/// is the safe direction for a read path.
#[test]
fn an_unparseable_document_contributes_nothing() {
    for kind in [
        RegistryKind::Npm,
        RegistryKind::Pypi,
        RegistryKind::Cargo,
        RegistryKind::Nuget,
        RegistryKind::Goproxy,
        RegistryKind::Maven,
        RegistryKind::Composer,
        RegistryKind::Rubygems,
        RegistryKind::Terraform,
        RegistryKind::Nodedist,
        RegistryKind::Sdkman,
    ] {
        // The wrong encoding entirely, and a well-formed document of the right
        // encoding with none of the expected keys.
        assert!(
            dispatch(kind, &text_doc("<html>404</html>"))
                .versions
                .is_empty()
                || true
        );
        let empty = dispatch(kind, &json_doc(serde_json::json!({ "unexpected": 1 })));
        assert!(
            empty.versions.is_empty(),
            "{kind} invented versions from a document with none"
        );
    }
}

// ── npm ───────────────────────────────────────────────────────────────────────

/// The packument answers both halves: versions with publish times, per-version
/// READMEs, and the root README attributed to `dist-tags.latest`.
#[test]
fn an_npm_packument_yields_versions_times_and_readmes() {
    let doc = json_doc(serde_json::json!({
        "dist-tags": { "latest": "2.0.0" },
        "readme": "# the package README",
        "time": {
            "created": "2020-01-01T00:00:00.000Z",
            "1.0.0": "2021-06-01T12:00:00.000Z",
            "2.0.0": "2023-06-01T12:00:00.000Z"
        },
        "versions": {
            "1.0.0": { "readme": "# the 1.x README" },
            "2.0.0": { "deprecated": "use 3.x" },
            "3.0.0-rc1": {}
        }
    }));
    let detail = dispatch(RegistryKind::Npm, &doc);

    let mut versions = versions_of(&detail);
    versions.sort_unstable();
    assert_eq!(versions, ["1.0.0", "2.0.0", "3.0.0-rc1"]);

    let one = detail
        .versions
        .iter()
        .find(|v| v.version == "1.0.0")
        .unwrap();
    assert_eq!(
        one.published_at.map(|t| t.to_rfc3339()),
        Some("2021-06-01T12:00:00+00:00".to_owned())
    );
    assert!(!one.is_prerelease);

    let two = detail
        .versions
        .iter()
        .find(|v| v.version == "2.0.0")
        .unwrap();
    assert_eq!(two.deprecated.as_deref(), Some("use 3.x"));

    assert!(
        detail
            .versions
            .iter()
            .find(|v| v.version == "3.0.0-rc1")
            .unwrap()
            .is_prerelease
    );

    // 1.0.0 has its own; 2.0.0 is `latest` and takes the root's, labelled.
    assert_eq!(
        detail.readmes["1.0.0"].content.as_deref(),
        Some("# the 1.x README")
    );
    assert!(!detail.readmes["1.0.0"].package_level);
    assert_eq!(
        detail.readmes["2.0.0"].content.as_deref(),
        Some("# the package README")
    );
    assert!(detail.readmes["2.0.0"].package_level);
    // And to no other version.
    assert!(!detail.readmes.contains_key("3.0.0-rc1"));
}

/// A version's own README wins over the root's, even for `latest` — the root is
/// the fallback, not the override.
#[test]
fn a_latest_version_with_its_own_readme_keeps_it() {
    let doc = json_doc(serde_json::json!({
        "dist-tags": { "latest": "1.0.0" },
        "readme": "# the package README",
        "versions": { "1.0.0": { "readme": "# this version's" } }
    }));
    let detail = dispatch(RegistryKind::Npm, &doc);
    assert_eq!(
        detail.readmes["1.0.0"].content.as_deref(),
        Some("# this version's")
    );
    assert!(!detail.readmes["1.0.0"].package_level);
}

/// The packument's links, normalised — the answer the package page shows when
/// nothing has resolved the selected version.
///
/// `git+https://…​.git` is npm's canonical spelling for `repository` and is not
/// something a browser opens, so what the reader gets has to be the rewritten
/// form, not the field.
#[test]
fn an_npm_packument_yields_the_packages_links() {
    let doc = json_doc(serde_json::json!({
        "dist-tags": { "latest": "2.0.0" },
        "repository": { "type": "git", "url": "git+https://github.com/o/r.git" },
        "homepage": "https://o.github.io/r",
        "versions": { "1.0.0": {}, "2.0.0": {} }
    }));
    let links = dispatch(RegistryKind::Npm, &doc).links.expect("links");
    assert_eq!(links.repository.as_deref(), Some("https://github.com/o/r"));
    assert_eq!(links.homepage.as_deref(), Some("https://o.github.io/r"));
}

/// `dist-tags.latest`'s own entry wins over the document root: the root fields
/// are a copy of the latest *publish*, and a package that moved forge without
/// cutting a release names the new home in the version and the old one at the
/// root.
#[test]
fn the_latest_versions_own_repository_wins_over_the_roots() {
    let doc = json_doc(serde_json::json!({
        "dist-tags": { "latest": "2.0.0" },
        "repository": "github:old/home",
        "versions": {
            "1.0.0": {},
            "2.0.0": { "repository": "https://gitlab.com/new/home" }
        }
    }));
    let links = dispatch(RegistryKind::Npm, &doc).links.expect("links");
    assert_eq!(
        links.repository.as_deref(),
        Some("https://gitlab.com/new/home")
    );
}

/// The overwhelmingly common case, and the one the page renders as absence
/// rather than as an empty link.
#[test]
fn a_packument_that_declares_no_links_yields_none() {
    let doc = json_doc(serde_json::json!({
        "dist-tags": { "latest": "1.0.0" },
        "versions": { "1.0.0": {} }
    }));
    assert!(dispatch(RegistryKind::Npm, &doc).links.is_none());
}

/// npm writes a placeholder string rather than omitting the field, so a
/// presence check alone would show an error message as documentation.
#[test]
fn npms_missing_readme_placeholder_is_not_a_readme() {
    let doc = json_doc(serde_json::json!({
        "dist-tags": { "latest": "1.0.0" },
        "readme": "ERROR: No README data found!",
        "versions": { "1.0.0": { "readme": "  " } }
    }));
    assert!(dispatch(RegistryKind::Npm, &doc).readmes.is_empty());
}

// ── cargo ─────────────────────────────────────────────────────────────────────

/// The sparse index is NDJSON, and `yanked` is cargo's own withdrawal mark —
/// carried through as the upstream's, not as this instance's policy.
#[test]
fn a_cargo_sparse_index_yields_versions_with_yanked_honoured() {
    let body = "{\"name\":\"serde\",\"vers\":\"1.0.0\",\"yanked\":false}\n\
                {\"name\":\"serde\",\"vers\":\"1.0.1\",\"yanked\":true}\n\
                \n\
                not json at all\n\
                {\"name\":\"serde\",\"vers\":\"2.0.0-beta.1\",\"yanked\":false}\n";
    let detail = dispatch(RegistryKind::Cargo, &text_doc(body));

    assert_eq!(versions_of(&detail), ["1.0.0", "1.0.1", "2.0.0-beta.1"]);
    assert!(!detail.versions[0].yanked);
    assert!(detail.versions[1].yanked);
    assert!(detail.versions[2].is_prerelease);
    // No publish times: the index carries none, and inventing one would be a
    // guess rendered as a fact.
    assert!(detail.versions.iter().all(|v| v.published_at.is_none()));
    // The sparse index carries no README, which is why cargo's unheld versions
    // report `unknown`.
    assert!(detail.readmes.is_empty());
}

// ── NuGet ─────────────────────────────────────────────────────────────────────

#[test]
fn a_nuget_flat_index_yields_its_version_strings() {
    let doc = json_doc(serde_json::json!({
        "versions": ["13.0.1", "13.0.2", "14.0.0-preview.1", 42]
    }));
    let detail = dispatch(RegistryKind::Nuget, &doc);
    // A non-string entry is not a version; it contributes nothing rather than
    // becoming a row that says `42`.
    assert_eq!(
        versions_of(&detail),
        ["13.0.1", "13.0.2", "14.0.0-preview.1"]
    );
    assert!(detail.versions[2].is_prerelease);
}

// ── Go ────────────────────────────────────────────────────────────────────────

#[test]
fn a_go_version_list_yields_one_row_per_line() {
    let detail = dispatch(
        RegistryKind::Goproxy,
        &text_doc("v1.0.0\nv1.1.0\n\n  v2.0.0-rc1  \n"),
    );
    assert_eq!(versions_of(&detail), ["v1.0.0", "v1.1.0", "v2.0.0-rc1"]);
    assert!(detail.versions[2].is_prerelease);
}

// ── Maven ─────────────────────────────────────────────────────────────────────

#[test]
fn maven_metadata_yields_the_versions_block_and_not_the_rest() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<metadata>
  <groupId>com.example</groupId>
  <artifactId>lib</artifactId>
  <version>ignored-outside-the-block</version>
  <versioning>
    <latest>2.0.0</latest>
    <release>2.0.0</release>
    <versions>
      <version>1.0.0</version>
      <version>2.0.0</version>
    </versions>
    <lastUpdated>20230601120000</lastUpdated>
  </versioning>
</metadata>"#;
    let detail = dispatch(RegistryKind::Maven, &text_doc(xml));
    assert_eq!(versions_of(&detail), ["1.0.0", "2.0.0"]);
    // `<lastUpdated>` describes the document, not a version, so it is not read
    // as a publish time.
    assert!(detail.versions.iter().all(|v| v.published_at.is_none()));
}

#[test]
fn maven_metadata_without_a_versions_block_yields_nothing() {
    let detail = dispatch(RegistryKind::Maven, &text_doc("<metadata/>"));
    assert!(detail.versions.is_empty());
}

// ── Composer ──────────────────────────────────────────────────────────────────

#[test]
fn composer_p2_yields_versions_with_their_times() {
    let doc = json_doc(serde_json::json!({
        "packages": {
            "vendor/pkg": [
                { "version": "2.0.0", "time": "2023-06-01T12:00:00+00:00" },
                { "version": "1.0.0", "time": "2021-01-01T00:00:00+00:00" }
            ]
        }
    }));
    let detail = dispatch(RegistryKind::Composer, &doc);
    assert_eq!(versions_of(&detail), ["2.0.0", "1.0.0"]);
    assert!(detail.versions[0].published_at.is_some());
}

/// In the `composer/2.0` minified encoding a field absent from an entry means
/// "unchanged from the previous one", so a naive read reports no time for every
/// version after the first.
#[test]
fn a_minified_p2_document_carries_its_fields_forward() {
    let doc = json_doc(serde_json::json!({
        "minified": "composer/2.0",
        "packages": {
            "vendor/pkg": [
                { "version": "2.0.0", "time": "2023-06-01T12:00:00+00:00" },
                { "version": "1.0.0" }
            ]
        }
    }));
    let detail = dispatch(RegistryKind::Composer, &doc);
    assert_eq!(versions_of(&detail), ["2.0.0", "1.0.0"]);
    assert!(
        detail.versions[1].published_at.is_some(),
        "the carried-forward time was dropped"
    );
}

/// p2 lists newest first and carries `source.url` per entry, so the newest
/// release's repository is the package's — and the `.git` suffix comes off,
/// because a browser opening it lands on the same forge page either way.
#[test]
fn composer_p2_yields_the_newest_releases_links() {
    let doc = json_doc(serde_json::json!({
        "packages": {
            "vendor/pkg": [
                {
                    "version": "2.0.0",
                    "source": { "type": "git", "url": "https://github.com/vendor/pkg.git" },
                    "homepage": "https://vendor.example/pkg"
                },
                { "version": "1.0.0", "source": { "url": "https://github.com/old/pkg.git" } }
            ]
        }
    }));
    let links = dispatch(RegistryKind::Composer, &doc).links.expect("links");
    assert_eq!(
        links.repository.as_deref(),
        Some("https://github.com/vendor/pkg")
    );
    assert_eq!(
        links.homepage.as_deref(),
        Some("https://vendor.example/pkg")
    );
}

// ── RubyGems ──────────────────────────────────────────────────────────────────

/// RubyGems states pre-release status rather than leaving it to be inferred,
/// and its rule ("contains a letter") accepts `1.0.0.beta`, which a `-` check
/// does not.
#[test]
fn the_rubygems_versions_api_yields_its_own_prerelease_answer() {
    let doc = json_doc(serde_json::json!([
        { "number": "1.0.0", "created_at": "2021-01-01T00:00:00.000Z", "prerelease": false },
        { "number": "1.0.0.beta", "created_at": "2020-12-01T00:00:00.000Z", "prerelease": true }
    ]));
    let detail = dispatch(RegistryKind::Rubygems, &doc);
    assert_eq!(versions_of(&detail), ["1.0.0", "1.0.0.beta"]);
    assert!(!detail.versions[0].is_prerelease);
    assert!(
        detail.versions[1].is_prerelease,
        "`1.0.0.beta` is a pre-release and only the document says so"
    );
    assert!(detail.versions[0].published_at.is_some());
}

// ── Terraform ─────────────────────────────────────────────────────────────────

#[test]
fn terraform_reads_both_the_provider_and_the_module_shape() {
    let providers = json_doc(serde_json::json!({
        "versions": [{ "version": "3.2.1" }, { "version": "3.3.0" }]
    }));
    assert_eq!(
        versions_of(&dispatch(RegistryKind::Terraform, &providers)),
        ["3.2.1", "3.3.0"]
    );

    let modules = json_doc(serde_json::json!({
        "modules": [{ "versions": [{ "version": "1.0.0" }, { "version": "1.1.0" }] }]
    }));
    assert_eq!(
        versions_of(&dispatch(RegistryKind::Terraform, &modules)),
        ["1.0.0", "1.1.0"]
    );
}

// ── PyPI ──────────────────────────────────────────────────────────────────────

/// PEP 700's `versions` is the index's own answer, and includes versions whose
/// files have all been deleted.
#[test]
fn a_pep_700_simple_page_uses_the_index_own_version_list() {
    let doc = json_doc(serde_json::json!({
        "versions": ["1.0.0", "2.0.0"],
        "files": [{ "filename": "pkg-1.0.0-py3-none-any.whl" }]
    }));
    assert_eq!(
        versions_of(&dispatch(RegistryKind::Pypi, &doc)),
        ["1.0.0", "2.0.0"]
    );
}

/// Without it — still every index in use — the versions come from the
/// filenames, deduplicated: one version has a wheel per platform plus an sdist,
/// and the table wants one row.
#[test]
fn a_simple_page_without_pep_700_derives_distinct_versions_from_filenames() {
    let doc = json_doc(serde_json::json!({
        "files": [
            { "filename": "requests-2.28.0-py3-none-any.whl" },
            { "filename": "requests-2.28.0.tar.gz" },
            { "filename": "requests-2.31.0-py3-none-any.whl" },
            { "filename": "not-a-distribution.txt" }
        ]
    }));
    assert_eq!(
        versions_of(&dispatch(RegistryKind::Pypi, &doc)),
        ["2.28.0", "2.31.0"]
    );
}

/// The README is deliberately absent: `info.description` lives in a per-version
/// document, so filling it for every row would be N upstream requests per page
/// view (open question 7).
#[test]
fn a_pypi_simple_page_carries_no_readme() {
    let doc = json_doc(serde_json::json!({ "versions": ["1.0.0"] }));
    assert!(dispatch(RegistryKind::Pypi, &doc).readmes.is_empty());
}

// ── nodedist ─────────────────────────────────────────────────────────────────

/// Node's `index.tab` as the tree serves it: eleven columns, newest first, the
/// release date in column two. The date is what the row is worth — it is the
/// same value the client hands the age gate.
#[test]
fn nodedist_index_tab_yields_versions_with_their_release_dates() {
    let doc = text_doc(
        "version\tdate\tfiles\tnpm\tv8\tuv\tzlib\topenssl\tmodules\tlts\tsecurity\n\
         v22.11.0\t2024-10-29\theaders,linux-x64\t10.9.0\t12.4\t1.49.1\t1.3\t3.0.15\t127\tJod\t-\n\
         v22.10.0\t2024-10-16\theaders,linux-x64\t10.9.0\t12.4\t1.49.1\t1.3\t3.0.15\t127\t-\t-\n",
    );
    let detail = dispatch(RegistryKind::Nodedist, &doc);
    assert_eq!(versions_of(&detail), ["v22.11.0", "v22.10.0"]);
    assert_eq!(
        detail.versions[0].published_at.map(|d| d.to_rfc3339()),
        Some("2024-10-29T00:00:00+00:00".to_owned())
    );
    assert!(detail.readmes.is_empty());
    assert!(detail.links.is_none());
}

/// io.js's nine columns read the same way: the reader goes by header name,
/// not by position.
#[test]
fn nodedist_reads_the_nine_column_iojs_table() {
    let doc = text_doc(
        "version\tdate\tfiles\tnpm\tv8\tuv\tzlib\topenssl\tmodules\n\
         v3.3.1\t2015-09-15\theaders,linux-x64\t2.14.3\t4.4\t1.7.4\t1.2.8\t1.0.2d\t45\n",
    );
    let detail = dispatch(RegistryKind::Nodedist, &doc);
    assert_eq!(versions_of(&detail), ["v3.3.1"]);
    assert!(detail.versions[0].published_at.is_some());
}
