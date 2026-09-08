use std::collections::HashMap;
use std::sync::Arc;

use batlehub_core::services::new_hot_lock;
use uuid::Uuid;

use super::*;

/// Build a `ConfigReloadService` that uses a real temporary file on disk.
/// The file is initialised with `initial_content` and its path is returned
/// alongside the service so tests can inspect it later.
async fn make_svc_with_file(
    enabled: bool,
    initial_content: &str,
) -> (Arc<ConfigReloadService>, tempfile::NamedTempFile) {
    let builder: HotConfigBuilder = Arc::new(|_| anyhow::bail!("builder not used in this test"));
    make_svc_with_file_and_builder(enabled, initial_content, builder).await
}

// ── Fixture helpers ───────────────────────────────────────────────────────────
//
// `ConfigReloadParams`, `BuiltHotState` and `PendingReload` are wide structs
// whose fields these tests almost never vary. Written out at every call site
// they were the largest duplicated block in the crate, and adding a field meant
// editing a dozen copies — which is how `sumdb_map` came to be added in twelve
// places at once. Only what a test actually varies is passed in.

/// An `AccessConfig` granting nothing.
fn empty_access_config() -> crate::AccessConfig {
    crate::AccessConfig {
        anonymous: Default::default(),
        user: Default::default(),
        admin: Default::default(),
        groups: Default::default(),
        explore_anonymous: Default::default(),
        explore_user: Default::default(),
        explore_admin: Default::default(),
    }
}

/// A `HotConfig` with no registries and no policies.
fn empty_hot_config() -> batlehub_core::services::HotConfig {
    batlehub_core::services::HotConfig {
        registries: HashMap::new(),
        policies: HashMap::new(),
        ..Default::default()
    }
}

/// What a successful builder returns: `hot`, and empty everything else.
fn built_hot_state(hot: batlehub_core::services::HotConfig) -> BuiltHotState {
    BuiltHotState {
        hot,
        access: empty_access_config(),
        search_readmes: false,
        registry_map: crate::RegistryMap::new(HashMap::new()),
        registry_mode_map: crate::RegistryModeMap::new(HashMap::new()),
        upstream_map: crate::UpstreamMap::new(HashMap::new()),
        cargo_index_map: crate::CargoIndexMap::new(HashMap::new()),
        repo_signer_map: crate::RepoSignerMap::default(),
        vuln_db_map: crate::VulnDbMap::default(),
        sumdb_map: crate::SumDbMap::default(),
        registry_host_map: crate::RegistryHostMap::default(),
    }
}

/// The service's construction parameters. Three of the seventeen ever vary;
/// `config_overlays` is empty here and exercised by its own tests below.
fn reload_params(
    config_path: String,
    hot_reload_enabled: bool,
    builder: HotConfigBuilder,
) -> ConfigReloadParams {
    ConfigReloadParams {
        hot: new_hot_lock(empty_hot_config()),
        access: crate::new_access_lock(empty_access_config()),
        search: crate::new_search_lock(false),
        registry_map: crate::RegistryMap::new(HashMap::new()),
        registry_mode_map: crate::RegistryModeMap::new(HashMap::new()),
        upstream_map: crate::UpstreamMap::new(HashMap::new()),
        cargo_index_map: crate::CargoIndexMap::new(HashMap::new()),
        repo_signer_map: crate::RepoSignerMap::default(),
        vuln_db_map: crate::VulnDbMap::default(),
        sumdb_map: crate::SumDbMap::default(),
        registry_host_map: crate::RegistryHostMap::default(),
        proxy_trust: crate::middleware::ProxyTrust::default(),
        config_path,
        config_overlays: Vec::new(),
        config_change_repo: None,
        hot_reload_enabled,
        builder,
        banner: None,
    }
}

/// A staged reload that changes nothing — the base for `..empty_pending()`,
/// which is how every test here builds one.
fn empty_pending() -> PendingReload {
    PendingReload {
        id: Uuid::new_v4(),
        created_at: chrono::Utc::now(),
        expires_at: chrono::Utc::now() + chrono::Duration::seconds(600),
        source: ReloadSource::AdminRequest,
        diff: ReloadDiff::default(),
        content: None,
        new_hot: batlehub_core::services::HotConfig::default(),
        new_access: empty_access_config(),
        new_search_readmes: false,
        new_registry_map: crate::RegistryMap::new(HashMap::new()),
        new_registry_mode_map: crate::RegistryModeMap::new(HashMap::new()),
        new_upstream_map: crate::UpstreamMap::new(HashMap::new()),
        new_cargo_index_map: crate::CargoIndexMap::new(HashMap::new()),
        new_repo_signer_map: crate::RepoSignerMap::default(),
        new_vuln_db_map: crate::VulnDbMap::default(),
        new_sumdb_map: crate::SumDbMap::default(),
        new_registry_host_map: crate::RegistryHostMap::default(),
        new_proxy_trust: crate::middleware::ProxyTrust::default(),
        warnings: Vec::new(),
    }
}

/// Same as `make_svc_with_file` but lets the caller supply a `builder` that
/// actually succeeds, for tests that exercise `load_pending`'s diff computation.
async fn make_svc_with_file_and_builder(
    enabled: bool,
    initial_content: &str,
    builder: HotConfigBuilder,
) -> (Arc<ConfigReloadService>, tempfile::NamedTempFile) {
    use std::io::Write as _;
    let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
    tmp.write_all(initial_content.as_bytes()).expect("write");
    let path = tmp.path().to_str().unwrap().to_owned();

    let svc = Arc::new(ConfigReloadService::new(reload_params(
        path, enabled, builder,
    )));
    (svc, tmp)
}

// ── Shared helper ─────────────────────────────────────────────────────────────

pub(super) fn make_svc(enabled: bool) -> Arc<ConfigReloadService> {
    let builder: HotConfigBuilder = Arc::new(|_| anyhow::bail!("builder not used in unit tests"));
    Arc::new(ConfigReloadService::new(reload_params(
        "config.toml".to_owned(),
        enabled,
        builder,
    )))
}

// ── Basic guard tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn load_pending_returns_error_when_disabled() {
    let svc = make_svc(false);
    let err = svc
        .load_pending(ReloadSource::AdminRequest)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("disabled"));
}

#[tokio::test]
async fn apply_returns_error_when_disabled() {
    let svc = make_svc(false);
    let err = svc.apply("test").await.unwrap_err();
    assert!(err.to_string().contains("disabled"));
}

#[tokio::test]
async fn apply_returns_error_when_no_pending() {
    let svc = make_svc(true);
    let err = svc.apply("test").await.unwrap_err();
    assert!(err.to_string().contains("no pending"));
}

#[test]
fn discard_returns_false_when_nothing_pending() {
    let svc = make_svc(true);
    assert!(!svc.discard_pending());
}

#[test]
fn pending_snapshot_is_none_initially() {
    let svc = make_svc(true);
    assert!(svc.pending_snapshot().is_none());
}

#[test]
fn discard_returns_true_when_pending_exists() {
    let svc = make_svc(true);
    let pending = make_pending(600, false);
    *svc.pending.lock().unwrap() = Some(pending);

    assert!(svc.discard_pending());
    assert!(svc.pending_snapshot().is_none());
    assert!(!svc.discard_pending());
}

#[test]
fn expire_stale_clears_expired_pending() {
    let svc = make_svc(true);
    let expired = make_pending(-100, true);
    *svc.pending.lock().unwrap() = Some(expired);

    svc.expire_pending_if_stale();
    assert!(svc.pending_snapshot().is_none());
}

// ── Apply / reload tests ──────────────────────────────────────────────────────

#[tokio::test]
async fn apply_success_swaps_hot_config() {
    let svc = make_svc(true);
    // The service starts with no signers; the reload should swap in a new one.
    assert!(svc.repo_signer_map.get("apt").is_none());
    let seed = "9d61b19deffeba00aa3f3b6e3b0fe6a3f3a76b08e2c0a3f3b6e3b0fe6a3f3a76";
    let new_signers: HashMap<String, Arc<batlehub_adapters::repo::OpenPgpSigner>> = [(
        "apt".to_owned(),
        Arc::new(
            batlehub_adapters::repo::OpenPgpSigner::from_seed_hex(seed, 1_700_000_000, "BatleHub")
                .unwrap(),
        ),
    )]
    .into();
    let new_hot = batlehub_core::services::HotConfig {
        registries: HashMap::new(),
        policies: HashMap::new(),
        max_artifact_size_bytes: Some(42),
        ..Default::default()
    };
    let new_access = crate::AccessConfig {
        anonymous: Default::default(),
        user: Default::default(),
        admin: Default::default(),
        groups: Default::default(),
        explore_anonymous: Default::default(),
        explore_user: Default::default(),
        explore_admin: Default::default(),
    };
    let pending = PendingReload {
        id: Uuid::new_v4(),
        created_at: chrono::Utc::now(),
        expires_at: chrono::Utc::now() + chrono::Duration::seconds(600),
        source: ReloadSource::AdminRequest,
        diff: ReloadDiff {
            added_registries: vec!["new-reg".to_string()],
            ..Default::default()
        },
        content: None,
        new_hot,
        new_access,
        new_search_readmes: false,
        new_registry_map: crate::RegistryMap::new(HashMap::new()),
        new_registry_mode_map: crate::RegistryModeMap::new(HashMap::new()),
        new_upstream_map: crate::UpstreamMap::new(HashMap::new()),
        new_cargo_index_map: crate::CargoIndexMap::new(HashMap::new()),
        new_repo_signer_map: crate::RepoSignerMap::from(new_signers),
        new_vuln_db_map: crate::VulnDbMap::default(),
        new_sumdb_map: crate::SumDbMap::default(),
        new_registry_host_map: crate::RegistryHostMap::default(),
        new_proxy_trust: crate::middleware::ProxyTrust::from_config(Some(&[
            "10.42.0.0/16".to_owned()
        ])),
        warnings: Vec::new(),
    };
    // The handle the app's middleware would hold. It must see the swap, or host
    // routing can go live while trust stays at its startup value.
    let live_trust = svc.proxy_trust.clone();
    assert!(!live_trust.is_configured());
    *svc.pending.lock().unwrap() = Some(pending);

    let diff = svc.apply("test-user").await.unwrap();

    assert_eq!(diff.added_registries, vec!["new-reg"]);
    assert!(svc.pending_snapshot().is_none());
    let hot = svc.hot.read().await;
    assert_eq!(hot.max_artifact_size_bytes, Some(42));
    // The deb/rpm signer map was swapped in by the same apply().
    assert!(svc.repo_signer_map.get("apt").is_some());
    // …and so was the proxy-trust policy.
    assert!(live_trust.is_configured());
    assert_eq!(
        live_trust.verdict_for(Some("10.42.7.1".parse().unwrap())),
        crate::middleware::PeerTrust::Trusted
    );
}

#[tokio::test]
async fn reload_immediate_applies_config() {
    let tmp_path = format!("/tmp/batlehub_reload_test_{}.toml", Uuid::new_v4());
    std::fs::write(
        &tmp_path,
        "[server]\nhost = \"127.0.0.1\"\nport = 8080\n\n[database]\ntype = \"postgresql\"\nurl = \"postgresql://user:pass@localhost/db\"\n\n[storage]\ntype = \"filesystem\"\npath = \"./tmp\"\n",
    )
    .unwrap();

    let builder: HotConfigBuilder = Arc::new(|_| {
        Ok(built_hot_state(batlehub_core::services::HotConfig {
            registries: HashMap::new(),
            policies: HashMap::new(),
            max_artifact_size_bytes: Some(999),
            ..Default::default()
        }))
    });
    let svc = Arc::new(ConfigReloadService::new(reload_params(
        tmp_path.clone(),
        true,
        builder,
    )));

    let diff = svc.reload_immediate("test").await.unwrap();
    assert!(diff.added_registries.is_empty());
    assert!(svc.pending_snapshot().is_none());
    let hot = svc.hot.read().await;
    assert_eq!(hot.max_artifact_size_bytes, Some(999));

    let _ = std::fs::remove_file(tmp_path);
}

#[tokio::test]
async fn list_changes_returns_error_without_database() {
    let svc = make_svc(true);
    let err = svc.list_changes(0, 10).await.unwrap_err();
    assert!(err.to_string().contains("database not configured"));
}

#[tokio::test]
async fn apply_expired_pending_returns_error() {
    let svc = make_svc(true);
    let expired = make_pending(-1, true);
    *svc.pending.lock().unwrap() = Some(expired);

    let err = svc.apply("test").await.unwrap_err();
    assert!(err.to_string().contains("expired"), "got: {err}");
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn make_pending(expires_offset_secs: i64, already_expired: bool) -> PendingReload {
    let hot = empty_hot_config();
    let access = empty_access_config();
    let created_at = if already_expired {
        chrono::Utc::now() - chrono::Duration::seconds(700)
    } else {
        chrono::Utc::now()
    };
    PendingReload {
        id: Uuid::new_v4(),
        created_at,
        expires_at: chrono::Utc::now() + chrono::Duration::seconds(expires_offset_secs),
        source: if already_expired {
            ReloadSource::FileWatcher
        } else {
            ReloadSource::AdminRequest
        },
        diff: ReloadDiff::default(),
        content: None,
        new_hot: hot,
        new_access: access,
        new_search_readmes: false,
        ..empty_pending()
    }
}

// ── config_content + load_pending_from_content + apply disk-write ─────────────

#[tokio::test]
async fn config_content_reads_file_from_disk() {
    let (svc, _tmp) = make_svc_with_file(true, "initial = true\n").await;
    let content = svc.config_content().await.expect("read");
    assert_eq!(content, "initial = true\n");
}

#[tokio::test]
async fn config_content_returns_error_for_missing_file() {
    let svc = make_svc(true); // uses non-existent "config.toml"
    let err = svc.config_content().await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[tokio::test]
async fn load_pending_from_content_returns_error_when_disabled() {
    let svc = make_svc(false);
    let err = svc
        .load_pending_from_content("[servers]\n", ReloadSource::AdminRequest)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("disabled"));
}

#[tokio::test]
async fn load_pending_from_content_returns_error_for_invalid_toml() {
    let svc = make_svc(true);
    let err = svc
        .load_pending_from_content("not valid toml ::::", ReloadSource::AdminRequest)
        .await
        .unwrap_err();
    // The error comes from TOML parsing — just verify it propagates.
    assert!(!err.to_string().is_empty());
}

/// Regression test: a file-watcher event whose config content is structurally a
/// no-op per `compute_diff` (no registry added/removed, no top-level access/limits
/// change) must still store a pending reload. `compute_diff` cannot see changes to
/// an *existing* registry's fields (`changed_registries` is always empty — see its
/// doc comment), so treating "empty diff" as "nothing changed" would silently drop
/// real edits to an existing registry's config.
#[tokio::test]
async fn load_pending_stores_pending_even_when_diff_is_structurally_noop() {
    let builder: HotConfigBuilder = Arc::new(|_| {
        Ok(built_hot_state(
            batlehub_core::services::HotConfig::default(),
        ))
    });
    let minimal_config = r#"
        [server]
        host = "127.0.0.1"
        port = 8080

        [database]
        type = "postgresql"
        url = "postgresql://user:pass@localhost/db"

        [storage]
        type = "filesystem"
        path = "./tmp"
        "#;
    let (svc, _tmp) = make_svc_with_file_and_builder(true, minimal_config, builder).await;

    let diff = svc
        .load_pending(ReloadSource::FileWatcher)
        .await
        .expect("load_pending");

    assert!(diff.is_noop());
    assert!(
        svc.pending_snapshot().is_some(),
        "a structurally-noop diff must not suppress storing the pending reload"
    );
}

/// A builder that succeeds with empty state, for tests that only care about the
/// staging bookkeeping rather than what gets built.
fn noop_builder() -> HotConfigBuilder {
    Arc::new(|_| {
        Ok(built_hot_state(
            batlehub_core::services::HotConfig::default(),
        ))
    })
}

const MINIMAL_CONFIG: &str = r#"
[server]
host = "127.0.0.1"
port = 8080

[database]
type = "postgresql"
url = "postgresql://user:pass@localhost/db"

[storage]
type = "filesystem"
path = "./tmp"
"#;

/// `pending_created` is the only thing that distinguishes "staged, go apply it"
/// from "nothing to stage": both return `Ok` with an empty diff, and the second
/// only reveals itself when the follow-up apply fails.
#[tokio::test]
async fn resubmitting_identical_content_reports_that_nothing_was_staged() {
    let (svc, _tmp) = make_svc_with_file_and_builder(true, MINIMAL_CONFIG, noop_builder()).await;

    let first = svc
        .load_pending_from_content(MINIMAL_CONFIG, ReloadSource::AdminRequest)
        .await
        .expect("first submission");
    assert!(first.pending_created, "the first submission stages");
    assert!(svc.pending_snapshot().is_some());

    // The admin discards it, then re-submits the very same bytes from the editor
    // — the shape of the real complaint: the file watcher had already loaded this
    // content, so the dedup fires even though nothing is staged any more.
    assert!(svc.discard_pending());
    let second = svc
        .load_pending_from_content(MINIMAL_CONFIG, ReloadSource::AdminRequest)
        .await
        .expect("re-submission still succeeds");

    assert!(
        !second.pending_created,
        "identical content stages nothing, and must say so"
    );
    assert!(svc.pending_snapshot().is_none());
    let err = svc.apply("test").await.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<ReloadApplyError>(),
            Some(ReloadApplyError::NoPendingReload)
        ),
        "…which is exactly what applying would have hit: {err}"
    );
}

#[tokio::test]
async fn validate_content_never_reports_a_staged_pending() {
    let (svc, _tmp) = make_svc_with_file_and_builder(true, MINIMAL_CONFIG, noop_builder()).await;

    let outcome = svc
        .validate_content(MINIMAL_CONFIG)
        .await
        .expect("validate");

    assert!(
        !outcome.pending_created,
        "validate is a dry run by contract"
    );
    assert!(svc.pending_snapshot().is_none());
}

/// Regression test: a *repeated* file-watcher event whose raw config bytes are
/// byte-identical to the previous load attempt (e.g. a touch or atomic-save
/// rewrite) must not rebuild or replace the existing pending reload — this is the
/// actual case the file watcher hits repeatedly and needs to dedup.
#[tokio::test]
async fn load_pending_skips_rebuild_when_raw_content_is_unchanged() {
    let builder: HotConfigBuilder = Arc::new(|_| {
        Ok(built_hot_state(
            batlehub_core::services::HotConfig::default(),
        ))
    });
    let minimal_config = r#"
        [server]
        host = "127.0.0.1"
        port = 8080

        [database]
        type = "postgresql"
        url = "postgresql://user:pass@localhost/db"

        [storage]
        type = "filesystem"
        path = "./tmp"
        "#;
    let (svc, _tmp) = make_svc_with_file_and_builder(true, minimal_config, builder).await;

    svc.load_pending(ReloadSource::FileWatcher)
        .await
        .expect("first load_pending");
    let first_id = svc
        .pending_snapshot()
        .expect("first load stores a pending")
        .id;

    // File watcher fires again; the file on disk hasn't actually changed.
    let second = svc
        .load_pending(ReloadSource::FileWatcher)
        .await
        .expect("second load_pending");

    assert!(second.is_noop());
    assert_eq!(
        svc.pending_snapshot().expect("pending left untouched").id,
        first_id,
        "unchanged raw content must not replace the existing pending reload"
    );
}

#[tokio::test]
async fn load_pending_from_content_stores_raw_content_in_pending() {
    let raw = "# valid minimal config\n";
    let svc = make_svc(true);
    // Override builder to succeed without touching the file.
    let hot = batlehub_core::services::HotConfig {
        ..Default::default()
    };
    let access = empty_access_config();
    // Inject a pending with content set, simulating a successful parse.
    let pending = PendingReload {
        id: Uuid::new_v4(),
        created_at: chrono::Utc::now(),
        expires_at: chrono::Utc::now() + chrono::Duration::seconds(600),
        source: ReloadSource::AdminRequest,
        diff: ReloadDiff::default(),
        content: Some(raw.to_owned()),
        new_hot: hot,
        new_access: access,
        new_search_readmes: false,
        ..empty_pending()
    };
    *svc.pending.lock().unwrap() = Some(pending);

    let stored = svc.pending.lock().unwrap();
    assert_eq!(stored.as_ref().unwrap().content.as_deref(), Some(raw));
}

#[tokio::test]
async fn apply_writes_editor_content_to_disk() {
    let initial = "# initial\n";
    let new_toml = "# after editor apply\n";
    let (svc, tmp) = make_svc_with_file(true, initial).await;

    // Manually set a pending reload with content (as load_pending_from_content would).
    let pending = PendingReload {
        id: Uuid::new_v4(),
        created_at: chrono::Utc::now(),
        expires_at: chrono::Utc::now() + chrono::Duration::seconds(600),
        source: ReloadSource::AdminRequest,
        diff: ReloadDiff::default(),
        content: Some(new_toml.to_owned()),
        ..empty_pending()
    };
    *svc.pending.lock().unwrap() = Some(pending);

    svc.apply("test-user").await.unwrap();

    // Verify the file now contains the editor-submitted content.
    let on_disk = tokio::fs::read_to_string(tmp.path()).await.unwrap();
    assert_eq!(on_disk, new_toml);
    // And config_content() returns the updated file.
    let via_svc = svc.config_content().await.unwrap();
    assert_eq!(via_svc, new_toml);
}

/// The write-back replaces the file by `rename`, which swaps in a whole new
/// inode — so the mode has to be carried over explicitly. `config.toml` holds
/// `database.url` with its credentials, and `NamedTempFile` creates at 0600, so a
/// regression to `File::create`'s default 0644 would be a real widening of a
/// secret-bearing file.
#[cfg(unix)]
#[tokio::test]
async fn apply_preserves_the_config_file_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let (svc, tmp) = make_svc_with_file(true, "# initial\n").await;
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600))
        .expect("chmod 600");

    *svc.pending.lock().unwrap() = Some(PendingReload {
        content: Some("# after editor apply\n".to_owned()),
        ..empty_pending()
    });
    svc.apply("test-user").await.unwrap();

    let mode = std::fs::metadata(tmp.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "rename must not widen the config file's mode");
}

/// A failed or successful write must not leave the staging file next to the
/// config — an operator listing `/etc/batlehub` should not find debris, and the
/// file watcher should not be handed a second `.toml`-adjacent file to notice.
#[tokio::test]
async fn apply_leaves_no_temp_file_beside_the_config() {
    let (svc, tmp) = make_svc_with_file(true, "# initial\n").await;
    let dir = tmp.path().parent().unwrap().to_owned();
    let name = tmp
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    *svc.pending.lock().unwrap() = Some(PendingReload {
        content: Some("# after editor apply\n".to_owned()),
        ..empty_pending()
    });
    svc.apply("test-user").await.unwrap();

    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|f| f.starts_with(&format!(".{name}.")) && f.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "staging files left behind: {leftovers:?}"
    );
}

/// A write-back to a path whose directory does not exist must fail *without*
/// taking down the reload: the new config is already live in memory, and the
/// documented contract of this branch is a warning, not an error.
#[tokio::test]
async fn apply_survives_an_unwritable_config_path() {
    let builder: HotConfigBuilder = Arc::new(|_| anyhow::bail!("builder not used in this test"));
    let svc = Arc::new(ConfigReloadService::new(reload_params(
        "/nonexistent-dir-for-test/config.toml".to_owned(),
        true,
        builder,
    )));

    *svc.pending.lock().unwrap() = Some(PendingReload {
        content: Some("# unwritable\n".to_owned()),
        ..empty_pending()
    });

    svc.apply("test-user")
        .await
        .expect("a failed disk write must not fail the reload");
}

#[tokio::test]
async fn apply_with_no_content_leaves_file_unchanged() {
    let initial = "# unchanged\n";
    let (svc, tmp) = make_svc_with_file(true, initial).await;

    let pending = PendingReload {
        id: Uuid::new_v4(),
        created_at: chrono::Utc::now(),
        expires_at: chrono::Utc::now() + chrono::Duration::seconds(600),
        source: ReloadSource::FileWatcher,
        diff: ReloadDiff::default(),
        content: None, // file-watcher path — no content to write back
        ..empty_pending()
    };
    *svc.pending.lock().unwrap() = Some(pending);
    svc.apply("test-user").await.unwrap();

    let on_disk = tokio::fs::read_to_string(tmp.path()).await.unwrap();
    assert_eq!(on_disk, initial);
}

// ── Layered config files ──────────────────────────────────────────────────────
//
// Two files, one process: the credentials layer has a different lifecycle from
// the rest of the configuration, and both have to hot-reload. What these tests
// pin down is the part that is easy to get subtly wrong — that a change to the
// *overlay* alone still produces a reload, and that the overlay never reaches
// the editor's read or write path.

/// A service over a primary file plus one overlay, both real files on disk.
async fn make_svc_with_layers(
    primary_content: &str,
    overlay_content: &str,
    builder: HotConfigBuilder,
) -> (
    Arc<ConfigReloadService>,
    tempfile::NamedTempFile,
    tempfile::NamedTempFile,
) {
    use std::io::Write as _;
    let mut primary = tempfile::NamedTempFile::new().expect("temp file");
    primary
        .write_all(primary_content.as_bytes())
        .expect("write");
    let mut overlay = tempfile::NamedTempFile::new().expect("temp file");
    overlay
        .write_all(overlay_content.as_bytes())
        .expect("write");

    let mut params = reload_params(primary.path().to_str().unwrap().to_owned(), true, builder);
    params.config_overlays = vec![overlay.path().to_str().unwrap().to_owned()];
    (Arc::new(ConfigReloadService::new(params)), primary, overlay)
}

/// The credentials layer in these tests: the piece the primary deliberately
/// does not carry.
const CREDENTIALS_LAYER: &str = r#"
[database]
url = "postgresql://real:s3cr3t@db/batlehub"
"#;

/// The reason the feature exists. A rotation touches only the overlay; the
/// primary's bytes are identical, and the dedup that exists for `touch` and
/// atomic saves must not read that as "nothing changed".
#[tokio::test]
async fn a_change_to_the_overlay_alone_is_not_deduplicated_away() {
    let (svc, _primary, overlay) =
        make_svc_with_layers(MINIMAL_CONFIG, CREDENTIALS_LAYER, noop_builder()).await;

    // First load: establishes the baseline the dedup compares against.
    svc.load_pending(ReloadSource::FileWatcher)
        .await
        .expect("first load failed");
    let first_id = svc
        .pending_snapshot()
        .expect("first load stores a pending")
        .id;

    // Rotate the credential. Nothing about the primary changes.
    tokio::fs::write(
        overlay.path(),
        "[database]\nurl = \"postgresql://real:rotated@db/batlehub\"\n",
    )
    .await
    .expect("overlay rewrite failed");

    svc.load_pending(ReloadSource::FileWatcher)
        .await
        .expect("reload after rotation failed");

    assert_ne!(
        svc.pending_snapshot()
            .expect("the rotation stages a pending")
            .id,
        first_id,
        "a credentials-only change was deduplicated away, so the rotation never took effect"
    );
}

/// The other half: an untouched overlay must still dedup, or every spurious
/// watcher event on the primary rebuilds the world.
#[tokio::test]
async fn an_unchanged_pair_of_layers_still_deduplicates() {
    let (svc, _primary, _overlay) =
        make_svc_with_layers(MINIMAL_CONFIG, CREDENTIALS_LAYER, noop_builder()).await;

    svc.load_pending(ReloadSource::FileWatcher)
        .await
        .expect("first load failed");
    let first_id = svc
        .pending_snapshot()
        .expect("first load stores a pending")
        .id;

    let second = svc
        .load_pending(ReloadSource::FileWatcher)
        .await
        .expect("second load failed");

    assert!(second.is_noop());
    assert_eq!(
        svc.pending_snapshot().expect("pending left untouched").id,
        first_id,
        "a byte-identical rewrite of both layers replaced the pending reload"
    );
}

/// The editor reads the primary and nothing else. This is what keeps the
/// credentials layer out of the admin API's responses.
#[tokio::test]
async fn the_editor_reads_the_primary_layer_only() {
    let (svc, _primary, _overlay) =
        make_svc_with_layers(MINIMAL_CONFIG, CREDENTIALS_LAYER, noop_builder()).await;

    let served = svc.config_content().await.expect("config_content failed");

    assert_eq!(served, MINIMAL_CONFIG);
    assert!(
        !served.contains("s3cr3t"),
        "the credentials layer was served to the config editor"
    );
}

/// And it writes the primary and nothing else, so a save cannot inline the
/// overlay's secrets into the file the editor owns.
#[tokio::test]
async fn applying_editor_content_leaves_the_overlay_untouched() {
    let (svc, primary, overlay) =
        make_svc_with_layers(MINIMAL_CONFIG, CREDENTIALS_LAYER, noop_builder()).await;

    let edited = format!("{MINIMAL_CONFIG}\n# edited by the console\n");
    svc.load_pending_from_content(&edited, ReloadSource::AdminRequest)
        .await
        .expect("editor load failed");
    svc.apply("test-user").await.expect("apply failed");

    let primary_on_disk = tokio::fs::read_to_string(primary.path()).await.unwrap();
    assert_eq!(primary_on_disk, edited);
    assert!(
        !primary_on_disk.contains("s3cr3t"),
        "the editor's write-back copied the credentials layer into the primary file"
    );

    let overlay_on_disk = tokio::fs::read_to_string(overlay.path()).await.unwrap();
    assert_eq!(
        overlay_on_disk, CREDENTIALS_LAYER,
        "the editor rewrote a layer it does not own"
    );
}

/// The editor's preview has to describe the config that would actually be in
/// force, which means validating the merge rather than the primary alone. A
/// primary that is incomplete on its own is the normal case here.
#[tokio::test]
async fn validate_content_validates_the_merged_document() {
    // The primary carries no [database] at all — invalid by itself, valid once
    // the credentials layer is merged over it.
    const NO_DATABASE: &str = r#"
[server]
host = "127.0.0.1"
port = 8080

[storage]
type = "filesystem"
path = "./tmp"
"#;
    const WHOLE_DATABASE_BLOCK: &str = r#"
[database]
type = "postgresql"
url = "postgresql://real:s3cr3t@db/batlehub"
"#;
    let (svc, _primary, _overlay) =
        make_svc_with_layers(NO_DATABASE, WHOLE_DATABASE_BLOCK, noop_builder()).await;

    svc.validate_content(NO_DATABASE)
        .await
        .expect("a primary that only validates once merged was refused");
}

/// An overlay that has gone missing is an error naming the file, not a silent
/// fallback to the primary. Falling back would drop every credential the
/// process was running with and look like a successful reload.
#[tokio::test]
async fn a_missing_overlay_fails_the_reload_by_name() {
    let (svc, _primary, overlay) =
        make_svc_with_layers(MINIMAL_CONFIG, CREDENTIALS_LAYER, noop_builder()).await;
    let overlay_path = overlay.path().to_owned();
    drop(overlay);

    let err = svc
        .load_pending(ReloadSource::FileWatcher)
        .await
        .expect_err("a missing overlay was accepted");

    let msg = format!("{err:#}");
    assert!(
        msg.contains(&overlay_path.to_string_lossy().to_string()),
        "the error does not name the missing overlay: {msg}"
    );
}

/// A deleted overlay must not fingerprint like an empty one. The dedup runs
/// before the parse, so an overlay that was empty and is now gone would
/// otherwise be read as "nothing changed" and the error never reported.
#[tokio::test]
async fn a_deleted_overlay_is_not_mistaken_for_an_empty_one() {
    let (svc, _primary, overlay) = make_svc_with_layers(MINIMAL_CONFIG, "", noop_builder()).await;

    svc.load_pending(ReloadSource::FileWatcher)
        .await
        .expect("first load failed");

    let overlay_path = overlay.path().to_owned();
    drop(overlay);

    let err = svc
        .load_pending(ReloadSource::FileWatcher)
        .await
        .expect_err("a deleted overlay was deduplicated away as unchanged");
    assert!(
        format!("{err:#}").contains(&overlay_path.to_string_lossy().to_string()),
        "the error does not name the deleted overlay: {err:#}"
    );
}
