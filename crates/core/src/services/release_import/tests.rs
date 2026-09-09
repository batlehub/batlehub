use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;

use super::*;
use crate::entities::{PackageId, PackageMetadata, Role};
use crate::error::CoreError;
use crate::ports::{
    ForgeAsset, ForgeRelease, ForgeReleaseSource, LocalRegistryBackend, RegistryClient,
};
use crate::services::hot_config::{new_hot_lock, HotConfig};
use crate::services::local_registry::test_support::{InMemBackend, NoopStorage};
use crate::services::release_import::run::glob_matches;

// ── Fakes ─────────────────────────────────────────────────────────────────

/// A forge with the releases a test names, reachable through the same
/// `RegistryClient` an import fetches bytes with — which is the arrangement
/// under test: one client, two answers.
#[derive(Default)]
struct FakeForge {
    releases: Vec<ForgeRelease>,
    /// Asset artifact coordinate → bytes.
    bodies: HashMap<String, Vec<u8>>,
    fetched: Mutex<Vec<String>>,
}

impl FakeForge {
    fn arc(releases: Vec<ForgeRelease>, bodies: HashMap<String, Vec<u8>>) -> Arc<Self> {
        Arc::new(Self {
            releases,
            bodies,
            fetched: Mutex::new(vec![]),
        })
    }
}

#[async_trait]
impl ForgeReleaseSource for FakeForge {
    async fn list_releases(&self, _repo: &str) -> Result<Vec<ForgeRelease>, CoreError> {
        Ok(self.releases.clone())
    }
    async fn release_by_tag(&self, repo: &str, tag: &str) -> Result<ForgeRelease, CoreError> {
        self.releases
            .iter()
            .find(|r| r.tag == tag)
            .cloned()
            .ok_or_else(|| CoreError::NotFound(format!("{repo}@{tag}")))
    }
}

#[async_trait]
impl RegistryClient for FakeForge {
    fn registry_type(&self) -> &str {
        "github"
    }
    fn releases(&self) -> Option<&dyn ForgeReleaseSource> {
        Some(self)
    }
    async fn resolve_metadata(&self, _pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Err(CoreError::NotSupported("not used".into()))
    }
    async fn fetch_artifact(
        &self,
        pkg: &PackageId,
    ) -> Result<crate::ports::FetchedArtifact, CoreError> {
        let artifact = pkg.artifact.clone().unwrap_or_default();
        self.fetched.lock().unwrap().push(artifact.clone());
        let body = self
            .bodies
            .get(&artifact)
            .cloned()
            .ok_or_else(|| CoreError::NotFound(artifact))?;
        Ok(crate::ports::FetchedArtifact {
            stream: Box::pin(futures::stream::once(async move {
                Ok::<Bytes, CoreError>(Bytes::from(body))
            })),
            cache_control: None,
        })
    }
}

/// The coordinate is the first two dash-separated fields of the file name, and
/// the *bytes* have to say the same thing — so a test can tell the pre-download
/// fast path from the post-download read by which one answered.
struct NameThenBytes {
    from_name: bool,
}

impl CoordinateReader for NameThenBytes {
    fn read(&self, filename: &str, bytes: &[u8]) -> Option<(String, String)> {
        let text = String::from_utf8_lossy(bytes);
        match text.split_once('@') {
            Some((name, version)) => Some((name.to_owned(), version.to_owned())),
            None => {
                let stem = filename.strip_suffix(".vsix")?;
                let (name, version) = stem.rsplit_once('-')?;
                Some((name.to_owned(), version.to_owned()))
            }
        }
    }
    fn read_from_name(&self, filename: &str) -> Option<(String, String)> {
        if !self.from_name {
            return None;
        }
        let stem = filename.strip_suffix(".vsix")?;
        let (name, version) = stem.rsplit_once('-')?;
        Some((name.to_owned(), version.to_owned()))
    }
}

fn asset(name: &str) -> ForgeAsset {
    ForgeAsset {
        artifact: format!("filename/{name}"),
        name: name.to_owned(),
        size: Some(4),
    }
}

fn release(tag: &str, assets: Vec<ForgeAsset>) -> ForgeRelease {
    ForgeRelease {
        tag: tag.to_owned(),
        draft: false,
        prerelease: false,
        assets,
    }
}

fn principal() -> ImportPrincipal {
    ImportPrincipal::new("svc-import", vec!["config:publishers".to_owned()]).unwrap()
}

struct Harness {
    svc: ReleaseImportService,
    forge: Arc<FakeForge>,
    backend: Arc<InMemBackend>,
}

fn harness(releases: Vec<ForgeRelease>, bodies: &[(&str, &str)], from_name: bool) -> Harness {
    let bodies: HashMap<String, Vec<u8>> = bodies
        .iter()
        .map(|(k, v)| (format!("filename/{k}"), v.as_bytes().to_vec()))
        .collect();
    let forge = FakeForge::arc(releases, bodies);
    let backend = InMemBackend::arc();
    let local = Arc::new(LocalRegistryService {
        backend: backend.clone() as Arc<dyn LocalRegistryBackend>,
        storage: Arc::new(NoopStorage),
        hot: new_hot_lock(HotConfig::default()),
        quota: None,
        ownership: None,
        team_namespace: None,
        sbom: None,
        explore_cache: None,
        package_repo: None,
        readme: None,
    });
    Harness {
        svc: ReleaseImportService {
            local,
            client: forge.clone() as Arc<dyn RegistryClient>,
            coordinates: Arc::new(NameThenBytes { from_name }),
            into: "vsx".to_owned(),
            from: "gh".to_owned(),
            repo: "acme/ext".to_owned(),
            assets: vec!["*.vsix".to_owned()],
            select: ReleaseSelector::Latest,
            principal: principal(),
            after_publish: None,
        },
        forge,
        backend,
    }
}

// ── The principal ─────────────────────────────────────────────────────────

/// The one rule that is a property of the type rather than of a call site: an
/// admin skips `check_namespace_membership` outright, so an import that could
/// be configured as one would publish into any namespace on the target and
/// nothing at request time would say so (RFC 0021 §4.3).
#[test]
fn the_principal_is_never_an_admin() {
    let identity = principal().identity();
    assert_eq!(identity.role, Role::User);
    assert!(!identity.is_admin());
    assert_eq!(identity.user_id.as_deref(), Some("svc-import"));
    assert_eq!(identity.groups, vec!["config:publishers".to_owned()]);
}

/// A config file that could mint an identity provider's group string would
/// collect that group's grants.
#[test]
fn a_group_without_the_reserved_prefix_is_refused() {
    for group in ["publishers", "oidc1:publishers", "config:", ""] {
        assert!(
            ImportPrincipal::new("svc", vec![group.to_owned()]).is_err(),
            "'{group}' should be refused"
        );
    }
    assert!(ImportPrincipal::new("svc", vec!["config:publishers".into()]).is_ok());
}

/// `system` is the schedule's own identity: an admin, and a different sentence.
#[test]
fn the_reserved_and_the_empty_user_id_are_refused() {
    assert!(ImportPrincipal::new("system", vec![]).is_err());
    assert!(ImportPrincipal::new("  ", vec![]).is_err());
}

// ── Asset globs ───────────────────────────────────────────────────────────

#[test]
fn a_glob_matches_the_asset_names_a_release_carries() {
    for (pattern, name, want) in [
        ("*.vsix", "batlehub-vsx-1.0.0.vsix", true),
        ("*.vsix", "batlehub-vsx-1.0.0.vsix.sig", false),
        ("*.vsix", "checksums.txt", false),
        ("batlehub-*.vsix", "batlehub-vsx-1.0.0.vsix", true),
        ("batlehub-*.vsix", "other-1.0.0.vsix", false),
        ("*", "anything", true),
        ("exact.vsix", "exact.vsix", true),
        ("exact.vsix", "exact.vsix.asc", false),
        ("*-*.vsix", "batlehub-vsx-1.0.0.vsix", true),
    ] {
        assert_eq!(glob_matches(pattern, name), want, "{pattern} vs {name}");
    }
}

// ── Choosing a release ────────────────────────────────────────────────────

/// `latest` is the newest release that is neither a draft nor a pre-release —
/// what a release page shows by default (RFC 0021 §11 q5).
#[tokio::test]
async fn latest_skips_a_prerelease_and_never_sees_a_draft() {
    let mut draft = release("v3", vec![asset("ext-3.0.0.vsix")]);
    draft.draft = true;
    let mut pre = release("v2", vec![asset("ext-2.0.0.vsix")]);
    pre.prerelease = true;
    let h = harness(
        vec![draft, pre, release("v1", vec![asset("ext-1.0.0.vsix")])],
        &[
            ("ext-3.0.0.vsix", "ext@3.0.0"),
            ("ext-2.0.0.vsix", "ext@2.0.0"),
            ("ext-1.0.0.vsix", "ext@1.0.0"),
        ],
        false,
    );
    let report = h.svc.import().await;

    assert_eq!((report.imported, report.errors), (1, 0), "{report:?}");
    let held = h.backend.versions.lock().unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(
        held[0].version, "1.0.0",
        "the stable release, not the newest"
    );
}

/// A pre-release is reachable by tag and by no other route.
#[tokio::test]
async fn a_prerelease_is_reachable_by_tag() {
    let mut pre = release("v2", vec![asset("ext-2.0.0.vsix")]);
    pre.prerelease = true;
    let h = harness(vec![pre], &[("ext-2.0.0.vsix", "ext@2.0.0")], false);

    assert_eq!(
        h.svc.import().await.imported,
        0,
        "latest will not choose it"
    );
    assert_eq!(h.svc.import_tag("v2").await.imported, 1);
}

/// A draft is not published, and importing one would serve bytes the producing
/// team has not released — so even asking for it by name is refused.
#[tokio::test]
async fn a_draft_is_refused_even_by_tag() {
    let mut draft = release("v9", vec![asset("ext-9.0.0.vsix")]);
    draft.draft = true;
    let h = harness(vec![draft], &[("ext-9.0.0.vsix", "ext@9.0.0")], false);

    let report = h.svc.import_tag("v9").await;
    assert_eq!((report.imported, report.errors), (0, 1), "{report:?}");
    assert!(report.failures[0].error.contains("draft"), "{report:?}");
}

// ── Publishing ────────────────────────────────────────────────────────────

/// The version exists, under the coordinate the artifact names, published by
/// the principal — the whole point of importing through the publish path.
#[tokio::test]
async fn an_imported_asset_is_a_published_version() {
    let h = harness(
        vec![release("v1", vec![asset("ext-1.0.0.vsix")])],
        &[("ext-1.0.0.vsix", "acme.ext@1.0.0")],
        false,
    );
    assert_eq!(h.svc.import().await.imported, 1);

    let held = h.backend.versions.lock().unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].registry, "vsx");
    assert_eq!(
        held[0].name, "acme.ext",
        "the manifest names the coordinate"
    );
    assert_eq!(held[0].version, "1.0.0");
}

/// Re-running is free, which is what makes an interval safe to set.
#[tokio::test]
async fn a_version_already_held_is_skipped_not_republished() {
    let h = harness(
        vec![release("v1", vec![asset("ext-1.0.0.vsix")])],
        &[("ext-1.0.0.vsix", "acme.ext@1.0.0")],
        false,
    );
    assert_eq!(h.svc.import().await.imported, 1);

    let again = h.svc.import().await;
    assert_eq!((again.imported, again.skipped, again.errors), (0, 1, 0));
    assert_eq!(h.backend.versions.lock().unwrap().len(), 1);
}

/// When the coordinate is in the asset name, a held version costs no download
/// at all — the difference between an interval that is affordable and one that
/// re-fetches every artifact of every release, forever.
#[tokio::test]
async fn a_filename_coordinate_skips_before_fetching() {
    let h = harness(
        vec![release("v1", vec![asset("ext-1.0.0.vsix")])],
        &[("ext-1.0.0.vsix", "ext@1.0.0")],
        true,
    );
    assert_eq!(h.svc.import().await.imported, 1);
    assert_eq!(h.forge.fetched.lock().unwrap().len(), 1);

    assert_eq!(h.svc.import().await.skipped, 1);
    assert_eq!(
        h.forge.fetched.lock().unwrap().len(),
        1,
        "the second run must not have downloaded anything"
    );
}

/// Only what the globs name. A release carries checksums and signatures beside
/// the artifact, and publishing those as packages is the failure `assets` is
/// required to prevent.
#[tokio::test]
async fn an_asset_no_glob_names_is_left_alone() {
    let h = harness(
        vec![release(
            "v1",
            vec![
                asset("ext-1.0.0.vsix"),
                asset("checksums.txt"),
                asset("ext-1.0.0.vsix.sig"),
            ],
        )],
        &[
            ("ext-1.0.0.vsix", "acme.ext@1.0.0"),
            ("checksums.txt", "not-a-package"),
            ("ext-1.0.0.vsix.sig", "signature"),
        ],
        false,
    );
    let report = h.svc.import().await;

    assert_eq!((report.imported, report.errors), (1, 0), "{report:?}");
    assert_eq!(
        h.forge.fetched.lock().unwrap().as_slice(),
        ["filename/ext-1.0.0.vsix"],
        "nothing else was even downloaded"
    );
}

/// One unreadable asset does not fail the release, and the operator is told
/// which one — the difference between a report and a count.
#[tokio::test]
async fn an_asset_that_names_no_coordinate_is_named_in_the_report() {
    let h = harness(
        vec![release(
            "v1",
            vec![asset("good-1.0.0.vsix"), asset("mystery.vsix")],
        )],
        &[
            ("good-1.0.0.vsix", "acme.good@1.0.0"),
            ("mystery.vsix", "no-coordinate-here"),
        ],
        false,
    );
    let report = h.svc.import().await;

    assert_eq!((report.imported, report.errors), (1, 1), "{report:?}");
    assert_eq!(report.failures[0].asset, "mystery.vsix");
    assert_eq!(report.failures[0].tag, "v1");
}

/// A source that serves no releases is refused before anything is fetched.
/// `AppConfig::validate()` refuses the same configuration at load; this is the
/// belt to that braces.
#[tokio::test]
async fn a_source_that_is_not_a_forge_is_refused() {
    struct NotAForge;
    #[async_trait]
    impl RegistryClient for NotAForge {
        fn registry_type(&self) -> &str {
            "npm"
        }
        async fn resolve_metadata(&self, _: &PackageId) -> Result<PackageMetadata, CoreError> {
            Err(CoreError::NotSupported("not used".into()))
        }
        async fn fetch_artifact(
            &self,
            _: &PackageId,
        ) -> Result<crate::ports::FetchedArtifact, CoreError> {
            Err(CoreError::NotSupported("not used".into()))
        }
    }
    let mut h = harness(vec![], &[], false);
    h.svc.client = Arc::new(NotAForge);

    let report = h.svc.import().await;
    assert_eq!(report.errors, 1);
    assert!(report.failures[0].error.contains("serves no releases"));
}

// ── Filename coordinates (phase 3) ────────────────────────────────────────

/// The conventions each ecosystem's own tooling produces — `npm pack`,
/// `cargo package`, a wheel's PEP 427 name. These were the CLI's rules and are
/// now the server's too, so `batlehub publish some.gem` and an import of the
/// same file agree about what it is.
#[test]
fn a_file_name_declares_its_ecosystem_name_and_version() {
    use crate::services::coordinate_from_filename as read;

    for (file, kind, name, version) in [
        (
            "Newtonsoft.Json.13.0.3.nupkg",
            "nuget",
            "Newtonsoft.Json",
            "13.0.3",
        ),
        ("my_pkg-1.2.3-py3-none-any.whl", "pypi", "my-pkg", "1.2.3"),
        ("rails-7.1.0.gem", "rubygems", "rails", "7.1.0"),
        ("left-pad-1.3.0.tgz", "npm", "left-pad", "1.3.0"),
        ("serde-1.0.0.crate", "cargo", "serde", "1.0.0"),
    ] {
        let got = read(file).unwrap_or_else(|| panic!("{file} should be read"));
        assert_eq!(
            (
                got.registry_type.as_str(),
                got.name.as_str(),
                got.version.as_str()
            ),
            (kind, name, version),
            "{file}"
        );
        assert!(got.is_publishable(), "{file}");
    }

    // A coordinate no file name carries: Maven needs a groupId, and a Composer
    // zip shares its extension with everything else.
    assert!(read("slf4j-api-2.0.13.jar").is_none());
    assert!(read("vendor-package-1.0.0.zip").is_none());

    // Cosmetic-only: the server reads the real version out of the package.
    let deb = read("anything.deb").expect("deb dispatches");
    assert!(!deb.is_publishable(), "an import must not publish under it");
}

/// The kind is checked, not assumed. An `npm` registry importing a wheel
/// because a glob was too wide would publish a Python artifact as a tarball,
/// and the failure would surface at `npm install`.
#[test]
fn a_filename_reader_ignores_another_ecosystems_artifact() {
    use crate::services::FilenameCoordinates;

    let npm = FilenameCoordinates {
        registry_type: "npm".to_owned(),
    };
    assert_eq!(
        npm.read_from_name("left-pad-1.3.0.tgz"),
        Some(("left-pad".to_owned(), "1.3.0".to_owned()))
    );
    assert_eq!(npm.read_from_name("my_pkg-1.2.3-py3-none-any.whl"), None);
    assert_eq!(npm.read_from_name("checksums.txt"), None);
}
