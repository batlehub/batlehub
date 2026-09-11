use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;

use super::test_support::*;
use super::*;
use crate::entities::{Action, CallerNet};
use crate::{
    entities::{Identity, Role},
    error::CoreError,
    ports::{
        QuotaOutcome, QuotaRepository, QuotaUsage, StorageBackend, StorageMeta, StoredArtifact,
    },
    services::hot_config::{new_hot_lock, HotConfig},
    services::{IntegrityPolicy, QuotaEnforcement, RegistryQuotaConfig, SigningConfig},
};

fn svc(
    backend: Arc<dyn crate::ports::LocalRegistryBackend>,
    max_bytes: Option<u64>,
) -> LocalRegistryService {
    LocalRegistryService {
        backend,
        storage: Arc::new(NoopStorage),
        hot: new_hot_lock(HotConfig {
            registries: HashMap::new(),
            policies: HashMap::new(),
            max_artifact_size_bytes: max_bytes,
            ..Default::default()
        }),
        quota: None,
        ownership: None,
        team_namespace: None,
        sbom: None,
        explore_cache: None,
        package_repo: None,
        readme: None,
    }
}

fn pkg(registry: &str, name: &str, version: &str) -> PublishedPackage {
    PublishedPackage {
        registry: registry.to_owned(),
        name: name.to_owned(),
        version: version.to_owned(),
        checksum: "abc".to_owned(),
        yanked: false,
        deprecated: false,
        deprecation_message: None,
        unlisted: false,
        index_metadata: serde_json::json!({}),
        published_at: Utc::now(),
        published_by: None,
        signature_bytes: None,
        signature_type: None,
        visibility: Default::default(),
        retention_keep: false,
    }
}

fn anon() -> Identity {
    Identity {
        user_id: None,
        role: Role::Anonymous,
        auth_provider: None,
        groups: vec![],
    }
}

fn user() -> Identity {
    Identity {
        user_id: Some("u1".into()),
        role: Role::User,
        auth_provider: None,
        groups: vec![],
    }
}

// ── publish error paths ───────────────────────────────────────────────────

#[tokio::test]
async fn publish_rejects_oversized_artifact() {
    let backend = InMemBackend::arc();
    let s = svc(backend, Some(10)); // 10-byte limit
    let req = PublishRequest {
        unlisted: false,
        registry: "npm".into(),
        name: "big".into(),
        version: "1.0.0".into(),
        artifact: Bytes::from(vec![0u8; 11]), // 11 bytes > 10-byte limit
        checksum: "abc".into(),
        index_metadata: serde_json::json!({}),
        publisher: user(),
        signature_bytes: None,
        signature_type: None,
    };
    let err = s.publish(req).await.unwrap_err();
    assert!(matches!(err, CoreError::PayloadTooLarge(_)));
}

// `enforce_publish_policy` is the shared policy gate that path-addressed
// registries (deb/rpm) call directly, bypassing the standard package-version
// `publish()`. These assert the limits still apply on that path.

fn apt_policy_req(name: &str, artifact_len: u64) -> PublishPolicyRequest<'_> {
    PublishPolicyRequest {
        registry: "apt",
        name,
        version: "1.0",
        artifact_len,
        signature_bytes: None,
        signature_type: None,
        artifact_key: None,
    }
}

#[tokio::test]
async fn enforce_publish_policy_rejects_oversized_artifact() {
    let s = svc(InMemBackend::arc(), Some(10)); // 10-byte limit
    let err = s
        .enforce_publish_policy(&apt_policy_req("hello", 11), &user())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::PayloadTooLarge(_)));
}

#[tokio::test]
async fn enforce_publish_policy_rejects_anonymous() {
    let s = svc(InMemBackend::arc(), None);
    let err = s
        .enforce_publish_policy(&apt_policy_req("hello", 5), &anon())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::AccessDenied(_)));
}

#[tokio::test]
async fn enforce_publish_policy_rejects_path_traversal_in_name() {
    let s = svc(InMemBackend::arc(), None);
    let err = s
        .enforce_publish_policy(&apt_policy_req("../../etc/evil", 5), &user())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::InvalidInput(_)));
}

#[tokio::test]
async fn enforce_publish_policy_accepts_valid_user_publish() {
    let s = svc(InMemBackend::arc(), None);
    s.enforce_publish_policy(&apt_policy_req("hello", 5), &user())
        .await
        .expect("valid publish should pass policy");
}

#[tokio::test]
async fn publish_rejects_path_traversal_in_name() {
    let s = svc(InMemBackend::arc(), None);
    let req = PublishRequest {
        unlisted: false,
        registry: "npm".into(),
        name: "../../../../etc/cron.d/evil".into(),
        version: "1.0.0".into(),
        artifact: Bytes::from_static(b"payload"),
        checksum: "abc".into(),
        index_metadata: serde_json::json!({}),
        publisher: user(),
        signature_bytes: None,
        signature_type: None,
    };
    let err = s.publish(req).await.unwrap_err();
    assert!(
        matches!(err, CoreError::InvalidInput(_)),
        "traversal name must be rejected, got {err:?}"
    );
}

#[tokio::test]
async fn publish_rejects_path_traversal_in_version() {
    let s = svc(InMemBackend::arc(), None);
    let req = PublishRequest {
        unlisted: false,
        registry: "npm".into(),
        name: "pkg".into(),
        version: "../../../../tmp/evil".into(),
        artifact: Bytes::from_static(b"payload"),
        checksum: "abc".into(),
        index_metadata: serde_json::json!({}),
        publisher: user(),
        signature_bytes: None,
        signature_type: None,
    };
    let err = s.publish(req).await.unwrap_err();
    assert!(
        matches!(err, CoreError::InvalidInput(_)),
        "traversal version must be rejected, got {err:?}"
    );
}

#[tokio::test]
async fn publish_rejects_slash_in_version() {
    // A `/` in the version (no `..`) would collapse two distinct coordinates onto
    // one storage key (name `pkg` + version `sub/1.0.0` == name `pkg/sub` +
    // version `1.0.0`), enabling cross-package overwrite. Must be rejected.
    let s = svc(InMemBackend::arc(), None);
    let req = PublishRequest {
        unlisted: false,
        registry: "npm".into(),
        name: "pkg".into(),
        version: "sub/1.0.0".into(),
        artifact: Bytes::from_static(b"payload"),
        checksum: "abc".into(),
        index_metadata: serde_json::json!({}),
        publisher: user(),
        signature_bytes: None,
        signature_type: None,
    };
    let err = s.publish(req).await.unwrap_err();
    assert!(
        matches!(err, CoreError::InvalidInput(_)),
        "version containing '/' must be rejected, got {err:?}"
    );
}

// ── yank / unyank: the engine decides, not a role assertion ───────────────
//
// These two used to pass against `svc()`, which wires no grants at all — because
// what they were really asserting was `has_role_at_least(&Role::User)` sitting in
// front of the decision function. RFC 0015 §6.1 asks for that assertion to be
// *replaced* by `releases:yank`, so it is gone, and a test that still passed
// against a service with no authorization model would be pinning its own absence.
//
// The property is unchanged and is now asserted where it lives: §10 rule 5's
// translation grants the write verbs to `role:user`, so an anonymous caller holds
// none of them and is refused by the resolver.

/// A registry whose grants are what §10 rule 5's translation produces for a
/// local-mode registry: the write verbs to `role:user`, nothing to anyone else.
fn rule_5_registry() -> crate::entities::RegistryGrants {
    use crate::entities::{GrantMap, Node, RegistryGrants, RegistryKind, SubjectMatcher, Tier};
    RegistryGrants {
        kind: RegistryKind::Cargo,
        registry: Node::new(
            Tier::Registry,
            "registry:cargo",
            Some(GrantMap::new().grant(
                SubjectMatcher::Role(Role::User),
                [
                    Action::ReleasesPublish,
                    Action::ReleasesOverwrite,
                    Action::ReleasesYank,
                    Action::ReleasesDelete,
                ],
            )),
        ),
        namespaces: Vec::new(),
    }
}

async fn svc_under_rule_5(backend: Arc<InMemBackend>) -> LocalRegistryService {
    let s = svc(backend, None);
    s.hot
        .write()
        .await
        .grants
        .insert("cargo".to_owned(), Arc::new(rule_5_registry()));
    s
}

#[tokio::test]
async fn yank_is_refused_to_a_caller_holding_no_grant() {
    let s = svc_under_rule_5(InMemBackend::arc()).await;
    let err = s
        .yank("cargo", "serde", "1.0.0", &anon())
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::AccessDenied(ref m) if m.contains("releases:yank")),
        "the refusal must name the verb the caller lacks, not a role: {err:?}"
    );
}

#[tokio::test]
async fn unyank_is_refused_to_a_caller_holding_no_grant() {
    let s = svc_under_rule_5(InMemBackend::arc()).await;
    let err = s
        .unyank("cargo", "serde", "1.0.0", &anon())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::AccessDenied(ref m) if m.contains("releases:yank")));
}

/// The positive control, and the half the old tests could not express: the role
/// rule 5 grants to still reaches the operation, through the engine rather than
/// around it.
#[tokio::test]
async fn a_user_yanks_under_the_rule_5_translation() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("cargo", "serde", "1.0.0"));
    let s = svc_under_rule_5(backend).await;
    s.yank("cargo", "serde", "1.0.0", &user())
        .await
        .expect("role:user holds releases:yank under rule 5");
}

// ── npm packument / version not-found ─────────────────────────────────────

#[tokio::test]
async fn get_npm_packument_not_found_when_no_versions() {
    let s = svc(InMemBackend::arc(), None);
    let err = s
        .get_npm_packument("npm", "unknown", "http://localhost", &anon())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

#[tokio::test]
async fn get_npm_version_not_found_for_unknown_version() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("npm", "express", "4.0.0"));
    let s = svc(backend, None);
    let err = s
        .get_npm_version("npm", "express", "9.9.9", "http://localhost", &anon())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

// ── go module not-found ───────────────────────────────────────────────────

#[tokio::test]
async fn get_go_version_list_not_found_when_empty() {
    let s = svc(InMemBackend::arc(), None);
    let err = s
        .get_go_version_list("go", "example.com/mod", &anon())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

#[tokio::test]
async fn get_go_info_not_found_for_unknown_version() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("go", "example.com/mod", "v1.0.0"));
    let s = svc(backend, None);
    let err = s
        .get_go_info("go", "example.com/mod", "v9.9.9", &anon())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

#[tokio::test]
async fn get_go_mod_not_found_for_unknown_version() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("go", "example.com/mod", "v1.0.0"));
    let s = svc(backend, None);
    let err = s
        .get_go_mod("go", "example.com/mod", "v9.9.9", &anon())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

#[tokio::test]
async fn get_go_mod_not_found_when_no_go_mod_key() {
    let backend = InMemBackend::arc();
    // Package exists but index_metadata has no "go_mod" key
    backend.seed(pkg("go", "example.com/mod", "v1.0.0"));
    let s = svc(backend, None);
    let err = s
        .get_go_mod("go", "example.com/mod", "v1.0.0", &anon())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

#[tokio::test]
async fn get_go_latest_not_found_when_no_versions() {
    let s = svc(InMemBackend::arc(), None);
    let err = s
        .get_go_latest("go", "example.com/mod", &anon())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

// ── maven / nuget / pypi / composer not-found ────────────────────────────────

#[tokio::test]
async fn get_maven_versions_not_found_when_no_versions() {
    let s = svc(InMemBackend::arc(), None);
    let err = s
        .get_maven_versions("maven", "com.example:mylib", &anon())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

#[tokio::test]
async fn get_nuget_versions_not_found_when_no_versions() {
    let s = svc(InMemBackend::arc(), None);
    let err = s
        .get_nuget_versions("nuget", "Newtonsoft.Json", &anon())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

#[tokio::test]
async fn get_nuget_versions_returns_versions_when_published() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("nuget", "mylib", "1.0.0"));
    backend.seed(pkg("nuget", "mylib", "2.0.0"));
    let s = svc(backend, None);
    let versions = s
        .get_nuget_versions("nuget", "mylib", &anon())
        .await
        .unwrap();
    assert_eq!(versions.len(), 2);
}

#[tokio::test]
async fn get_pypi_simple_page_not_found_when_no_versions() {
    let s = svc(InMemBackend::arc(), None);
    let err = s
        .get_pypi_simple_page("pypi", "requests", "http://localhost", &anon())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

/// The Simple index is `text/html` on the console's own origin, and every value
/// it interpolates came from a publisher. A filename read back from
/// `index_metadata` must not be able to close the `href`, close the anchor, or
/// open a tag of its own — `pip` follows absolute `href`s out of this page, so an
/// injected second anchor substitutes the artifact source for every install.
#[tokio::test]
async fn pypi_simple_page_escapes_a_hostile_filename() {
    let backend = InMemBackend::arc();
    let mut hostile = pkg("pypi", "evil", "1.0.0");
    hostile.index_metadata = serde_json::json!({
        "filename": "evil-1.0.tar.gz\"></a><a href=https://attacker.tld/backdoor.whl>backdoor<a x=\"",
        "sha256": "<img src=x onerror=alert(1)>",
    });
    backend.seed(hostile);

    let html = svc(backend, None)
        .get_pypi_simple_page("pypi", "evil", "http://localhost", &anon())
        .await
        .unwrap();

    // The payload survives only as inert text and as percent-encoded bytes
    // inside this server's own path, so assert on structure: one anchor, closed
    // once, pointing here. Two of either is the vulnerability.
    assert_eq!(html.matches("<a ").count(), 1, "{html}");
    assert_eq!(html.matches("</a>").count(), 1, "{html}");
    assert_eq!(
        html.matches("<a href=\"http://localhost/packages/").count(),
        1,
        "{html}"
    );
    assert!(
        !html.contains("<img") && !html.contains("<a href=https"),
        "a value opened a tag of its own: {html}"
    );
    // The hash reaches the fragment, so it is the percent-encode — not the
    // entity-escape — that neutralises it there.
    assert!(html.contains("#sha256=%3Cimg"), "{html}");
}

/// The same primitive through the package name, which reaches `<title>` and
/// `<h1>` and is only normalised on the way (lowercase and `-_.` collapse — it
/// removes nothing).
#[tokio::test]
async fn pypi_simple_page_escapes_a_hostile_package_name() {
    let backend = InMemBackend::arc();
    let name = "evil<script>alert(1)</script>";
    backend.seed(pkg("pypi", name, "1.0.0"));

    let html = svc(backend, None)
        .get_pypi_simple_page("pypi", name, "http://localhost", &anon())
        .await
        .unwrap();

    assert!(!html.contains("<script>"), "{html}");
    assert!(html.contains("&lt;script&gt;"), "{html}");
}

/// A `Host`-derived base is attacker-controlled in plenty of deployments.
#[tokio::test]
async fn pypi_simple_page_escapes_the_base_url() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("pypi", "requests", "2.28.0"));

    let html = svc(backend, None)
        .get_pypi_simple_page(
            "pypi",
            "requests",
            "http://evil\"><script>alert(1)</script>",
            &anon(),
        )
        .await
        .unwrap();

    assert!(!html.contains("<script>"), "{html}");
}

/// The escaping must not cost the page its job: an ordinary filename still
/// renders as a plain anchor `pip` can resolve.
#[tokio::test]
async fn pypi_simple_page_keeps_an_ordinary_link_intact() {
    let backend = InMemBackend::arc();
    let mut p = pkg("pypi", "requests", "2.28.0");
    p.index_metadata = serde_json::json!({
        "filename": "requests-2.28.0-py3-none-any.whl",
        "sha256": "deadbeef",
    });
    backend.seed(p);

    let html = svc(backend, None)
        .get_pypi_simple_page("pypi", "requests", "http://localhost/proxy/pypi/", &anon())
        .await
        .unwrap();

    assert!(
        html.contains(
            "<a href=\"http://localhost/proxy/pypi/packages/requests-2.28.0-py3-none-any.whl#sha256=deadbeef\">requests-2.28.0-py3-none-any.whl</a>"
        ),
        "{html}"
    );
}

#[tokio::test]
async fn get_composer_p2_response_not_found_when_no_versions() {
    let s = svc(InMemBackend::arc(), None);
    let err = s
        .get_composer_p2_response("composer", "vendor/pkg", "http://localhost", &anon())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

// ── Beta channel ─────────────────────────────────────────────────────────────

/// Minimal in-memory BetaChannelPort whose membership set is seeded at construction.
struct MemBetaChannel {
    members: std::collections::HashSet<String>, // user_ids
}

impl MemBetaChannel {
    fn with_users(ids: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            members: ids.iter().map(ToString::to_string).collect(),
        })
    }
    fn empty() -> Arc<Self> {
        Arc::new(Self {
            members: std::collections::HashSet::new(),
        })
    }
}

#[async_trait]
impl crate::ports::BetaChannelPort for MemBetaChannel {
    async fn is_member(&self, _registry: &str, identity: &Identity) -> Result<bool, CoreError> {
        Ok(identity
            .user_id
            .as_ref()
            .map(|id| self.members.contains(id))
            .unwrap_or(false))
    }
    async fn add_member(
        &self,
        _: &str,
        _: crate::ports::BetaChannelEntry,
    ) -> Result<(), CoreError> {
        Ok(())
    }
    async fn remove_member(&self, _: &str, _: &str, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn list_members(
        &self,
        _: &str,
    ) -> Result<Vec<crate::ports::BetaChannelEntry>, CoreError> {
        Ok(vec![])
    }
}

fn svc_with_beta(
    backend: Arc<InMemBackend>,
    beta: Arc<dyn crate::ports::BetaChannelPort>,
) -> LocalRegistryService {
    let mut bc = HashMap::new();
    bc.insert(
        "reg".to_owned(),
        beta as Arc<dyn crate::ports::BetaChannelPort>,
    );
    LocalRegistryService {
        backend,
        storage: Arc::new(NoopStorage),
        hot: new_hot_lock(HotConfig {
            registries: HashMap::new(),
            policies: HashMap::new(),
            beta_channel: bc,
            ..Default::default()
        }),
        quota: None,
        ownership: None,
        team_namespace: None,
        sbom: None,
        explore_cache: None,
        package_repo: None,
        readme: None,
    }
}

fn beta_user() -> Identity {
    Identity {
        user_id: Some("beta".into()),
        role: Role::User,
        auth_provider: None,
        groups: vec![],
    }
}

// No beta channel configured → all versions visible to everyone (tested via npm packument).
#[tokio::test]
async fn filter_no_beta_channel_shows_all_versions() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "lib", "1.0.0"));
    backend.seed(pkg("reg", "lib", "1.1.0-beta.1"));
    let s = svc(backend, None);
    let doc = s
        .get_npm_packument("reg", "lib", "http://localhost", &anon())
        .await
        .unwrap();
    assert_eq!(doc["versions"].as_object().unwrap().len(), 2);
}

// Beta channel configured; anonymous user sees only stable versions.
#[tokio::test]
async fn filter_non_member_hides_prerelease() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "lib", "1.0.0"));
    backend.seed(pkg("reg", "lib", "1.1.0-beta.1"));
    let s = svc_with_beta(backend, MemBetaChannel::empty());
    let doc = s
        .get_npm_packument("reg", "lib", "http://localhost", &anon())
        .await
        .unwrap();
    let versions = doc["versions"].as_object().unwrap();
    assert_eq!(versions.len(), 1);
    assert!(versions.contains_key("1.0.0"));
}

// Beta channel configured; member sees all versions including pre-release.
#[tokio::test]
async fn filter_member_sees_prerelease() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "lib", "1.0.0"));
    backend.seed(pkg("reg", "lib", "1.1.0-beta.1"));
    let s = svc_with_beta(backend, MemBetaChannel::with_users(&["beta"]));
    let doc = s
        .get_npm_packument("reg", "lib", "http://localhost", &beta_user())
        .await
        .unwrap();
    assert_eq!(doc["versions"].as_object().unwrap().len(), 2);
}

// check_prerelease_access passes for stable versions regardless of membership.
#[tokio::test]
async fn check_prerelease_access_stable_always_ok() {
    let backend = InMemBackend::arc();
    let s = svc_with_beta(backend, MemBetaChannel::empty());
    s.check_prerelease_access("reg", "1.0.0", &anon())
        .await
        .unwrap();
}

// check_prerelease_access blocks non-members on pre-release versions.
#[tokio::test]
async fn check_prerelease_access_blocks_non_member() {
    let backend = InMemBackend::arc();
    let s = svc_with_beta(backend, MemBetaChannel::empty());
    let err = s
        .check_prerelease_access("reg", "1.1.0-beta.1", &anon())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

// check_prerelease_access allows members on pre-release versions.
#[tokio::test]
async fn check_prerelease_access_allows_member() {
    let backend = InMemBackend::arc();
    let s = svc_with_beta(backend, MemBetaChannel::with_users(&["beta"]));
    s.check_prerelease_access("reg", "1.1.0-beta.1", &beta_user())
        .await
        .unwrap();
}

// check_prerelease_access passes when no beta channel is configured (open access).
#[tokio::test]
async fn check_prerelease_access_no_channel_open() {
    let backend = InMemBackend::arc();
    let s = svc(backend, None);
    s.check_prerelease_access("reg", "1.1.0-beta.1", &anon())
        .await
        .unwrap();
}

// npm packument: dist-tags.latest must point to latest stable, not pre-release.
#[tokio::test]
async fn npm_packument_latest_tag_skips_prerelease() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "pkg", "1.0.0"));
    backend.seed(pkg("reg", "pkg", "2.0.0-alpha.1"));
    // Even beta members should not see a pre-release as `latest`.
    let s = svc_with_beta(backend, MemBetaChannel::with_users(&["beta"]));
    let doc = s
        .get_npm_packument("reg", "pkg", "http://localhost", &beta_user())
        .await
        .unwrap();
    let latest = doc["dist-tags"]["latest"].as_str().unwrap();
    assert_eq!(latest, "1.0.0");
}

// npm packument: if all visible versions are pre-release, latest falls back to the newest pre-release.
#[tokio::test]
async fn npm_packument_latest_tag_only_prereleases() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "pkg", "1.0.0-beta.1"));
    let s = svc_with_beta(backend, MemBetaChannel::with_users(&["beta"]));
    let doc = s
        .get_npm_packument("reg", "pkg", "http://localhost", &beta_user())
        .await
        .unwrap();
    // No stable version; latest must fall back to the newest pre-release, not "".
    let latest = doc["dist-tags"]["latest"].as_str().unwrap();
    assert_eq!(latest, "1.0.0-beta.1");
}

// go @latest: prefers last stable; falls back to last pre-release only if no stable exists.
#[tokio::test]
async fn go_latest_prefers_stable_over_prerelease() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "mod", "1.0.0"));
    backend.seed(pkg("reg", "mod", "2.0.0-rc.1"));
    let s = svc_with_beta(backend, MemBetaChannel::with_users(&["beta"]));
    let info = s.get_go_latest("reg", "mod", &beta_user()).await.unwrap();
    assert_eq!(info["Version"].as_str().unwrap(), "1.0.0");
}

#[tokio::test]
async fn go_latest_falls_back_to_prerelease_when_all_prerelease() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "mod", "1.0.0-alpha.1"));
    let s = svc_with_beta(backend, MemBetaChannel::with_users(&["beta"]));
    let info = s.get_go_latest("reg", "mod", &beta_user()).await.unwrap();
    assert_eq!(info["Version"].as_str().unwrap(), "1.0.0-alpha.1");
}

// rubygems gem_info: same stable-preference behaviour.
#[tokio::test]
async fn rubygems_gem_info_prefers_stable() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "gem", "1.0.0"));
    backend.seed(pkg("reg", "gem", "1.1.0-pre"));
    let s = svc_with_beta(backend, MemBetaChannel::with_users(&["beta"]));
    let info = s
        .get_rubygems_gem_info("reg", "gem", &beta_user())
        .await
        .unwrap();
    assert_eq!(info["version"].as_str().unwrap(), "1.0.0");
}

// rubygems versions: prerelease field uses semver-aware detection.
#[tokio::test]
async fn rubygems_versions_prerelease_flag_uses_semver() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "gem", "1.0.0"));
    backend.seed(pkg("reg", "gem", "1.1.0-rc.1"));
    let s = svc_with_beta(backend, MemBetaChannel::with_users(&["beta"]));
    let versions = s
        .get_rubygems_versions("reg", "gem", &beta_user())
        .await
        .unwrap();
    // Newest first; 1.1.0-rc.1 is index 0.
    let pre = versions[0]["prerelease"].as_bool().unwrap();
    let stable = versions[1]["prerelease"].as_bool().unwrap();
    assert!(pre, "1.1.0-rc.1 should be marked prerelease=true");
    assert!(!stable, "1.0.0 should be marked prerelease=false");
}

// is_prerelease handles v-prefixed and Composer dev-branch versions.
#[test]
fn is_prerelease_handles_v_prefix_and_dev_branches() {
    let check = |v: &str| LocalRegistryService::is_prerelease(v);
    assert!(check("v1.0.0-beta.1"), "v-prefixed pre-release");
    assert!(check("dev-main"), "dev- prefix");
    assert!(check("dev-feature/branch"), "dev- with path");
    assert!(check("1.0.0-dev"), "-dev suffix");
    assert!(!check("v1.0.0"), "v-prefixed stable");
    assert!(!check("1.0.0"), "plain stable");
    assert!(!check("1.0.0.0"), "four-part (non-semver stable)");
}

// check_prerelease_access blocks non-members on Composer dev-branch versions.
#[tokio::test]
async fn check_prerelease_access_blocks_dev_branch_non_member() {
    let backend = InMemBackend::arc();
    let s = svc_with_beta(backend, MemBetaChannel::empty());
    let err = s
        .check_prerelease_access("reg", "dev-main", &anon())
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::NotFound(_)),
        "dev-main must be gated"
    );
}

// ── Team namespace enforcement tests ─────────────────────────────────────

#[derive(Debug, Default)]
struct MockTeamNamespace {
    namespaces: Mutex<Vec<crate::entities::TeamNamespace>>,
    visibility: Mutex<HashMap<(String, String), Visibility>>,
}

impl MockTeamNamespace {
    fn arc() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn with_namespace(registry: &str, prefix: &str, group: &str) -> Arc<Self> {
        let s = Self::arc();
        s.namespaces
            .lock()
            .unwrap()
            .push(crate::entities::TeamNamespace {
                registry: registry.to_owned(),
                prefix: prefix.to_owned(),
                group_id: group.to_owned(),
                claimed_by: None,
                separator: '/',
            });
        s
    }
    fn with_visibility(registry: &str, package: &str, vis: Visibility) -> Arc<Self> {
        let s = Self::arc();
        s.visibility
            .lock()
            .unwrap()
            .insert((registry.to_owned(), package.to_owned()), vis);
        s
    }
}

#[async_trait]
impl TeamNamespacePort for MockTeamNamespace {
    async fn find_namespace(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Option<crate::entities::TeamNamespace>, CoreError> {
        // `TeamNamespace::covers`, not a fifth spelling of it.
        //
        // This mock carried its own matcher hardcoded to `/`, which made it agree
        // with the three real ones by coincidence — and stop agreeing the moment
        // migration 045 made the separator per-claim. §6.3 requires every copy of
        // this rule to agree character for character; the way to get that is for
        // there to be one copy.
        let ns = self.namespaces.lock().unwrap();
        let result = ns
            .iter()
            .filter(|n| n.registry == registry && n.covers(package))
            .max_by_key(|n| n.prefix.len())
            .cloned();
        Ok(result)
    }
    async fn list_namespaces(
        &self,
        _: &str,
    ) -> Result<Vec<crate::entities::TeamNamespace>, CoreError> {
        Ok(vec![])
    }
    async fn claim_namespace(&self, ns: crate::entities::TeamNamespace) -> Result<(), CoreError> {
        // Stores, and refuses a second claim — a mock that silently accepted one
        // would let `a_second_claim_is_refused` pass against a store that takes
        // over rather than conflicts, which is the behaviour that test exists for.
        let mut all = self.namespaces.lock().unwrap();
        if all
            .iter()
            .any(|n| n.registry == ns.registry && n.prefix == ns.prefix)
        {
            return Err(CoreError::Conflict(format!(
                "namespace '{}' in registry '{}' is already claimed",
                ns.prefix, ns.registry
            )));
        }
        all.push(ns);
        Ok(())
    }
    async fn release_namespace(&self, _: &str, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn set_visibility(&self, _: &str, _: &str, _: Visibility) -> Result<(), CoreError> {
        Ok(())
    }
    async fn get_visibility(&self, registry: &str, package: &str) -> Result<Visibility, CoreError> {
        Ok(self
            .visibility
            .lock()
            .unwrap()
            .get(&(registry.to_owned(), package.to_owned()))
            .cloned()
            .unwrap_or_default())
    }
    async fn list_namespaces_for_groups(
        &self,
        groups: &[String],
    ) -> Result<Vec<crate::entities::TeamNamespace>, CoreError> {
        let ns = self.namespaces.lock().unwrap();
        Ok(ns
            .iter()
            .filter(|n| {
                groups
                    .iter()
                    .any(|g| g.replace(' ', "") == n.group_id.replace(' ', ""))
            })
            .cloned()
            .collect())
    }
    async fn list_packages_in_namespace(
        &self,
        _: &str,
        _: &str,
        _: u64,
        _: u64,
    ) -> Result<Vec<crate::entities::NamespacePackage>, CoreError> {
        Ok(vec![])
    }

    async fn count_packages_in_namespace(&self, _: &str, _: &str) -> Result<u64, CoreError> {
        Ok(0)
    }
}

fn svc_with_ns(backend: Arc<InMemBackend>, ns: Arc<dyn TeamNamespacePort>) -> LocalRegistryService {
    LocalRegistryService {
        backend,
        storage: Arc::new(NoopStorage),
        hot: new_hot_lock(HotConfig {
            registries: HashMap::new(),
            policies: HashMap::new(),
            ..Default::default()
        }),
        quota: None,
        ownership: None,
        team_namespace: Some(ns),
        sbom: None,
        explore_cache: None,
        package_repo: None,
        readme: None,
    }
}

fn member() -> Identity {
    Identity {
        user_id: Some("m1".into()),
        role: Role::User,
        auth_provider: None,
        groups: vec!["team-a".into()],
    }
}

fn non_member() -> Identity {
    Identity {
        user_id: Some("u2".into()),
        role: Role::User,
        auth_provider: None,
        groups: vec![],
    }
}

fn admin_id() -> Identity {
    Identity {
        user_id: Some("adm".into()),
        role: Role::Admin,
        auth_provider: None,
        groups: vec![],
    }
}

#[tokio::test]
async fn namespace_enforcement_blocks_non_member() {
    let backend = InMemBackend::arc();
    let ns = MockTeamNamespace::with_namespace("reg", "frontend", "team-a");
    let s = svc_with_ns(backend, ns);
    let req = PublishRequest {
        unlisted: false,
        registry: "reg".into(),
        name: "frontend/utils".into(),
        version: "1.0.0".into(),
        artifact: Bytes::from("data"),
        checksum: "abc".into(),
        index_metadata: serde_json::json!({}),
        publisher: non_member(),
        signature_bytes: None,
        signature_type: None,
    };
    let err = s.publish(req).await.unwrap_err();
    assert!(
        matches!(err, CoreError::AccessDenied(_)),
        "non-member must be denied"
    );
}

#[tokio::test]
async fn namespace_enforcement_allows_member() {
    let backend = InMemBackend::arc();
    let ns = MockTeamNamespace::with_namespace("reg", "frontend", "team-a");
    let s = svc_with_ns(backend, ns);
    let req = PublishRequest {
        unlisted: false,
        registry: "reg".into(),
        name: "frontend/utils".into(),
        version: "1.0.0".into(),
        artifact: Bytes::from("data"),
        checksum: "abc".into(),
        index_metadata: serde_json::json!({}),
        publisher: member(),
        signature_bytes: None,
        signature_type: None,
    };
    assert!(s.publish(req).await.is_ok(), "member must be allowed");
}

#[tokio::test]
async fn namespace_enforcement_admin_bypasses() {
    let backend = InMemBackend::arc();
    let ns = MockTeamNamespace::with_namespace("reg", "frontend", "team-a");
    let s = svc_with_ns(backend, ns);
    let req = PublishRequest {
        unlisted: false,
        registry: "reg".into(),
        name: "frontend/utils".into(),
        version: "1.0.0".into(),
        artifact: Bytes::from("data"),
        checksum: "abc".into(),
        index_metadata: serde_json::json!({}),
        publisher: admin_id(),
        signature_bytes: None,
        signature_type: None,
    };
    assert!(
        s.publish(req).await.is_ok(),
        "admin must bypass namespace gate"
    );
}

#[tokio::test]
async fn no_namespace_claim_allows_any_user() {
    let backend = InMemBackend::arc();
    let ns = MockTeamNamespace::arc(); // no namespaces
    let s = svc_with_ns(backend, ns);
    let req = PublishRequest {
        unlisted: false,
        registry: "reg".into(),
        name: "any/package".into(),
        version: "1.0.0".into(),
        artifact: Bytes::from("data"),
        checksum: "abc".into(),
        index_metadata: serde_json::json!({}),
        publisher: non_member(),
        signature_bytes: None,
        signature_type: None,
    };
    assert!(
        s.publish(req).await.is_ok(),
        "unclaimed namespace allows any user"
    );
}

// ── check_visibility tests ────────────────────────────────────────────────

#[tokio::test]
async fn visibility_public_allows_anonymous() {
    let s = svc(InMemBackend::arc(), None);
    // no team_namespace configured -> always Ok
    assert!(s.check_visibility("reg", "pkg", &anon()).await.is_ok());
}

#[tokio::test]
async fn visibility_internal_blocks_anonymous() {
    let ns = MockTeamNamespace::with_visibility("reg", "pkg", Visibility::Internal);
    let s = svc_with_ns(InMemBackend::arc(), ns);
    let err = s.check_visibility("reg", "pkg", &anon()).await.unwrap_err();
    assert!(matches!(err, CoreError::AccessDenied(_)));
}

#[tokio::test]
async fn visibility_internal_allows_user() {
    let ns = MockTeamNamespace::with_visibility("reg", "pkg", Visibility::Internal);
    let s = svc_with_ns(InMemBackend::arc(), ns);
    assert!(s
        .check_visibility("reg", "pkg", &non_member())
        .await
        .is_ok());
}

#[tokio::test]
async fn visibility_team_blocks_non_member() {
    let mock = MockTeamNamespace::with_namespace("reg", "frontend", "team-a");
    // override visibility map
    let mock = {
        let inner = Arc::try_unwrap(mock).unwrap();
        inner
            .visibility
            .lock()
            .unwrap()
            .insert(("reg".into(), "frontend/pkg".into()), Visibility::Team);
        Arc::new(inner)
    };
    let s = svc_with_ns(InMemBackend::arc(), mock);
    let err = s
        .check_visibility("reg", "frontend/pkg", &non_member())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::AccessDenied(_)));
}

#[tokio::test]
async fn visibility_team_allows_member() {
    let mock = MockTeamNamespace::with_namespace("reg", "frontend", "team-a");
    mock.visibility
        .lock()
        .unwrap()
        .insert(("reg".into(), "frontend/pkg".into()), Visibility::Team);
    let s = svc_with_ns(InMemBackend::arc(), mock);
    assert!(s
        .check_visibility("reg", "frontend/pkg", &member())
        .await
        .is_ok());
}

#[tokio::test]
async fn visibility_admin_bypasses_all() {
    let ns = MockTeamNamespace::with_visibility("reg", "pkg", Visibility::Team);
    let s = svc_with_ns(InMemBackend::arc(), ns);
    assert!(s.check_visibility("reg", "pkg", &admin_id()).await.is_ok());
}

// When Team visibility is set but no namespace claim exists, access must
// be denied for ALL non-admins — falling back to "any authenticated user"
// would allow non-team members to read team-private packages.
#[tokio::test]
async fn visibility_team_no_claim_denies_authenticated_user() {
    // Visibility is Team but no namespace claim is seeded.
    let ns = MockTeamNamespace::with_visibility("reg", "pkg", Visibility::Team);
    let s = svc_with_ns(InMemBackend::arc(), ns);
    let err = s
        .check_visibility("reg", "pkg", &non_member())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::AccessDenied(_)));
}

#[tokio::test]
async fn visibility_team_no_claim_denies_anonymous() {
    let ns = MockTeamNamespace::with_visibility("reg", "pkg", Visibility::Team);
    let s = svc_with_ns(InMemBackend::arc(), ns);
    let err = s.check_visibility("reg", "pkg", &anon()).await.unwrap_err();
    assert!(matches!(err, CoreError::AccessDenied(_)));
}

// Verify visibility is inherited when a second version is published on a
// package that already has a non-public visibility.
#[tokio::test]
async fn publish_second_version_inherits_visibility() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "my-pkg", "1.0.0"));

    // Seed visibility = Internal for the first version.
    let ns = MockTeamNamespace::arc();
    ns.visibility
        .lock()
        .unwrap()
        .insert(("reg".into(), "my-pkg".into()), Visibility::Internal);
    let s = svc_with_ns(backend, ns);

    let req = PublishRequest {
        unlisted: false,
        registry: "reg".into(),
        name: "my-pkg".into(),
        version: "2.0.0".into(),
        artifact: bytes::Bytes::from("data"),
        checksum: "abc".into(),
        index_metadata: serde_json::json!({}),
        publisher: user(),
        signature_bytes: None,
        signature_type: None,
    };
    s.publish(req).await.unwrap();

    // The newly published version must carry the inherited visibility.
    let versions = s.backend.get_versions("reg", "my-pkg").await.unwrap();
    let v2 = versions.iter().find(|v| v.version == "2.0.0").unwrap();
    assert_eq!(
        v2.visibility,
        Visibility::Internal,
        "second version must inherit Internal visibility from the package"
    );
}

// ── yank/unyank namespace enforcement ────────────────────────────────────

#[tokio::test]
async fn yank_blocks_non_member_in_claimed_namespace() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "frontend/utils", "1.0.0"));
    let ns = MockTeamNamespace::with_namespace("reg", "frontend", "team-a");
    let s = svc_with_ns(backend, ns);
    let err = s
        .yank("reg", "frontend/utils", "1.0.0", &non_member())
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::AccessDenied(_)),
        "non-member must not yank namespace package"
    );
}

#[tokio::test]
async fn yank_allows_namespace_member() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "frontend/utils", "1.0.0"));
    let ns = MockTeamNamespace::with_namespace("reg", "frontend", "team-a");
    let s = svc_with_ns(backend, ns);
    assert!(s
        .yank("reg", "frontend/utils", "1.0.0", &member())
        .await
        .is_ok());
}

#[tokio::test]
async fn yank_admin_bypasses_namespace() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "frontend/utils", "1.0.0"));
    let ns = MockTeamNamespace::with_namespace("reg", "frontend", "team-a");
    let s = svc_with_ns(backend, ns);
    assert!(s
        .yank("reg", "frontend/utils", "1.0.0", &admin_id())
        .await
        .is_ok());
}

#[tokio::test]
async fn yank_unclaimed_package_allows_any_user() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "unclaimed/pkg", "1.0.0"));
    let ns = MockTeamNamespace::arc(); // no claims
    let s = svc_with_ns(backend, ns);
    assert!(s
        .yank("reg", "unclaimed/pkg", "1.0.0", &non_member())
        .await
        .is_ok());
}

#[tokio::test]
async fn yank_rejects_non_owner_of_claimed_package() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("npm", "pkg", "1.0.0"));
    let mut s = svc(backend, None);
    let ownership = MockOwnership::arc();
    ownership.seed("npm", "pkg", "owner1");
    s.ownership = Some(ownership);

    let err = s.yank("npm", "pkg", "1.0.0", &user()).await.unwrap_err(); // user() is "u1", not an owner
    assert!(
        matches!(err, CoreError::AccessDenied(_)),
        "yank by a non-owner of a claimed package must be denied, got {err:?}"
    );
}

#[tokio::test]
async fn yank_allows_owner_of_claimed_package() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("npm", "pkg", "1.0.0"));
    let mut s = svc(backend, None);
    let ownership = MockOwnership::arc();
    ownership.seed("npm", "pkg", "u1");
    s.ownership = Some(ownership);

    assert!(s.yank("npm", "pkg", "1.0.0", &user()).await.is_ok());
}

#[tokio::test]
async fn unyank_blocks_non_member_in_claimed_namespace() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "frontend/utils", "1.0.0"));
    let ns = MockTeamNamespace::with_namespace("reg", "frontend", "team-a");
    let s = svc_with_ns(backend, ns);
    let err = s
        .unyank("reg", "frontend/utils", "1.0.0", &non_member())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::AccessDenied(_)));
}

#[tokio::test]
async fn unyank_allows_namespace_member() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("reg", "frontend/utils", "1.0.0"));
    let ns = MockTeamNamespace::with_namespace("reg", "frontend", "team-a");
    let s = svc_with_ns(backend, ns);
    assert!(s
        .unyank("reg", "frontend/utils", "1.0.0", &member())
        .await
        .is_ok());
}

// ── signing policy ────────────────────────────────────────────────────────

fn publish_req(registry: &str, name: &str, version: &str, publisher: Identity) -> PublishRequest {
    PublishRequest {
        unlisted: false,
        registry: registry.into(),
        name: name.into(),
        version: version.into(),
        artifact: Bytes::from_static(b"payload"),
        checksum: "abc".into(),
        index_metadata: serde_json::json!({}),
        publisher,
        signature_bytes: None,
        signature_type: None,
    }
}

#[tokio::test]
async fn publish_rejects_missing_required_signature() {
    let s = svc(InMemBackend::arc(), None);
    s.hot.write().await.signing.insert(
        "npm".into(),
        SigningConfig {
            required: true,
            allowed_types: vec![],
            ..Default::default()
        },
    );
    let req = publish_req("npm", "pkg", "1.0.0", user());
    let err = s.publish(req).await.unwrap_err();
    assert!(
        matches!(err, CoreError::AccessDenied(_)),
        "missing required signature must be denied, got {err:?}"
    );
}

#[tokio::test]
async fn publish_rejects_disallowed_signature_type() {
    let s = svc(InMemBackend::arc(), None);
    s.hot.write().await.signing.insert(
        "npm".into(),
        SigningConfig {
            required: false,
            allowed_types: vec!["ed25519".into()],
            ..Default::default()
        },
    );
    let mut req = publish_req("npm", "pkg", "1.0.0", user());
    req.signature_bytes = Some(vec![1, 2, 3]);
    req.signature_type = Some("rsa".into());
    let err = s.publish(req).await.unwrap_err();
    assert!(
        matches!(err, CoreError::AccessDenied(_)),
        "disallowed signature type must be denied, got {err:?}"
    );
}

#[tokio::test]
async fn publish_allows_matching_signature_type() {
    let s = svc(InMemBackend::arc(), None);
    s.hot.write().await.signing.insert(
        "npm".into(),
        SigningConfig {
            required: true,
            allowed_types: vec!["ed25519".into()],
            ..Default::default()
        },
    );
    let mut req = publish_req("npm", "pkg", "1.0.0", user());
    req.signature_bytes = Some(vec![1, 2, 3]);
    req.signature_type = Some("ed25519".into());
    assert!(s.publish(req).await.is_ok());
}

// ── download-path verification (re-serve checksum + signature) ─────────────

/// Build a service wired for download-path verification tests: a real in-memory
/// store plus per-registry integrity/signing policies for `"npm"`.
fn download_svc(
    backend: Arc<InMemBackend>,
    storage: Arc<MemStore>,
    integrity: Option<IntegrityPolicy>,
    signing: Option<SigningConfig>,
) -> LocalRegistryService {
    download_svc_with_access_log(backend, storage, integrity, signing, None)
}

fn download_svc_with_access_log(
    backend: Arc<InMemBackend>,
    storage: Arc<MemStore>,
    integrity: Option<IntegrityPolicy>,
    signing: Option<SigningConfig>,
    package_repo: Option<Arc<dyn crate::ports::PackageRepository>>,
) -> LocalRegistryService {
    let mut integrity_map = HashMap::new();
    if let Some(i) = integrity {
        integrity_map.insert("npm".to_owned(), i);
    }
    let mut signing_map = HashMap::new();
    if let Some(s) = signing {
        signing_map.insert("npm".to_owned(), s);
    }
    LocalRegistryService {
        backend,
        storage,
        hot: new_hot_lock(HotConfig {
            integrity: integrity_map,
            signing: signing_map,
            ..Default::default()
        }),
        quota: None,
        ownership: None,
        team_namespace: None,
        sbom: None,
        explore_cache: None,
        package_repo,
        readme: None,
    }
}

/// Spies on `record_access` calls; used to verify `get_artifact` produces the
/// same audit trail `ProxyService::handle` does (see [`SpyRepo`] in `proxy/tests.rs`
/// for the sibling used on the proxy-fallback path).
struct SpyRepo {
    events: Mutex<Vec<AccessEvent>>,
}

impl SpyRepo {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(vec![]),
        })
    }

    fn events(&self) -> Vec<AccessEvent> {
        self.events.lock().unwrap().clone()
    }
}

#[async_trait]
impl crate::ports::PackageRepository for SpyRepo {
    async fn record_access(&self, event: AccessEvent) -> Result<(), CoreError> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }
    async fn get_status(
        &self,
        _pkg: &PackageId,
    ) -> Result<crate::entities::PackageStatus, CoreError> {
        Ok(crate::entities::PackageStatus::Available)
    }
    async fn set_status(
        &self,
        _pkg: &PackageId,
        _status: crate::entities::PackageStatus,
    ) -> Result<(), CoreError> {
        Ok(())
    }
    async fn list_packages(
        &self,
        _filter: crate::entities::PackageFilter,
    ) -> Result<Vec<crate::entities::PackageSummary>, CoreError> {
        Ok(vec![])
    }
    async fn count_packages(
        &self,
        _filter: crate::entities::PackageFilter,
    ) -> Result<u64, CoreError> {
        Ok(0)
    }
    async fn list_events(
        &self,
        _filter: crate::entities::EventFilter,
    ) -> Result<Vec<AccessEvent>, CoreError> {
        Ok(self.events.lock().unwrap().clone())
    }
    async fn count_events(&self, _filter: crate::entities::EventFilter) -> Result<u64, CoreError> {
        Ok(self.events.lock().unwrap().len() as u64)
    }
    async fn delete_package(&self, _pkg: &PackageId) -> Result<bool, CoreError> {
        Ok(false)
    }
}

fn seed_version(
    backend: &InMemBackend,
    checksum: &str,
    sig_bytes: Option<Vec<u8>>,
    sig_type: Option<String>,
) {
    backend.seed(PublishedPackage {
        registry: "npm".into(),
        name: "pkg".into(),
        version: "1.0.0".into(),
        checksum: checksum.to_owned(),
        yanked: false,
        deprecated: false,
        deprecation_message: None,
        unlisted: false,
        index_metadata: serde_json::json!({}),
        published_at: Utc::now(),
        published_by: None,
        signature_bytes: sig_bytes,
        signature_type: sig_type,
        retention_keep: false,
        visibility: Default::default(),
    });
}

fn reverify_policy(verify_on_serve: bool) -> IntegrityPolicy {
    IntegrityPolicy {
        enabled: true,
        block_on_mismatch: true,
        require_metadata: false,
        bypass_roles: vec![],
        verify_on_serve,
    }
}

#[tokio::test]
async fn get_artifact_reverify_passes_when_bytes_match() {
    let body: &[u8] = b"local-artifact-bytes";
    let backend = InMemBackend::arc();
    seed_version(
        &backend,
        &crate::services::integrity::sha256_hex(body),
        None,
        None,
    );
    let storage = MemStore::arc();
    storage.put(
        &artifact_storage_key("npm", "pkg", "1.0.0"),
        Bytes::from_static(body),
    );

    let s = download_svc(backend, storage, Some(reverify_policy(true)), None);
    let out = s
        .get_artifact(
            "npm",
            "pkg",
            "1.0.0",
            Action::ReleasesRead,
            &user(),
            &CallerNet::unknown(),
        )
        .await
        .unwrap();
    assert_eq!(out.as_ref(), body);
}

#[tokio::test]
async fn get_artifact_reverify_detects_corruption() {
    let body: &[u8] = b"local-artifact-bytes";
    let backend = InMemBackend::arc();
    // Recorded checksum is for the real bytes; stored bytes are corrupted.
    seed_version(
        &backend,
        &crate::services::integrity::sha256_hex(body),
        None,
        None,
    );
    let storage = MemStore::arc();
    storage.put(
        &artifact_storage_key("npm", "pkg", "1.0.0"),
        Bytes::from_static(b"CORRUPTED"),
    );

    let s = download_svc(backend, storage, Some(reverify_policy(true)), None);
    let err = s
        .get_artifact(
            "npm",
            "pkg",
            "1.0.0",
            Action::ReleasesRead,
            &user(),
            &CallerNet::unknown(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::IntegrityFailure(_)),
        "corrupted local artifact must fail re-serve verification, got {err:?}"
    );
}

#[tokio::test]
async fn get_artifact_reverify_off_serves_corrupted_bytes() {
    let body: &[u8] = b"local-artifact-bytes";
    let backend = InMemBackend::arc();
    seed_version(
        &backend,
        &crate::services::integrity::sha256_hex(body),
        None,
        None,
    );
    let storage = MemStore::arc();
    storage.put(
        &artifact_storage_key("npm", "pkg", "1.0.0"),
        Bytes::from_static(b"CORRUPTED"),
    );

    let s = download_svc(backend, storage, Some(reverify_policy(false)), None);
    assert!(s
        .get_artifact(
            "npm",
            "pkg",
            "1.0.0",
            Action::ReleasesRead,
            &user(),
            &CallerNet::unknown(),
        )
        .await
        .is_ok());
}

#[tokio::test]
async fn get_artifact_reverify_fails_closed_when_metadata_row_missing() {
    let body: &[u8] = b"local-artifact-bytes";
    // Bytes exist in storage, but no published-version metadata row was seeded
    // (an inconsistent state). With verify_on_serve on, we must refuse to serve
    // unverified rather than silently skip the check.
    let backend = InMemBackend::arc();
    let storage = MemStore::arc();
    storage.put(
        &artifact_storage_key("npm", "pkg", "1.0.0"),
        Bytes::from_static(body),
    );

    let s = download_svc(backend, storage, Some(reverify_policy(true)), None);
    let err = s
        .get_artifact(
            "npm",
            "pkg",
            "1.0.0",
            Action::ReleasesRead,
            &user(),
            &CallerNet::unknown(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::IntegrityFailure(_)),
        "missing metadata for stored bytes must fail closed, got {err:?}"
    );
}

#[tokio::test]
async fn get_artifact_verifies_ed25519_signature() {
    use ed25519_dalek::{Signer, SigningKey};
    let body: &[u8] = b"signed-artifact";
    let sk = SigningKey::from_bytes(&[42u8; 32]);
    let pub_hex = hex::encode(sk.verifying_key().to_bytes());
    let sig = sk.sign(body).to_bytes().to_vec();

    let backend = InMemBackend::arc();
    seed_version(
        &backend,
        &crate::services::integrity::sha256_hex(body),
        Some(sig),
        Some("ed25519".into()),
    );
    let storage = MemStore::arc();
    storage.put(
        &artifact_storage_key("npm", "pkg", "1.0.0"),
        Bytes::from_static(body),
    );

    let signing = SigningConfig {
        verify_on_download: true,
        trusted_keys: vec![pub_hex],
        ..Default::default()
    };
    let s = download_svc(backend, storage, None, Some(signing));
    assert!(s
        .get_artifact(
            "npm",
            "pkg",
            "1.0.0",
            Action::ReleasesRead,
            &user(),
            &CallerNet::unknown(),
        )
        .await
        .is_ok());
}

#[tokio::test]
async fn get_artifact_rejects_signature_from_untrusted_key() {
    use ed25519_dalek::{Signer, SigningKey};
    let body: &[u8] = b"signed-artifact";
    let sk = SigningKey::from_bytes(&[42u8; 32]);
    let other_pub = hex::encode(
        SigningKey::from_bytes(&[7u8; 32])
            .verifying_key()
            .to_bytes(),
    );
    let sig = sk.sign(body).to_bytes().to_vec();

    let backend = InMemBackend::arc();
    seed_version(
        &backend,
        &crate::services::integrity::sha256_hex(body),
        Some(sig),
        Some("ed25519".into()),
    );
    let storage = MemStore::arc();
    storage.put(
        &artifact_storage_key("npm", "pkg", "1.0.0"),
        Bytes::from_static(body),
    );

    let signing = SigningConfig {
        verify_on_download: true,
        trusted_keys: vec![other_pub],
        ..Default::default()
    };
    let s = download_svc(backend, storage, None, Some(signing));
    let err = s
        .get_artifact(
            "npm",
            "pkg",
            "1.0.0",
            Action::ReleasesRead,
            &user(),
            &CallerNet::unknown(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::IntegrityFailure(_)),
        "signature from an untrusted key must be rejected, got {err:?}"
    );
}

/// Survey finding 13, download half. A row carrying signature bytes with **no
/// type** used to take the same branch as "no signature at all": counted as
/// `outcome="skipped"` and served unverified, so omitting the header was
/// strictly weaker than sending a bogus one — `X-Signature-Type: pgp` was
/// refused by the branch directly below.
///
/// The publish edge no longer accepts that pair, so this is the rows written
/// before it did. There is no way to reach them except through here.
#[tokio::test]
async fn get_artifact_refuses_stored_signature_bytes_with_no_type() {
    let body: &[u8] = b"signed-artifact";
    let backend = InMemBackend::arc();
    seed_version(
        &backend,
        &crate::services::integrity::sha256_hex(body),
        Some(vec![0xAA; 64]),
        None, // bytes, but no type
    );
    let storage = MemStore::arc();
    storage.put(
        &artifact_storage_key("npm", "pkg", "1.0.0"),
        Bytes::from_static(body),
    );

    let signing = SigningConfig {
        verify_on_download: true,
        trusted_keys: vec![hex::encode([1u8; 32])],
        ..Default::default()
    };
    let s = download_svc(backend, storage, None, Some(signing));
    let err = s
        .get_artifact(
            "npm",
            "pkg",
            "1.0.0",
            Action::ReleasesRead,
            &user(),
            &CallerNet::unknown(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::IntegrityFailure(_)),
        "a signature that names no type cannot be verified and must not be served, got {err:?}"
    );
}

/// The other half of the same `match`, and the reason it is a `match` and not a
/// blanket refusal: an artifact with **no signature at all** is governed by
/// publish-time `signing.required`, and `verify_on_download` must not
/// retroactively refuse everything published before signing was turned on.
#[tokio::test]
async fn get_artifact_still_serves_an_unsigned_artifact_under_verify_on_download() {
    let body: &[u8] = b"unsigned-artifact";
    let backend = InMemBackend::arc();
    seed_version(
        &backend,
        &crate::services::integrity::sha256_hex(body),
        None,
        None,
    );
    let storage = MemStore::arc();
    storage.put(
        &artifact_storage_key("npm", "pkg", "1.0.0"),
        Bytes::from_static(body),
    );

    let signing = SigningConfig {
        verify_on_download: true,
        trusted_keys: vec![hex::encode([1u8; 32])],
        ..Default::default()
    };
    let s = download_svc(backend, storage, None, Some(signing));
    assert!(s
        .get_artifact(
            "npm",
            "pkg",
            "1.0.0",
            Action::ReleasesRead,
            &user(),
            &CallerNet::unknown(),
        )
        .await
        .is_ok());
}

/// The gate rules judge a **concrete version**, and for a local read this
/// service holds that version's real metadata — `published_at`, whether it is
/// signed, its checksum. Handing the chain a synthetic coordinate with all three
/// `None` does not gate the download, it refuses it: `require_signed_release`
/// with `deny_missing_signature` and `release_age` with `deny_missing_timestamp`
/// both deny on absent metadata, so a Local-mode registry with either turned on
/// would answer `403` to every artifact it holds — including the properly signed
/// ones the operator turned the gate on to require.
#[tokio::test]
async fn get_artifact_judges_the_chain_against_the_versions_real_metadata() {
    use crate::rules::{ReleaseAgeGateRule, RequireSignedReleaseRule};

    for (label, rule) in [
        (
            "require_signed_release",
            Box::new(RequireSignedReleaseRule::new(vec![]).with_deny_missing_signature(true))
                as Box<dyn crate::rules::Rule>,
        ),
        (
            "release_age deny_missing_timestamp",
            Box::new(
                ReleaseAgeGateRule::new(std::time::Duration::from_secs(0), vec![])
                    .with_deny_missing_timestamp(true),
            ) as Box<dyn crate::rules::Rule>,
        ),
    ] {
        let body: &[u8] = b"signed-artifact";
        let backend = InMemBackend::arc();
        seed_version(
            &backend,
            &crate::services::integrity::sha256_hex(body),
            Some(vec![0xAA; 64]),
            Some("ed25519".into()),
        );
        let storage = MemStore::arc();
        storage.put(
            &artifact_storage_key("npm", "pkg", "1.0.0"),
            Bytes::from_static(body),
        );
        let s = download_svc(backend, storage, None, None);
        s.hot.write().await.policies.insert(
            "npm".to_owned(),
            Arc::new(crate::services::RegistryPolicy {
                metadata_ttl: None,
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![rule],
            }),
        );

        assert!(
            s.get_artifact(
                "npm",
                "pkg",
                "1.0.0",
                Action::ReleasesRead,
                &user(),
                &CallerNet::unknown(),
            )
            .await
            .is_ok(),
            "{label} refused a version whose stored row satisfies it"
        );
    }
}

/// A coordinate this instance has **no row for** must not be refused by rules
/// that judge a version.
///
/// This is what a Hybrid registry asks on every proxied artifact: not published
/// here, so no `published_at` and no signature to report. Judged against
/// `synthetic_metadata` the deny-missing gates refuse it, and the handler
/// surfaces `AccessDenied` instead of the `NotFound` that would have fallen
/// through to the upstream — so a gated Hybrid registry would answer `403` to
/// everything it proxies. RBAC still runs; only the version-judging rules defer.
#[tokio::test]
async fn get_artifact_defers_the_version_gates_for_a_coordinate_it_has_no_row_for() {
    use crate::rules::RequireSignedReleaseRule;

    // Nothing seeded: no row, and no bytes either — exactly the hybrid miss.
    let s = download_svc(InMemBackend::arc(), MemStore::arc(), None, None);
    s.hot.write().await.policies.insert(
        "npm".to_owned(),
        Arc::new(crate::services::RegistryPolicy {
            metadata_ttl: None,
            firewall_only: false,
            serve_stale_metadata: false,
            artifact_ttl: None,
            rules: vec![Box::new(
                RequireSignedReleaseRule::new(vec![]).with_deny_missing_signature(true),
            )],
        }),
    );

    let err = s
        .get_artifact(
            "npm",
            "pkg",
            "1.0.0",
            Action::ReleasesRead,
            &user(),
            &CallerNet::unknown(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::NotFound(_)),
        "a coordinate with no local row must report NotFound — the hybrid \
         fall-through keys on it — not a gate refusal: {err:?}"
    );
}

/// The other half of that split, and the reason it is not simply "rbac only":
/// an operator blocking a version this instance never published must still see
/// it refused — and refused with `AccessDenied`, because `NotFound` is what a
/// Hybrid fall-through reads as "ask the upstream for it".
#[tokio::test]
async fn get_artifact_still_applies_the_block_list_when_it_has_no_row() {
    /// [`SpyRepo`] with one coordinate reported blocked — the shape
    /// `BlockListRule` reads.
    struct BlockingRepo;

    #[async_trait]
    impl crate::ports::PackageRepository for BlockingRepo {
        async fn record_access(&self, _event: AccessEvent) -> Result<(), CoreError> {
            Ok(())
        }
        async fn get_status(
            &self,
            pkg: &PackageId,
        ) -> Result<crate::entities::PackageStatus, CoreError> {
            Ok(if pkg.version == "1.0.0" {
                crate::entities::PackageStatus::Blocked {
                    reason: "recalled upstream".to_owned(),
                    blocked_by: "admin".to_owned(),
                    blocked_at: Utc::now(),
                }
            } else {
                crate::entities::PackageStatus::Available
            })
        }
        async fn set_status(
            &self,
            _pkg: &PackageId,
            _status: crate::entities::PackageStatus,
        ) -> Result<(), CoreError> {
            Ok(())
        }
        async fn list_packages(
            &self,
            _filter: crate::entities::PackageFilter,
        ) -> Result<Vec<crate::entities::PackageSummary>, CoreError> {
            Ok(vec![])
        }
        async fn count_packages(
            &self,
            _filter: crate::entities::PackageFilter,
        ) -> Result<u64, CoreError> {
            Ok(0)
        }
        async fn list_events(
            &self,
            _filter: crate::entities::EventFilter,
        ) -> Result<Vec<AccessEvent>, CoreError> {
            Ok(vec![])
        }
        async fn count_events(
            &self,
            _filter: crate::entities::EventFilter,
        ) -> Result<u64, CoreError> {
            Ok(0)
        }
        async fn delete_package(&self, _pkg: &PackageId) -> Result<bool, CoreError> {
            Ok(false)
        }
    }

    let s = download_svc(InMemBackend::arc(), MemStore::arc(), None, None);
    s.hot.write().await.policies.insert(
        "npm".to_owned(),
        Arc::new(crate::services::RegistryPolicy {
            metadata_ttl: None,
            firewall_only: false,
            serve_stale_metadata: false,
            artifact_ttl: None,
            rules: vec![Box::new(crate::rules::BlockListRule::new(Arc::new(
                BlockingRepo,
            )))],
        }),
    );

    let err = s
        .get_artifact(
            "npm",
            "pkg",
            "1.0.0",
            Action::ReleasesRead,
            &user(),
            &CallerNet::unknown(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::AccessDenied(_)),
        "a blocked version must be refused even with no local row, and not \
         downgraded to NotFound: {err:?}"
    );
}

/// …and RBAC is not what defers. A caller the registry refuses is refused for a
/// coordinate it holds no row for, or the survey's finding comes back through
/// the door marked "not published here".
#[tokio::test]
async fn get_artifact_still_applies_rbac_when_it_has_no_row() {
    use crate::rules::RbacRule;

    let s = download_svc(InMemBackend::arc(), MemStore::arc(), None, None);
    s.hot.write().await.policies.insert(
        "npm".to_owned(),
        Arc::new(crate::services::RegistryPolicy {
            metadata_ttl: None,
            firewall_only: false,
            serve_stale_metadata: false,
            artifact_ttl: None,
            rules: vec![Box::new(
                RbacRule::from_patterns(HashMap::from([
                    (Role::Anonymous, vec![]),
                    (Role::User, vec![]),
                    (Role::Admin, vec!["*".to_owned()]),
                ]))
                .expect("fixture patterns are valid"),
            )],
        }),
    );

    let err = s
        .get_artifact(
            "npm",
            "pkg",
            "1.0.0",
            Action::ReleasesRead,
            &user(),
            &CallerNet::unknown(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::AccessDenied(_)),
        "RBAC must still refuse, got {err:?}"
    );
}

/// …and the gate still bites. The fix is to tell the chain the truth, not to
/// stop asking it.
#[tokio::test]
async fn get_artifact_still_refuses_an_unsigned_version_under_require_signed_release() {
    use crate::rules::RequireSignedReleaseRule;

    let body: &[u8] = b"unsigned-artifact";
    let backend = InMemBackend::arc();
    seed_version(
        &backend,
        &crate::services::integrity::sha256_hex(body),
        None,
        None,
    );
    let storage = MemStore::arc();
    storage.put(
        &artifact_storage_key("npm", "pkg", "1.0.0"),
        Bytes::from_static(body),
    );
    let s = download_svc(backend, storage, None, None);
    s.hot.write().await.policies.insert(
        "npm".to_owned(),
        Arc::new(crate::services::RegistryPolicy {
            metadata_ttl: None,
            firewall_only: false,
            serve_stale_metadata: false,
            artifact_ttl: None,
            rules: vec![Box::new(
                RequireSignedReleaseRule::new(vec![]).with_deny_missing_signature(true),
            )],
        }),
    );

    let err = s
        .get_artifact(
            "npm",
            "pkg",
            "1.0.0",
            Action::ReleasesRead,
            &user(),
            &CallerNet::unknown(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::AccessDenied(_)),
        "an unsigned version must still be refused, got {err:?}"
    );
}

// ── publish-time signing policy (survey finding 13, publish half) ─────────────

/// Bytes with no type satisfied `required` (bytes are present) and skipped the
/// `allowed_types` allow-list (there is no type to check), so the strongest
/// posture an operator can configure accepted a signature nothing would ever
/// verify.
#[tokio::test]
async fn publish_rejects_signature_bytes_with_no_type() {
    for allowed in [vec![], vec!["ed25519".to_owned()]] {
        let s = svc(InMemBackend::arc(), None);
        s.hot.write().await.signing.insert(
            "npm".into(),
            SigningConfig {
                required: true,
                allowed_types: allowed.clone(),
                ..Default::default()
            },
        );
        let mut req = publish_req("npm", "pkg", "1.0.0", user());
        req.signature_bytes = Some(vec![1, 2, 3]);
        req.signature_type = None;
        let err = s.publish(req).await.unwrap_err();
        assert!(
            matches!(err, CoreError::AccessDenied(_)),
            "signature bytes with no type must be denied (allowed_types = {allowed:?}), got {err:?}"
        );
    }
}

/// The pair is incoherent in both directions, and a type with no bytes was
/// silently accepted and stored.
#[tokio::test]
async fn publish_rejects_signature_type_with_no_bytes() {
    let s = svc(InMemBackend::arc(), None);
    s.hot.write().await.signing.insert(
        "npm".into(),
        SigningConfig {
            required: false,
            allowed_types: vec!["ed25519".into()],
            ..Default::default()
        },
    );
    let mut req = publish_req("npm", "pkg", "1.0.0", user());
    req.signature_type = Some("ed25519".into());
    let err = s.publish(req).await.unwrap_err();
    assert!(
        matches!(err, CoreError::AccessDenied(_)),
        "a signature type with no signature must be denied, got {err:?}"
    );
}

/// The pair check must not turn `allowed_types` into "a signature is required":
/// an unsigned publish is still governed by `required` alone.
#[tokio::test]
async fn publish_allows_an_unsigned_artifact_when_a_type_list_exists_but_signing_is_optional() {
    let s = svc(InMemBackend::arc(), None);
    s.hot.write().await.signing.insert(
        "npm".into(),
        SigningConfig {
            required: false,
            allowed_types: vec!["ed25519".into()],
            ..Default::default()
        },
    );
    let req = publish_req("npm", "pkg", "1.0.0", user());
    assert!(s.publish(req).await.is_ok());
}

// ── ownership enforcement ─────────────────────────────────────────────────

#[derive(Default)]
struct MockOwnership {
    /// (registry, package) -> owning user ids.
    owners: Mutex<HashMap<(String, String), Vec<String>>>,
}

impl MockOwnership {
    fn arc() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn seed(&self, registry: &str, package: &str, user_id: &str) {
        self.owners
            .lock()
            .unwrap()
            .entry((registry.to_owned(), package.to_owned()))
            .or_default()
            .push(user_id.to_owned());
    }
}

#[async_trait]
impl OwnershipPort for MockOwnership {
    async fn initialize_owner(
        &self,
        registry: &str,
        package: &str,
        user_id: &str,
    ) -> Result<(), CoreError> {
        self.seed(registry, package, user_id);
        Ok(())
    }

    async fn can_publish(
        &self,
        registry: &str,
        package: &str,
        identity: &Identity,
    ) -> Result<bool, CoreError> {
        let owners = self.owners.lock().unwrap();
        let Some(rows) = owners.get(&(registry.to_owned(), package.to_owned())) else {
            return Ok(true);
        };
        Ok(identity
            .user_id
            .as_ref()
            .is_some_and(|uid| rows.contains(uid)))
    }

    async fn add_owner(
        &self,
        _registry: &str,
        _package: &str,
        _entry: crate::ports::OwnerEntry,
    ) -> Result<(), CoreError> {
        Ok(())
    }

    async fn remove_owner(
        &self,
        _registry: &str,
        _package: &str,
        _principal_type: &str,
        _principal_id: &str,
    ) -> Result<(), CoreError> {
        Ok(())
    }

    async fn list_owners(
        &self,
        _registry: &str,
        _package: &str,
    ) -> Result<Vec<crate::ports::OwnerEntry>, CoreError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn publish_rejects_non_owner_of_existing_package() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("npm", "pkg", "1.0.0"));
    let mut s = svc(backend, None);
    let ownership = MockOwnership::arc();
    ownership.seed("npm", "pkg", "owner1");
    s.ownership = Some(ownership);

    let req = publish_req("npm", "pkg", "2.0.0", user()); // user() is "u1", not an owner
    let err = s.publish(req).await.unwrap_err();
    assert!(
        matches!(err, CoreError::AccessDenied(_)),
        "non-owner publish to existing package must be denied, got {err:?}"
    );
}

#[tokio::test]
async fn publish_allows_owner_of_existing_package() {
    let backend = InMemBackend::arc();
    backend.seed(pkg("npm", "pkg", "1.0.0"));
    let mut s = svc(backend, None);
    let ownership = MockOwnership::arc();
    ownership.seed("npm", "pkg", "u1");
    s.ownership = Some(ownership);

    let req = publish_req("npm", "pkg", "2.0.0", user());
    assert!(s.publish(req).await.is_ok());
}

#[tokio::test]
async fn publish_registers_initial_owner_for_new_package() {
    let backend = InMemBackend::arc();
    let mut s = svc(backend, None);
    let ownership = MockOwnership::arc();
    s.ownership = Some(ownership.clone());

    let req = publish_req("npm", "pkg", "1.0.0", user());
    assert!(s.publish(req).await.is_ok());
    assert!(ownership
        .owners
        .lock()
        .unwrap()
        .get(&("npm".to_owned(), "pkg".to_owned()))
        .unwrap()
        .contains(&"u1".to_owned()));
}

// ── quota enforcement ─────────────────────────────────────────────────────

#[derive(Default)]
struct MockQuotaRepo {
    usage: Mutex<(u64, u32)>,
}

impl MockQuotaRepo {
    fn arc() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl QuotaRepository for MockQuotaRepo {
    async fn get_usage(&self, user_id: &str, registry: &str) -> Result<QuotaUsage, CoreError> {
        let (bytes, packages) = *self.usage.lock().unwrap();
        Ok(QuotaUsage {
            user_id: user_id.to_owned(),
            registry: registry.to_owned(),
            bytes_published: bytes,
            packages_count: packages,
        })
    }

    async fn record_publish(&self, _: &str, _: &str, bytes: u64) -> Result<(), CoreError> {
        let mut g = self.usage.lock().unwrap();
        g.0 += bytes;
        g.1 += 1;
        Ok(())
    }

    async fn try_record_publish(
        &self,
        _: &str,
        _: &str,
        bytes: u64,
        max_bytes: Option<u64>,
        max_packages: Option<u32>,
    ) -> Result<QuotaOutcome, CoreError> {
        let mut g = self.usage.lock().unwrap();
        let new_bytes = g.0 + bytes;
        let new_packages = g.1 + 1;
        let exceeded = max_bytes.is_some_and(|max| new_bytes > max)
            || max_packages.is_some_and(|max| new_packages > max);
        if exceeded {
            return Ok(QuotaOutcome::Exceeded {
                bytes_used: new_bytes,
                packages_used: new_packages,
            });
        }
        g.0 = new_bytes;
        g.1 = new_packages;
        Ok(QuotaOutcome::Recorded {
            bytes_used: new_bytes,
            packages_used: new_packages,
        })
    }

    async fn revoke_publish(&self, _: &str, _: &str, bytes: u64) -> Result<(), CoreError> {
        let mut g = self.usage.lock().unwrap();
        g.0 = g.0.saturating_sub(bytes);
        g.1 = g.1.saturating_sub(1);
        Ok(())
    }

    async fn reset_usage(&self, _: &str, _: &str) -> Result<(), CoreError> {
        *self.usage.lock().unwrap() = (0, 0);
        Ok(())
    }

    async fn list_usage(&self, _: Option<&str>) -> Result<Vec<QuotaUsage>, CoreError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn publish_rejects_when_storage_quota_exceeded() {
    let mut s = svc(InMemBackend::arc(), None);
    let mut configs = HashMap::new();
    configs.insert(
        "npm".to_owned(),
        RegistryQuotaConfig {
            max_storage_bytes_per_user: Some(0),
            max_packages_per_user: None,
            warn_threshold: 0.8,
            enforcement: QuotaEnforcement::Block,
        },
    );
    s.quota = Some(Arc::new(QuotaService::new(MockQuotaRepo::arc(), configs)));

    let req = publish_req("npm", "pkg", "1.0.0", user());
    let err = s.publish(req).await.unwrap_err();
    assert!(
        matches!(err, CoreError::QuotaExceeded(_)),
        "publish over quota must be rejected, got {err:?}"
    );
}

// ── publish transaction rollback ────────────────────────────────────────────

struct FailingStorage;

#[async_trait]
impl StorageBackend for FailingStorage {
    async fn store(&self, _: &str, _: Bytes, _: StorageMeta) -> Result<(), CoreError> {
        Err(CoreError::Storage("simulated storage failure".into()))
    }
    async fn retrieve(&self, _: &str) -> Result<Option<StoredArtifact>, CoreError> {
        Ok(None)
    }
    async fn exists(&self, _: &str) -> Result<bool, CoreError> {
        Ok(false)
    }
    async fn delete(&self, _: &str) -> Result<bool, CoreError> {
        Ok(false)
    }
    async fn delete_by_prefix(&self, _: &str) -> Result<usize, CoreError> {
        Ok(0)
    }
    async fn stat_by_prefix(&self, _: &str) -> Result<(u64, u64), CoreError> {
        Ok((0, 0))
    }
    async fn list_keys(&self, _: &str) -> Result<Vec<String>, CoreError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn publish_propagates_storage_failure_and_rolls_back() {
    let backend = InMemBackend::arc();
    let mut s = svc(backend, None);
    s.storage = Arc::new(FailingStorage);

    let req = publish_req("npm", "pkg", "1.0.0", user());
    let err = s.publish(req).await.unwrap_err();
    assert!(
        matches!(err, CoreError::Storage(_)),
        "storage failure during publish must propagate, got {err:?}"
    );
}

#[derive(Default)]
struct CommitFailBackend {
    inner: InMemBackend,
}

impl CommitFailBackend {
    fn arc() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl crate::ports::LocalRegistryBackend for CommitFailBackend {
    async fn publish(&self, pkg: PublishedPackage) -> Result<(), CoreError> {
        self.inner.publish(pkg).await
    }
    async fn yank(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError> {
        self.inner.yank(registry, name, version).await
    }
    async fn unyank(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError> {
        self.inner.unyank(registry, name, version).await
    }
    async fn deprecate(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        message: Option<&str>,
    ) -> Result<(), CoreError> {
        self.inner.deprecate(registry, name, version, message).await
    }
    async fn undeprecate(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<(), CoreError> {
        self.inner.undeprecate(registry, name, version).await
    }
    async fn set_channel(&self, _: &str, _: &str, _: &str, _: &str) -> Result<bool, CoreError> {
        Ok(false)
    }

    async fn unlist(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError> {
        self.inner.unlist(registry, name, version).await
    }
    async fn relist(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError> {
        self.inner.relist(registry, name, version).await
    }
    async fn get_versions(
        &self,
        registry: &str,
        name: &str,
    ) -> Result<Vec<PublishedPackage>, CoreError> {
        self.inner.get_versions(registry, name).await
    }
    async fn exists(&self, registry: &str, name: &str) -> Result<bool, CoreError> {
        self.inner.exists(registry, name).await
    }
    async fn commit_publish(
        &self,
        _registry: &str,
        _name: &str,
        _version: &str,
    ) -> Result<(), CoreError> {
        Err(CoreError::Database("simulated commit failure".into()))
    }
}

#[tokio::test]
async fn publish_propagates_commit_failure_and_rolls_back() {
    let backend = CommitFailBackend::arc();
    let s = svc(backend, None);

    let req = publish_req("npm", "pkg", "1.0.0", user());
    let err = s.publish(req).await.unwrap_err();
    assert!(
        matches!(err, CoreError::Database(_)),
        "commit failure during publish must propagate, got {err:?}"
    );
}

// ── get_artifact audit trail (local/hybrid reads must not skip access logging) ──

#[tokio::test]
async fn get_artifact_records_allowed_download_when_access_log_configured() {
    let body: &[u8] = b"local-artifact-bytes";
    let backend = InMemBackend::arc();
    seed_version(
        &backend,
        &crate::services::integrity::sha256_hex(body),
        None,
        None,
    );
    let storage = MemStore::arc();
    storage.put(
        &artifact_storage_key("npm", "pkg", "1.0.0"),
        Bytes::from_static(body),
    );

    let spy = SpyRepo::new();
    let s = download_svc_with_access_log(backend, storage, None, None, Some(spy.clone()));
    s.get_artifact(
        "npm",
        "pkg",
        "1.0.0",
        Action::ReleasesRead,
        &user(),
        &CallerNet::unknown(),
    )
    .await
    .unwrap();

    let events = spy.events();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0].result,
        crate::entities::AccessResult::Allowed
    ));
    assert_eq!(events[0].package_id.as_ref().unwrap().name, "pkg");
}

/// A local download reaches the trail with the caller's address and agent, the
/// two columns `audit pulls` reports beside the count (RFC 0018 §13.10).
///
/// The report reads them off the event, so an event that never carried them is
/// indistinguishable there from a caller that sent no `User-Agent` — a `count`
/// with two blanks next to it, on exactly the deployments that publish their own
/// packages. This is the assertion that the local path now records what the
/// proxy path has always recorded.
#[tokio::test]
async fn get_artifact_records_the_callers_address_and_agent() {
    let body: &[u8] = b"local-artifact-bytes";
    let backend = InMemBackend::arc();
    seed_version(
        &backend,
        &crate::services::integrity::sha256_hex(body),
        None,
        None,
    );
    let storage = MemStore::arc();
    storage.put(
        &artifact_storage_key("npm", "pkg", "1.0.0"),
        Bytes::from_static(body),
    );

    let spy = SpyRepo::new();
    let s = download_svc_with_access_log(backend, storage, None, None, Some(spy.clone()));
    let net = CallerNet {
        ip: Some("10.0.0.7".to_owned()),
        user_agent: Some("npm/10.9.2 node/v22.14.0".to_owned()),
    };
    s.get_artifact("npm", "pkg", "1.0.0", Action::ReleasesRead, &user(), &net)
        .await
        .unwrap();

    let events = spy.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].ip_address.as_deref(), Some("10.0.0.7"));
    assert_eq!(
        events[0].user_agent.as_deref(),
        Some("npm/10.9.2 node/v22.14.0")
    );
}

/// The same for a refusal, which `authorize_artifact_read` records on its own.
///
/// A denied download is the row an incident starts from — "which address was
/// turned away, and what was it running" — so the enrichment cannot be limited
/// to the path that delivered bytes.
#[tokio::test]
async fn denied_local_download_records_the_callers_address_and_agent() {
    let backend = InMemBackend::arc();
    let ns = MockTeamNamespace::with_visibility("npm", "pkg", Visibility::Internal);
    let spy = SpyRepo::new();
    let s = LocalRegistryService {
        backend,
        storage: Arc::new(NoopStorage),
        hot: new_hot_lock(HotConfig {
            registries: HashMap::new(),
            policies: HashMap::new(),
            ..Default::default()
        }),
        quota: None,
        ownership: None,
        team_namespace: Some(ns),
        sbom: None,
        explore_cache: None,
        package_repo: Some(spy.clone()),
        readme: None,
    };

    let net = CallerNet {
        ip: Some("192.0.2.9".to_owned()),
        user_agent: Some("curl/8.7.1".to_owned()),
    };
    s.get_artifact("npm", "pkg", "1.0.0", Action::ReleasesRead, &anon(), &net)
        .await
        .unwrap_err();

    let events = spy.events();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0].result,
        crate::entities::AccessResult::Denied { .. }
    ));
    assert_eq!(events[0].ip_address.as_deref(), Some("192.0.2.9"));
    assert_eq!(events[0].user_agent.as_deref(), Some("curl/8.7.1"));
}

#[tokio::test]
async fn get_artifact_records_denied_download_when_visibility_check_fails() {
    let backend = InMemBackend::arc();
    let ns = MockTeamNamespace::with_visibility("npm", "pkg", Visibility::Internal);
    let spy = SpyRepo::new();
    let s = LocalRegistryService {
        backend,
        storage: Arc::new(NoopStorage),
        hot: new_hot_lock(HotConfig {
            registries: HashMap::new(),
            policies: HashMap::new(),
            ..Default::default()
        }),
        quota: None,
        ownership: None,
        team_namespace: Some(ns),
        sbom: None,
        explore_cache: None,
        package_repo: Some(spy.clone()),
        readme: None,
    };

    // Anonymous identity can't see an `Internal` package.
    let err = s
        .get_artifact(
            "npm",
            "pkg",
            "1.0.0",
            Action::ReleasesRead,
            &anon(),
            &CallerNet::unknown(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::AccessDenied(_)));

    let events = spy.events();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0].result,
        crate::entities::AccessResult::Denied { .. }
    ));
}

#[tokio::test]
async fn get_artifact_is_a_noop_for_audit_when_access_log_is_none() {
    // Default construction (package_repo: None) must not panic or behave differently —
    // audit logging is opt-in, matching the quota/ownership/sbom fields on this struct.
    let body: &[u8] = b"local-artifact-bytes";
    let backend = InMemBackend::arc();
    seed_version(
        &backend,
        &crate::services::integrity::sha256_hex(body),
        None,
        None,
    );
    let storage = MemStore::arc();
    storage.put(
        &artifact_storage_key("npm", "pkg", "1.0.0"),
        Bytes::from_static(body),
    );

    let s = download_svc(backend, storage, None, None);
    let out = s
        .get_artifact(
            "npm",
            "pkg",
            "1.0.0",
            Action::ReleasesRead,
            &user(),
            &CallerNet::unknown(),
        )
        .await
        .unwrap();
    assert_eq!(out.as_ref(), body);
}

// ── RFC 0015 §11.1 axis D — verb granularity on the write path ────────────
//
// > A subject holding `releases:read` and not `releases:publish` is served the
// > artifact and refused the publish, on the same coordinate. This is the axis
// > that does not exist today because the verbs do not.
//
// The verbs exist now and are resolved on the write path, so it does. Every row
// here keeps its positive control: under a model where refusal is the default, a
// test whose *permitted* caller is also refused proves nothing, and that failure
// mode is much easier to hit than it looks.
mod write_verbs {
    use super::*;
    use crate::entities::{
        Action, GrantMap, Node, RegistryGrants, RegistryKind, SubjectMatcher, Tier,
    };

    /// A registry whose only grant is `verbs` to everyone.
    fn registry_granting(verbs: &[Action]) -> RegistryGrants {
        RegistryGrants {
            kind: RegistryKind::Npm,
            registry: Node::new(
                Tier::Registry,
                "registry:npm",
                Some(GrantMap::new().grant(SubjectMatcher::Anyone, verbs.to_vec())),
            ),
            namespaces: Vec::new(),
        }
    }

    fn svc_with(backend: Arc<InMemBackend>, grants: RegistryGrants) -> LocalRegistryService {
        let mut hot = HotConfig::default();
        hot.grants.insert("npm".to_owned(), Arc::new(grants));
        LocalRegistryService {
            backend,
            storage: Arc::new(NoopStorage),
            hot: new_hot_lock(hot),
            quota: None,
            ownership: None,
            team_namespace: None,
            sbom: None,
            explore_cache: None,
            package_repo: None,
            readme: None,
        }
    }

    fn publish_req() -> PublishRequest {
        PublishRequest {
            unlisted: false,
            registry: "npm".into(),
            name: "pkg".into(),
            version: "1.0.0".into(),
            artifact: Bytes::from_static(b"body"),
            checksum: "abc".into(),
            index_metadata: serde_json::json!({}),
            publisher: user(),
            signature_bytes: None,
            signature_type: None,
        }
    }

    /// The axis, in one test: same caller, same coordinate, read served and
    /// publish refused.
    ///
    /// Before the write verbs were on the request path this was unassertable —
    /// publish answered `has_role_at_least(&Role::User)`, so a grant hierarchy
    /// that withheld `releases:publish` changed nothing at all. That is the
    /// *"I granted it and nothing happened"* failure §4.2 rule 2 exists to
    /// remove, arriving inside the model built to remove it.
    #[tokio::test]
    async fn a_reader_without_publish_is_served_the_read_and_refused_the_write() {
        let backend = InMemBackend::arc();
        let s = svc_with(
            Arc::clone(&backend),
            registry_granting(&[Action::ReleasesRead, Action::ReleasesList]),
        );

        // The read this caller does hold.
        crate::services::authz::authorize_read(
            &s.hot,
            &PackageId::new("npm", "pkg", "1.0.0"),
            &user(),
            Action::ReleasesRead,
        )
        .await
        .expect("the read verb is granted and must be served");

        // …and the write they do not, on the same coordinate.
        let err = s.publish(publish_req()).await.unwrap_err();
        assert!(
            matches!(err, CoreError::AccessDenied(ref m) if m.contains("releases:publish")),
            "a caller without releases:publish must be refused, and told which verb: {err:?}"
        );
    }

    /// The positive control for the row above.
    #[tokio::test]
    async fn the_same_caller_publishes_once_the_verb_is_granted() {
        let backend = InMemBackend::arc();
        let s = svc_with(
            Arc::clone(&backend),
            registry_granting(&[Action::ReleasesRead, Action::ReleasesPublish]),
        );
        s.publish(publish_req())
            .await
            .expect("the grant is what was missing, not the fixture");
    }

    /// Publish does not carry yank, and yank does not carry delete.
    ///
    /// The verbs are separate lines in §4.2's table and an operator granting one
    /// is not granting the others. Asserted on the same service so the only
    /// difference between the arms is the verb.
    #[tokio::test]
    async fn each_write_verb_is_held_separately() {
        let backend = InMemBackend::arc();
        backend.seed(pkg("npm", "pkg", "1.0.0"));

        let publisher_only = svc_with(
            Arc::clone(&backend),
            registry_granting(&[Action::ReleasesPublish]),
        );
        let err = publisher_only
            .yank("npm", "pkg", "1.0.0", &user())
            .await
            .unwrap_err();
        assert!(
            matches!(err, CoreError::AccessDenied(ref m) if m.contains("releases:yank")),
            "releases:publish must not carry releases:yank: {err:?}"
        );

        let yanker = svc_with(
            Arc::clone(&backend),
            registry_granting(&[Action::ReleasesYank]),
        );
        yanker
            .yank("npm", "pkg", "1.0.0", &user())
            .await
            .expect("the control: releases:yank does carry yank");
        let err = yanker
            .delete_version("npm", "pkg", "1.0.0", &user())
            .await
            .unwrap_err();
        assert!(
            matches!(err, CoreError::AccessDenied(ref m) if m.contains("releases:delete")),
            "releases:yank must not carry releases:delete: {err:?}"
        );
    }

    /// Unlist and deprecate are the same verb as yank (§4.2 rule 3), and the
    /// module docs say so — this is the assertion behind the sentence.
    #[tokio::test]
    async fn the_hide_family_shares_one_verb() {
        let backend = InMemBackend::arc();
        backend.seed(pkg("npm", "pkg", "1.0.0"));
        let s = svc_with(
            Arc::clone(&backend),
            registry_granting(&[Action::ReleasesYank]),
        );
        for op in ["unlist", "deprecate"] {
            let out = match op {
                "unlist" => s.unlist("npm", "pkg", "1.0.0", &user()).await,
                _ => s.deprecate("npm", "pkg", "1.0.0", None, &user()).await,
            };
            out.unwrap_or_else(|e| panic!("{op} must be reachable with releases:yank: {e:?}"));
        }

        // …and refused without it, or the row above would pass on a service that
        // never consulted a grant.
        let ungranted = svc_with(backend, registry_granting(&[Action::ReleasesRead]));
        assert!(ungranted
            .unlist("npm", "pkg", "1.0.0", &user())
            .await
            .is_err());
    }

    /// RFC 0015 §4.3 — a namespace seal stops the registry's grants, and the
    /// write path honours it.
    ///
    /// This is the configuration the finding was reported against: an operator
    /// seals `@acme/billing` expecting nobody to publish there, and before the
    /// verbs were wired the publish succeeded anyway because it never asked.
    #[tokio::test]
    async fn a_namespace_seal_refuses_a_publish_the_registry_would_allow() {
        let backend = InMemBackend::arc();
        let sealed = RegistryGrants {
            kind: RegistryKind::Npm,
            registry: Node::new(
                Tier::Registry,
                "registry:npm",
                Some(GrantMap::new().grant(
                    SubjectMatcher::Anyone,
                    [Action::ReleasesRead, Action::ReleasesPublish],
                )),
            ),
            namespaces: vec![(
                "@acme/billing".to_owned(),
                Node::new(
                    Tier::Namespace,
                    "namespace:@acme/billing",
                    Some(GrantMap::sealed()),
                ),
            )],
        };
        let s = svc_with(Arc::clone(&backend), sealed);

        let mut inside = publish_req();
        inside.name = "@acme/billing/cards".into();
        let err = s.publish(inside).await.unwrap_err();
        assert!(
            matches!(err, CoreError::AccessDenied(_)),
            "a sealed namespace must refuse the publish the registry grants: {err:?}"
        );

        // The control, and the one that says the seal is a *namespace* seal
        // rather than the fixture refusing everything: its neighbour is
        // unaffected, on segment boundaries (`-internal` is not under it).
        let mut outside = publish_req();
        outside.name = "@acme/billing-internal".into();
        s.publish(outside)
            .await
            .expect("a seal must not reach a name that merely starts with the prefix");
    }

    /// RFC 0015 §6.1 — replacing existing bytes needs `releases:overwrite` as
    /// well as `releases:publish`.
    ///
    /// On the path where a replacement is possible at all: the path-addressed and
    /// multi-file publishers, which name their storage key. §13.6 records why
    /// that is the whole scope — the row-based backend refuses every republish
    /// before any policy is consulted, so there is nothing there to authorize.
    #[tokio::test]
    async fn replacing_existing_bytes_needs_the_overwrite_verb() {
        /// A storage backend that already holds everything asked of it.
        struct Present;
        #[async_trait]
        impl StorageBackend for Present {
            async fn store(&self, _: &str, _: Bytes, _: StorageMeta) -> Result<(), CoreError> {
                Ok(())
            }
            async fn retrieve(&self, _: &str) -> Result<Option<StoredArtifact>, CoreError> {
                Ok(None)
            }
            async fn exists(&self, _: &str) -> Result<bool, CoreError> {
                Ok(true)
            }
            async fn delete(&self, _: &str) -> Result<bool, CoreError> {
                Ok(false)
            }
            async fn delete_by_prefix(&self, _: &str) -> Result<usize, CoreError> {
                Ok(0)
            }
            async fn stat_by_prefix(&self, _: &str) -> Result<(u64, u64), CoreError> {
                Ok((0, 0))
            }
            async fn list_keys(&self, _: &str) -> Result<Vec<String>, CoreError> {
                Ok(vec![])
            }
        }

        let req = |key| PublishPolicyRequest {
            registry: "npm",
            name: "pkg",
            version: "1.0.0",
            artifact_len: 4,
            signature_bytes: None,
            signature_type: None,
            artifact_key: Some(key),
        };

        // Publish alone is not enough when the bytes are already there…
        let mut s = svc_with(
            InMemBackend::arc(),
            registry_granting(&[Action::ReleasesPublish]),
        );
        s.storage = Arc::new(Present);
        let err = s
            .enforce_publish_policy(&req("npm/pkg/1.0.0/pkg.jar"), &user())
            .await
            .unwrap_err();
        assert!(
            matches!(err, CoreError::AccessDenied(ref m) if m.contains("releases:overwrite")),
            "a re-PUT over existing bytes needs the overwrite verb: {err:?}"
        );

        // …and is enough when they are not, which is every first publish
        // including each later file of one multi-file coordinate.
        let fresh = svc_with(
            InMemBackend::arc(),
            registry_granting(&[Action::ReleasesPublish]),
        );
        fresh
            .enforce_publish_policy(&req("npm/pkg/1.0.0/pkg.jar"), &user())
            .await
            .expect("nothing is being replaced, so publish alone carries it");

        // …and the caller who holds both is served, or the row above would be
        // asserting that this path refuses everyone.
        let mut both = svc_with(
            InMemBackend::arc(),
            registry_granting(&[Action::ReleasesPublish, Action::ReleasesOverwrite]),
        );
        both.storage = Arc::new(Present);
        both.enforce_publish_policy(&req("npm/pkg/1.0.0/pkg.jar"), &user())
            .await
            .expect("both verbs held, so the replacement is authorized");
    }

    /// The attributability floor survives the verbs.
    ///
    /// A hand-written `"*" = ["releases:publish"]` grants publish to anonymous —
    /// and an anonymous publish is one `register_initial_owner` cannot attribute
    /// and the quota counter cannot meter. The floor is why `authorize_write`
    /// is added *above* the role check rather than replacing it.
    #[tokio::test]
    async fn an_anonymous_publish_is_refused_even_when_a_grant_names_everyone() {
        let s = svc_with(
            InMemBackend::arc(),
            registry_granting(&[Action::ReleasesPublish]),
        );
        let mut req = publish_req();
        req.publisher = anon();
        assert!(matches!(
            s.publish(req).await.unwrap_err(),
            CoreError::AccessDenied(_)
        ));
    }
}

// ── RFC 0015 §4.2/§4.4 — `releases:list` and `releases:read` are separable ────
//
// §4.4's opening sentence describes a caller *"holding `releases:list` on a
// namespace but `releases:read` on only some of its packages"*, and until the
// listing funnel requested `releases:list` that configuration was inexpressible:
// both documents and artifacts asked for the same verb, so a grant naming one of
// them either opened both or closed both.
//
// Asserted in both directions on one service, because an implementation that got
// either backwards would pass the other.
mod listing_verb {
    use super::*;
    use crate::entities::{
        Action, GrantMap, Node, RegistryGrants, RegistryKind, SubjectMatcher, Tier,
    };

    fn svc_granting(verbs: &[Action]) -> (LocalRegistryService, Arc<InMemBackend>) {
        let backend = InMemBackend::arc();
        backend.seed(pkg("npm", "pkg", "1.0.0"));
        let s = svc(
            Arc::clone(&backend) as Arc<dyn crate::ports::LocalRegistryBackend>,
            None,
        );
        futures::executor::block_on(async {
            s.hot.write().await.grants.insert(
                "npm".to_owned(),
                Arc::new(RegistryGrants {
                    kind: RegistryKind::Npm,
                    registry: Node::new(
                        Tier::Registry,
                        "registry:npm",
                        Some(GrantMap::new().grant(SubjectMatcher::Anyone, verbs.to_vec())),
                    ),
                    namespaces: Vec::new(),
                }),
            );
        });
        (s, backend)
    }

    /// `releases:list` alone serves the version document.
    ///
    /// The caller holds no `releases:read`, so before the funnel requested the
    /// listing verb this was a refusal — the document and the bytes were one
    /// permission.
    #[tokio::test]
    async fn list_without_read_serves_the_version_document() {
        let (s, _b) = svc_granting(&[Action::ReleasesList]);
        let doc = s
            .get_npm_packument("npm", "pkg", "http://x", &user())
            .await
            .expect("a caller holding releases:list may read the version document");
        assert!(doc["versions"].get("1.0.0").is_some());
    }

    /// …and does not serve the artifact.
    ///
    /// The half that makes the split worth having: §4.4's caller sees the index
    /// and is refused the bytes, on the same coordinate.
    #[tokio::test]
    async fn list_without_read_is_refused_the_artifact() {
        let (s, _b) = svc_granting(&[Action::ReleasesList]);
        let err = s
            .get_artifact(
                "npm",
                "pkg",
                "1.0.0",
                Action::ReleasesRead,
                &user(),
                &CallerNet::unknown(),
            )
            .await
            .expect_err("releases:list must not carry the bytes");
        assert!(matches!(err, CoreError::AccessDenied(_)), "{err:?}");
    }

    /// The inverse: `releases:read` alone is refused the document.
    ///
    /// Asserted because an implementation that requested `releases:list` for the
    /// artifact *too* would pass the pair above and fail here — the two tests
    /// together pin which verb goes where rather than that two verbs exist.
    ///
    /// Note this configuration cannot arise from `[registries.rbac]`: §10 rule 4
    /// gives `releases:list` to anyone holding `releases:read`, precisely so no
    /// migrated estate lands here. It takes a hand-written grants block, which is
    /// what the verb is for.
    #[tokio::test]
    async fn read_without_list_is_refused_the_document() {
        let (s, _b) = svc_granting(&[Action::ReleasesRead]);
        let err = s
            .get_npm_packument("npm", "pkg", "http://x", &user())
            .await
            .expect_err("releases:read must not carry the version document");
        assert!(matches!(err, CoreError::AccessDenied(_)), "{err:?}");
    }

    /// §10 rule 4's guarantee, as a test: the translation gives both together, so
    /// no config that reached a document yesterday is refused one today.
    #[tokio::test]
    async fn the_migration_grants_both_so_nothing_regresses() {
        let (s, _b) = svc_granting(&[Action::ReleasesRead, Action::ReleasesList]);
        assert!(s
            .get_npm_packument("npm", "pkg", "http://x", &user())
            .await
            .is_ok());
    }
}

// ── RFC 0015 §4.2 — `jetbrains:channel:assign` ───────────────────────────────
//
// The verb §13.13 listed as gating an unimplemented action. What made it
// implementable in a day rather than a week is that the *read* side already
// selected on `channel` (`eco_jetbrains.rs`) — only the write was missing.
mod channel_assign {
    use super::*;
    use crate::entities::{
        Action, GrantMap, Node, RegistryGrants, RegistryKind, SubjectMatcher, Tier,
    };

    fn plugin(channel: &str) -> PublishedPackage {
        let mut p = pkg("jb", "com.acme.plugin", "1.0.0");
        p.index_metadata = serde_json::json!({ "channel": channel });
        p
    }

    fn svc_granting(verbs: &[Action], seed: PublishedPackage) -> LocalRegistryService {
        let backend = InMemBackend::arc();
        backend.seed(seed);
        let s = svc(backend, None);
        futures::executor::block_on(async {
            s.hot.write().await.grants.insert(
                "jb".to_owned(),
                Arc::new(RegistryGrants {
                    kind: RegistryKind::Jetbrains,
                    registry: Node::new(
                        Tier::Registry,
                        "registry:jb",
                        Some(GrantMap::new().grant(SubjectMatcher::Anyone, verbs.to_vec())),
                    ),
                    namespaces: Vec::new(),
                }),
            );
        });
        s
    }

    async fn channel_of(s: &LocalRegistryService) -> String {
        s.backend
            .get_versions("jb", "com.acme.plugin")
            .await
            .unwrap()
            .first()
            .and_then(|p| p.index_metadata.get("channel").cloned())
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// The verb moves a build between channels.
    #[tokio::test]
    async fn the_verb_moves_a_build_to_another_channel() {
        let s = svc_granting(&[Action::JetbrainsChannelAssign], plugin(""));
        assert_eq!(channel_of(&s).await, "", "the control: it starts on Stable");

        let changed = s
            .assign_channel("jb", "com.acme.plugin", "1.0.0", "eap", &user())
            .await
            .expect("the grant permits it");
        assert!(changed);
        assert_eq!(channel_of(&s).await, "eap");
    }

    /// …and nothing else does.
    ///
    /// §4.2's whole argument for an ecosystem verb: *"forcing it into an existing
    /// verb makes the grant mean something different on npm than it does anywhere
    /// else"*. A caller holding every shared write verb still cannot move a
    /// channel.
    #[tokio::test]
    async fn no_shared_write_verb_carries_a_channel_move() {
        let s = svc_granting(
            &[
                Action::ReleasesPublish,
                Action::ReleasesOverwrite,
                Action::ReleasesYank,
                Action::ReleasesDelete,
            ],
            plugin(""),
        );
        let err = s
            .assign_channel("jb", "com.acme.plugin", "1.0.0", "eap", &user())
            .await
            .expect_err("only jetbrains:channel:assign carries this");
        assert!(
            matches!(err, CoreError::AccessDenied(ref m) if m.contains("jetbrains:channel:assign")),
            "{err:?}"
        );
        assert_eq!(channel_of(&s).await, "", "and nothing moved");
    }

    /// The empty string is Stable, not a missing value.
    ///
    /// JetBrains' own convention, and the one `eco_jetbrains.rs` already reads
    /// when selecting builds — so moving *back* to Stable has to be expressible
    /// rather than being indistinguishable from "no channel given".
    #[tokio::test]
    async fn the_empty_channel_is_stable_and_is_assignable() {
        let s = svc_granting(&[Action::JetbrainsChannelAssign], plugin("eap"));
        assert!(s
            .assign_channel("jb", "com.acme.plugin", "1.0.0", "", &user())
            .await
            .unwrap());
        assert_eq!(channel_of(&s).await, "");
    }

    /// A move to the channel it is already on reports `false` and writes nothing.
    #[tokio::test]
    async fn a_move_to_the_same_channel_changes_nothing() {
        let s = svc_granting(&[Action::JetbrainsChannelAssign], plugin("eap"));
        assert!(!s
            .assign_channel("jb", "com.acme.plugin", "1.0.0", "eap", &user())
            .await
            .unwrap());
    }

    /// **The bytes are untouched**, which is why `immutable` does not apply.
    ///
    /// §4.5 asks whether repointing a published version counts as replacing it;
    /// §13.6 answered the general form — *"immutability is a question about
    /// **bytes**, not about a coordinate"*. This is that answer as an assertion:
    /// the checksum a client verified against before the move is the checksum
    /// after it.
    #[tokio::test]
    async fn a_channel_move_does_not_touch_the_artifact() {
        let s = svc_granting(&[Action::JetbrainsChannelAssign], plugin(""));
        let before = s
            .backend
            .get_versions("jb", "com.acme.plugin")
            .await
            .unwrap();
        let checksum = before[0].checksum.clone();

        s.assign_channel("jb", "com.acme.plugin", "1.0.0", "eap", &user())
            .await
            .unwrap();

        let after = s
            .backend
            .get_versions("jb", "com.acme.plugin")
            .await
            .unwrap();
        assert_eq!(after[0].checksum, checksum, "no byte of the artifact moved");
        assert_eq!(after[0].version, "1.0.0", "and it is the same coordinate");
    }
}

// ── RFC 0015 §4.2 — `openvsx:namespace:claim` ────────────────────────────────
//
// The last of the four, and the one that was **blocked rather than declined**:
// `team_namespaces` was always the right store, and what stopped it was that
// every matcher hardcoded `/` while OpenVSX namespaces are dotted (§4.1).
// Migration 045 put the separator on the claim, which is what makes this a
// lookup rather than a special case.
mod openvsx_namespace_claim {
    use super::*;
    use crate::entities::{
        Action, GrantMap, Node, RegistryGrants, RegistryKind, SubjectMatcher, Tier,
    };

    fn svc_granting(verbs: &[Action]) -> LocalRegistryService {
        let mut s = svc(InMemBackend::arc(), None);
        s.team_namespace = Some(MockTeamNamespace::arc());
        futures::executor::block_on(async {
            s.hot.write().await.grants.insert(
                "vsx".to_owned(),
                Arc::new(RegistryGrants {
                    kind: RegistryKind::Openvsx,
                    registry: Node::new(
                        Tier::Registry,
                        "registry:vsx",
                        Some(GrantMap::new().grant(SubjectMatcher::Anyone, verbs.to_vec())),
                    ),
                    namespaces: Vec::new(),
                }),
            );
        });
        s
    }

    /// The verb claims a namespace, and the claim records the **ecosystem's**
    /// separator.
    ///
    /// That last part is the whole blocker: with `/` the claim would cover
    /// `digital` and nothing else, so every extension under it stayed outside any
    /// claim and its team visibility was unenforceable.
    #[tokio::test]
    async fn a_claim_records_the_ecosystem_separator() {
        let s = svc_granting(&[Action::OpenvsxNamespaceClaim]);
        s.claim_openvsx_namespace("vsx", "digital", "team-digital", &user())
            .await
            .expect("the grant permits it");

        let claim = s
            .namespace_claim("vsx", "digital")
            .await
            .unwrap()
            .expect("claimed");
        assert_eq!(claim.separator, '.', "OpenVSX namespaces are dotted (§4.1)");
        assert_eq!(claim.group_id, "team-digital");
        assert_eq!(claim.claimed_by.as_deref(), Some("u1"));
    }

    /// …and the claim then covers the extensions beneath it.
    ///
    /// The property that was unreachable before: `find_namespace` answers for a
    /// child, which is what makes `verified` meaningful and what lets
    /// `check_team_visibility` enforce anything on this ecosystem.
    #[tokio::test]
    async fn the_claim_covers_the_extensions_under_it() {
        let s = svc_granting(&[Action::OpenvsxNamespaceClaim]);
        s.claim_openvsx_namespace("vsx", "digital", "team-digital", &user())
            .await
            .unwrap();

        assert!(
            s.namespace_claim("vsx", "digital.pipeline-tools")
                .await
                .unwrap()
                .is_some(),
            "a dotted child must fall under its publisher's claim"
        );
        assert!(
            s.namespace_claim("vsx", "digitalpipeline")
                .await
                .unwrap()
                .is_none(),
            "and a name that merely starts with it must not — RFC 0011-bis §4.2"
        );
    }

    /// No other verb carries it, including every shared write verb.
    ///
    /// §4.2's argument for an ecosystem verb, asserted rather than assumed: a
    /// claim decides who may see every package beneath it once they are
    /// `team`-visible, so acquiring it by being able to publish would be §4.3's
    /// delegation bound broken by a spelling.
    #[tokio::test]
    async fn no_shared_write_verb_carries_a_namespace_claim() {
        let s = svc_granting(&[
            Action::ReleasesPublish,
            Action::ReleasesOverwrite,
            Action::ReleasesYank,
            Action::ReleasesDelete,
            Action::OwnersWrite,
        ]);
        let err = s
            .claim_openvsx_namespace("vsx", "digital", "team-digital", &user())
            .await
            .expect_err("only openvsx:namespace:claim carries this");
        assert!(
            matches!(err, CoreError::AccessDenied(ref m) if m.contains("openvsx:namespace:claim")),
            "{err:?}"
        );
        assert!(s.namespace_claim("vsx", "digital").await.unwrap().is_none());
    }

    /// An unclaimed namespace answers `None`, which is the protocol's
    /// `verified: false`.
    #[tokio::test]
    async fn an_unclaimed_namespace_is_not_verified() {
        let s = svc_granting(&[Action::OpenvsxNamespaceClaim]);
        assert!(s.namespace_claim("vsx", "nobody").await.unwrap().is_none());
    }

    /// Claiming twice is a conflict, not a silent takeover.
    ///
    /// The one thing a self-service protocol gets right and this must not lose:
    /// a namespace has one owner, and the second claimant is told so rather than
    /// replacing the first.
    #[tokio::test]
    async fn a_second_claim_is_refused() {
        let s = svc_granting(&[Action::OpenvsxNamespaceClaim]);
        s.claim_openvsx_namespace("vsx", "digital", "team-a", &user())
            .await
            .unwrap();
        assert!(s
            .claim_openvsx_namespace("vsx", "digital", "team-b", &user())
            .await
            .is_err());
        assert_eq!(
            s.namespace_claim("vsx", "digital")
                .await
                .unwrap()
                .unwrap()
                .group_id,
            "team-a"
        );
    }
}

/// RFC 0017 phase 2 — the version-tier listing filter.
///
/// RFC 0015 §4.4 rule 2's second half. These are the tests that turn
/// `filter_listing` and `package_visibility` from correct-and-idle into
/// correct-and-load-bearing: before the grants editor existed, the state they
/// decide was unreachable because nothing wrote a version-tier row.
mod version_grant_filter {
    use super::*;
    use crate::entities::{GrantMap, Node, RegistryGrants, RegistryKind, SubjectMatcher, Tier};
    use crate::ports::{GrantRepository, NodeKind, StoredGrant};
    use crate::services::hot_config::HotConfig;
    use tokio::sync::RwLock as AsyncRwLock;

    #[derive(Default)]
    struct Rows(
        AsyncRwLock<Vec<StoredGrant>>,
        std::sync::atomic::AtomicUsize,
    );

    impl Rows {
        /// How many *per-package* grant queries the funnel issued.
        ///
        /// The N+1 this counts is not a slow test — it is the §13.2 shape, one
        /// `LIKE` per package on every whole-registry document, for exactly the
        /// callers the config tiers do not already satisfy. A number is the only
        /// way to assert it stayed gone.
        fn per_package_queries(&self) -> usize {
            self.1.load(std::sync::atomic::Ordering::Relaxed)
        }

        /// Seeding the fixture goes through the same port, so the count starts
        /// after the setup rather than including it.
        fn reset_counter(&self) {
            self.1.store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[async_trait]
    impl GrantRepository for Rows {
        async fn grants_for(
            &self,
            registry: &str,
            package: &str,
            version: Option<&str>,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            let want = version.map(|v| crate::ports::version_node_key(package, v));
            Ok(self
                .0
                .read()
                .await
                .iter()
                .filter(|g| {
                    g.registry == registry
                        && match g.node_kind {
                            NodeKind::Package => g.node_key == package,
                            NodeKind::Version => Some(&g.node_key) == want.as_ref(),
                        }
                })
                .cloned()
                .collect())
        }
        async fn put_grant(&self, grant: StoredGrant) -> Result<(), CoreError> {
            self.0.write().await.push(grant);
            Ok(())
        }
        async fn delete_grant(
            &self,
            _: &str,
            _: NodeKind,
            _: &str,
            _: &SubjectMatcher,
        ) -> Result<(), CoreError> {
            Ok(())
        }
        async fn package_grants_in_registry(
            &self,
            registry: &str,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            Ok(self
                .0
                .read()
                .await
                .iter()
                .filter(|g| g.registry == registry && g.node_kind == NodeKind::Package)
                .cloned()
                .collect())
        }
        async fn version_grants_in_registry(
            &self,
            registry: &str,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            Ok(self
                .0
                .read()
                .await
                .iter()
                .filter(|g| g.registry == registry && g.node_kind == NodeKind::Version)
                .cloned()
                .collect())
        }
        async fn version_grants_for_package(
            &self,
            registry: &str,
            package: &str,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let prefix = format!("{package}@");
            Ok(self
                .0
                .read()
                .await
                .iter()
                .filter(|g| {
                    g.registry == registry
                        && g.node_kind == NodeKind::Version
                        && g.node_key.starts_with(&prefix)
                })
                .cloned()
                .collect())
        }
        async fn grants_on_node(
            &self,
            registry: &str,
            node_kind: NodeKind,
            node_key: &str,
        ) -> Result<Vec<StoredGrant>, CoreError> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(self
                .0
                .read()
                .await
                .iter()
                .filter(|g| {
                    g.registry == registry && g.node_kind == node_kind && g.node_key == node_key
                })
                .cloned()
                .collect())
        }
        async fn delete_package_grants(&self, _: &str, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
    }

    fn alice() -> Identity {
        Identity {
            user_id: Some("alice".to_owned()),
            role: Role::User,
            auth_provider: None,
            groups: vec![],
        }
    }

    /// A registry granting `verbs` to everyone. The interesting fixture grants
    /// `releases:list` and **not** `releases:read`, which is §4.4's opening
    /// configuration and the only one where a version filter can bite.
    fn registry_granting(verbs: &[Action]) -> RegistryGrants {
        RegistryGrants {
            kind: RegistryKind::Npm,
            registry: Node::new(
                Tier::Registry,
                "registry:npm",
                Some(GrantMap::new().grant(SubjectMatcher::Anyone, verbs.to_vec())),
            ),
            namespaces: Vec::new(),
        }
    }

    fn service(
        backend: Arc<InMemBackend>,
        grants: RegistryGrants,
        rows: Arc<Rows>,
    ) -> LocalRegistryService {
        let mut hot = HotConfig::default();
        hot.grants.insert("npm".to_owned(), Arc::new(grants));
        hot.grant_repo = Some(rows as Arc<dyn GrantRepository>);
        LocalRegistryService {
            backend,
            storage: Arc::new(NoopStorage),
            hot: new_hot_lock(hot),
            quota: None,
            ownership: None,
            team_namespace: None,
            sbom: None,
            explore_cache: None,
            package_repo: None,
            readme: None,
        }
    }

    fn three_versions() -> Arc<InMemBackend> {
        let backend = InMemBackend::arc();
        for v in ["1.0.0", "2.0.0", "3.0.0"] {
            backend.seed(pkg("npm", "pkg", v));
        }
        backend
    }

    fn version_grant(version: &str, actions: &[Action]) -> StoredGrant {
        StoredGrant {
            registry: "npm".to_owned(),
            node_kind: NodeKind::Version,
            node_key: crate::ports::version_node_key("pkg", version),
            subject: SubjectMatcher::User("alice".to_owned()),
            actions: actions.to_vec(),
            granted_by: None,
        }
    }

    async fn versions_seen(s: &LocalRegistryService) -> Vec<String> {
        s.load_visible_versions("npm", "pkg", &alice())
            .await
            .expect("listing")
            .into_iter()
            .map(|p| p.version)
            .collect()
    }

    /// A whole-registry document costs **no** per-package grant query.
    ///
    /// The filter asks two questions per package — the version rows under this
    /// name, and the package node. A document that walks every package asked
    /// them once per package, for exactly the callers the config tiers do not
    /// already satisfy: the ones this feature exists for. The fast path missed
    /// the case that matters, and a registry with no version-tier rows at all
    /// still paid one wasted `LIKE` per package.
    ///
    /// A count rather than a timing: the §13.2 shape is a query multiplier, and
    /// a multiplier is invisible at three packages and fatal at two hundred
    /// thousand.
    #[tokio::test]
    async fn a_whole_registry_document_costs_no_per_package_query() {
        let backend = InMemBackend::arc();
        for name in ["alpha", "beta", "gamma", "delta"] {
            for v in ["1.0.0", "2.0.0"] {
                backend.seed(pkg("npm", name, v));
            }
        }
        let rows = Arc::new(Rows::default());
        rows.put_grant(StoredGrant {
            node_key: crate::ports::version_node_key("beta", "2.0.0"),
            ..version_grant("2.0.0", &[Action::ReleasesRead])
        })
        .await
        .unwrap();
        // A package-tier read on each name — the shape the ownership projection
        // writes, and the one that makes the multiplier real. Such a name is
        // admitted by `Readable::with_package_grants`, so it reaches the filter;
        // but `filter_by_grants`' fast path resolves the *config* tiers only, so
        // it does not short-circuit, and each name then asked for its version
        // rows and its package node. Two queries per package, on a document that
        // walks every package.
        for name in ["alpha", "beta", "gamma", "delta"] {
            rows.put_grant(StoredGrant {
                registry: "npm".to_owned(),
                node_kind: NodeKind::Package,
                node_key: name.to_owned(),
                subject: SubjectMatcher::User("alice".to_owned()),
                actions: vec![Action::ReleasesRead],
                granted_by: None,
            })
            .await
            .unwrap();
        }
        rows.reset_counter();

        // The §4.4 caller: holds the list, holds the read nowhere, so the fast
        // path does not fire and every package reaches the querying branch.
        let s = service(
            backend,
            registry_granting(&[Action::ReleasesList]),
            rows.clone(),
        );
        let doc = s
            .get_rubygems_compact_versions("npm", &alice())
            .await
            .expect("document");

        for name in ["alpha", "beta", "gamma", "delta"] {
            assert!(
                doc.contains(name),
                "the positive control: every package the caller may read must be \
                 in the document, or 'no queries' passes against a document that \
                 names nothing — {doc:?}"
            );
        }
        assert_eq!(
            rows.per_package_queries(),
            0,
            "the document already fetches the registry's rows once to build \
             `Readable` and the cache key; asking again per package is the N+1"
        );
    }

    /// RFC 0017 §9's promise, asserted: with no version-tier row the funnel
    /// returns byte for byte what it returned before this feature existed.
    #[tokio::test]
    async fn with_no_version_row_the_listing_is_untouched() {
        let s = service(
            three_versions(),
            registry_granting(&[Action::ReleasesRead, Action::ReleasesList]),
            Arc::new(Rows::default()),
        );
        assert_eq!(versions_seen(&s).await, ["1.0.0", "2.0.0", "3.0.0"]);
    }

    /// The same, for the caller the filter is *about* — list but no read. Before
    /// a version row exists this caller still sees everything, because their
    /// read verdict is uniform across every version and the package tier is the
    /// whole answer.
    #[tokio::test]
    async fn a_list_only_caller_sees_everything_until_a_version_row_exists() {
        let s = service(
            three_versions(),
            registry_granting(&[Action::ReleasesList]),
            Arc::new(Rows::default()),
        );
        assert_eq!(versions_seen(&s).await, ["1.0.0", "2.0.0", "3.0.0"]);
    }

    /// §4.4 rule 2's second half, and the reason the filter had to ship with the
    /// writer: one version granted, three published, one listed.
    #[tokio::test]
    async fn a_version_grant_narrows_the_listing_to_what_the_caller_may_read() {
        let rows = Arc::new(Rows::default());
        rows.put_grant(version_grant("2.0.0", &[Action::ReleasesRead]))
            .await
            .unwrap();
        let s = service(
            three_versions(),
            registry_granting(&[Action::ReleasesList]),
            rows,
        );
        assert_eq!(versions_seen(&s).await, ["2.0.0"]);
    }

    /// Grants only widen (§4.3): a caller who holds the read at a config tier
    /// keeps every version, and a version row cannot take one away. This is also
    /// the fast path — it must reach that answer without consulting the rows at
    /// all.
    #[tokio::test]
    async fn a_registry_tier_read_is_not_narrowed_by_a_version_row() {
        let rows = Arc::new(Rows::default());
        rows.put_grant(version_grant("2.0.0", &[Action::ReleasesRead]))
            .await
            .unwrap();
        let s = service(
            three_versions(),
            registry_granting(&[Action::ReleasesRead, Action::ReleasesList]),
            rows,
        );
        assert_eq!(versions_seen(&s).await, ["1.0.0", "2.0.0", "3.0.0"]);
    }

    /// A package-tier read covers every version beneath it, for the same reason.
    #[tokio::test]
    async fn a_package_tier_read_is_not_narrowed_by_a_version_row() {
        let rows = Arc::new(Rows::default());
        rows.put_grant(version_grant("2.0.0", &[Action::ReleasesRead]))
            .await
            .unwrap();
        rows.put_grant(StoredGrant {
            registry: "npm".to_owned(),
            node_kind: NodeKind::Package,
            node_key: "pkg".to_owned(),
            subject: SubjectMatcher::User("alice".to_owned()),
            actions: vec![Action::ReleasesRead],
            granted_by: None,
        })
        .await
        .unwrap();
        let s = service(
            three_versions(),
            registry_granting(&[Action::ReleasesList]),
            rows,
        );
        assert_eq!(versions_seen(&s).await, ["1.0.0", "2.0.0", "3.0.0"]);
    }

    /// A version row for a *different* subject filters this caller out of it.
    #[tokio::test]
    async fn a_version_granted_to_someone_else_is_not_listed() {
        let rows = Arc::new(Rows::default());
        rows.put_grant(StoredGrant {
            subject: SubjectMatcher::User("bob".to_owned()),
            ..version_grant("2.0.0", &[Action::ReleasesRead])
        })
        .await
        .unwrap();
        let s = service(
            three_versions(),
            registry_granting(&[Action::ReleasesList]),
            rows,
        );
        assert!(versions_seen(&s).await.is_empty());
    }

    /// Two subjects on one version node, and the caller is the one written
    /// *first*. Every row for a version has to fold into one `GrantMap` — the
    /// way `chain::stored_nodes` folds them — or all but one subject's grant is
    /// dropped, and which one survives is whatever order the repository returned.
    /// That is a caller silently losing a version they were granted.
    #[tokio::test]
    async fn two_subjects_on_one_version_both_hold_it() {
        let rows = Arc::new(Rows::default());
        rows.put_grant(version_grant("2.0.0", &[Action::ReleasesRead]))
            .await
            .unwrap();
        rows.put_grant(StoredGrant {
            subject: SubjectMatcher::User("bob".to_owned()),
            ..version_grant("2.0.0", &[Action::ReleasesRead])
        })
        .await
        .unwrap();
        let s = service(
            three_versions(),
            registry_granting(&[Action::ReleasesList]),
            rows,
        );
        assert_eq!(
            versions_seen(&s).await,
            ["2.0.0"],
            "alice's row was written before bob's; keeping one row per version \
             would have let bob's overwrite it"
        );
    }

    /// The verb matters: a version row granting something other than the read
    /// does not make the version listable.
    #[tokio::test]
    async fn a_version_row_without_the_read_verb_does_not_list_it() {
        let rows = Arc::new(Rows::default());
        rows.put_grant(version_grant("2.0.0", &[Action::ReleasesYank]))
            .await
            .unwrap();
        let s = service(
            three_versions(),
            registry_granting(&[Action::ReleasesList]),
            rows,
        );
        assert!(versions_seen(&s).await.is_empty());
    }

    /// A version row on a *different package* must not reach this one — the
    /// `package@` boundary, which a bare prefix would cross.
    #[tokio::test]
    async fn a_row_on_a_similarly_named_package_does_not_leak_in() {
        let rows = Arc::new(Rows::default());
        rows.put_grant(StoredGrant {
            node_key: "pkg-internal@2.0.0".to_owned(),
            ..version_grant("2.0.0", &[Action::ReleasesRead])
        })
        .await
        .unwrap();
        let s = service(
            three_versions(),
            registry_granting(&[Action::ReleasesList]),
            rows,
        );
        assert_eq!(
            versions_seen(&s).await,
            ["1.0.0", "2.0.0", "3.0.0"],
            "`pkg-internal`'s row is not a row on `pkg`, so `pkg` has none and \
             the filter stays inert — a bare prefix match would instead have \
             narrowed this listing to the one version that row names"
        );
    }

    /// The filter runs after the others, so a version the caller may read but
    /// that is unlisted stays hidden: this narrows, it never widens.
    #[tokio::test]
    async fn the_filter_does_not_resurrect_an_unlisted_version() {
        let backend = InMemBackend::arc();
        backend.seed(PublishedPackage {
            unlisted: true,
            ..pkg("npm", "pkg", "2.0.0")
        });
        let rows = Arc::new(Rows::default());
        rows.put_grant(version_grant("2.0.0", &[Action::ReleasesRead]))
            .await
            .unwrap();
        let s = service(backend, registry_granting(&[Action::ReleasesList]), rows);
        assert!(versions_seen(&s).await.is_empty());
    }
}

/// Traversal spelled through percent-encoding.
///
/// `actix-router` decodes a path parameter twice — once while matching the
/// route with `%` protected, once in the `web::Path` extractor with nothing
/// protected — so `%252e%252e` reaches a handler as the literal `%2e%2e`, which
/// is not a `..` segment to `split('/')`. `url::Url::parse` *is* willing to read
/// it as one, so the validator has to decode before it decides.
mod encoded_traversal {
    use super::*;
    use crate::services::{has_traversal_after_decoding, validate_path_safe};

    /// Every spelling `url`'s own parser folds to a dot segment, plus the
    /// nested forms that survive a decoding round to become one.
    const ESCAPES: &[&str] = &[
        // Single-encoded: caught before this change, and still caught.
        "a/%2e%2e/b",
        "a/%2E%2E/b",
        // The mixed spellings the URL spec lists alongside `..`.
        "a/%2e./b",
        "a/.%2e/b",
        "a/%2E./b",
        // Double-encoded: what actually reaches a handler.
        "a/%252e%252e/b",
        "a/%252E%252E/b",
        // Triple, for the same reason.
        "a/%25252e%25252e/b",
        // The separator encoded rather than the dots.
        "a%2f..%2fb",
        "..%252fetc%252fpasswd",
        // Backslash, which WHATWG folds into `/` for special schemes.
        "a/%5c..%5cb",
    ];

    #[test]
    fn every_encoded_spelling_of_a_dot_segment_is_rejected() {
        for value in ESCAPES {
            let err = validate_path_safe("package name", value)
                .expect_err("must reject encoded traversal in {value}");
            assert!(
                matches!(err, CoreError::InvalidInput(_)),
                "{value} must be InvalidInput, got {err:?}"
            );
            assert!(
                has_traversal_after_decoding(value),
                "{value} must also be caught at the storage chokepoint"
            );
        }
    }

    /// The exact request shape from the review: three encoded dot segments walk
    /// out of `{owner}/{repo}/{ref}` into another repository.
    #[test]
    fn the_credentialed_cross_repo_read_is_rejected() {
        let artifact = "raw/main/%252e%252e/%252e%252e/%252e%252e/victim/private/main/.env";
        assert!(
            validate_path_safe("artifact", artifact).is_err(),
            "the cross-repository raw read must not validate"
        );
    }

    /// Nested past any plausible real coordinate: rejected rather than decoded
    /// forever.
    #[test]
    fn absurdly_nested_encoding_is_rejected() {
        let mut value = "..".to_owned();
        for _ in 0..12 {
            value = value.replace('%', "%25").replace("..", "%2e%2e");
        }
        assert!(validate_path_safe("package name", &value).is_err());
    }

    /// Not a traversal — these never leave the tree — but not distinct either:
    /// the filesystem backend joins the key verbatim, so `a//b`, `a/./b` and
    /// `a/b` are three coordinates and one file. Found by `fuzz_path_safe`.
    #[test]
    fn empty_and_dot_segments_alias_a_neighbour_and_are_rejected() {
        for value in [
            "a//b",
            "a/./b",
            ".",
            "./a",
            "a/.",
            "a/%2e/b",
            "a%2f%2fb",
            "a/%252e/b",
        ] {
            assert!(
                validate_path_safe("package name", value).is_err(),
                "{value} must be rejected"
            );
            assert!(
                has_traversal_after_decoding(value),
                "{value} must also be caught at the storage chokepoint"
            );
        }
        // A trailing separator is a coordinate that ends in `/` at the edge —
        // rejected on the raw value already, and here on the decoded one so
        // `a%2f` cannot sneak past as `a/`. At the chokepoint it is a prefix
        // (`delete_by_prefix("{key}/")`) and stays legal; `a//` does not.
        assert!(validate_path_safe("package name", "a%2f").is_err());
        assert!(!has_traversal_after_decoding("artifact:npm/a/"));
        assert!(has_traversal_after_decoding("artifact:npm/a//"));
    }

    /// The check decodes, so it must not start rejecting names that merely
    /// contain a percent escape or a dot.
    #[test]
    fn legitimate_coordinates_still_pass() {
        for value in [
            "@scope/name",
            "owner/repo",
            "com.example:lib",
            "raw/main/src/main.rs",
            "some-package",
            "a/.hidden/b",
            "a/.../b",
            // A file whose name genuinely contains an escape, once decoded to
            // something harmless.
            "docs/50%25-off.md",
            "raw/main/a%20b/c.txt",
        ] {
            validate_path_safe("package name", value)
                .unwrap_or_else(|e| panic!("{value} must stay valid, got {e:?}"));
            assert!(
                !has_traversal_after_decoding(value),
                "{value} must stay valid at the storage chokepoint too"
            );
        }
    }

    /// An encoded `/` collapses two coordinates onto one storage key exactly as
    /// a literal one does.
    #[test]
    fn a_version_may_not_smuggle_a_separator() {
        for value in ["1.0.0%2f2", "1.0.0%2F2", "1.0.0%252f2"] {
            assert!(
                validate_path_safe("version", value).is_err(),
                "{value} must not pass as a version"
            );
        }
        validate_path_safe("version", "1.0.0+build.1").expect("a real version still passes");
    }
}
