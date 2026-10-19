//! Integration tests for `PostgresLocalRegistry`.
//!
//! Requires a running PostgreSQL instance. Set `DATABASE_URL` to opt in:
//!
//!   task test:pg-local-registry                      # starts Postgres via Podman automatically
//!   DATABASE_URL=postgresql://batlehub:changeme@localhost/batlehub \
//!     cargo test -p batlehub-adapters --test local_registry

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use sqlx::PgPool;

use batlehub_adapters::local_registry::PostgresLocalRegistry;
use batlehub_core::{entities::PublishedPackage, error::CoreError, ports::LocalRegistryBackend};

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

static TEST_ID: AtomicU64 = AtomicU64::new(0);

struct TestRegistry {
    reg: PostgresLocalRegistry,
    /// Unique registry name per test to avoid row collisions across parallel tests.
    registry: String,
}

async fn make_registry(url: &str) -> TestRegistry {
    let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let pool = PgPool::connect(url).await.expect("connect to postgres");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("run migrations");
    TestRegistry {
        reg: PostgresLocalRegistry::new(pool),
        registry: format!("test-reg-{pid}-{id}"),
    }
}

fn pkg(registry: &str, name: &str, version: &str) -> PublishedPackage {
    PublishedPackage {
        registry: registry.to_owned(),
        name: name.to_owned(),
        version: version.to_owned(),
        checksum: format!("{:064x}", 0u64),
        yanked: false,
        deprecated: false,
        deprecation_message: None,
        unlisted: false,
        index_metadata: serde_json::json!({ "name": name, "version": version }),
        published_at: Utc::now(),
        published_by: Some("test-user".to_owned()),
        signature_bytes: None,
        signature_type: None,
        visibility: Default::default(),
        retention_keep: false,
    }
}

// ── publish ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn publish_inserts_package() {
    let Some(url) = db_url() else { return };
    let t = make_registry(&url).await;
    t.reg
        .publish(pkg(&t.registry, "my-crate", "1.0.0"))
        .await
        .unwrap();
    t.reg
        .commit_publish(&t.registry, "my-crate", "1.0.0")
        .await
        .unwrap();
    let versions = t.reg.get_versions(&t.registry, "my-crate").await.unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version, "1.0.0");
}

#[tokio::test]
async fn publish_duplicate_version_returns_conflict() {
    let Some(url) = db_url() else { return };
    let t = make_registry(&url).await;
    t.reg
        .publish(pkg(&t.registry, "dup", "1.0.0"))
        .await
        .unwrap();
    t.reg
        .commit_publish(&t.registry, "dup", "1.0.0")
        .await
        .unwrap();
    let err = t
        .reg
        .publish(pkg(&t.registry, "dup", "1.0.0"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::Conflict(_)),
        "expected Conflict, got {err:?}"
    );
}

#[tokio::test]
async fn publish_different_versions_are_independent() {
    let Some(url) = db_url() else { return };
    let t = make_registry(&url).await;
    for v in ["1.0.0", "1.1.0", "2.0.0"] {
        t.reg.publish(pkg(&t.registry, "multi", v)).await.unwrap();
        t.reg.commit_publish(&t.registry, "multi", v).await.unwrap();
    }
    let versions = t.reg.get_versions(&t.registry, "multi").await.unwrap();
    assert_eq!(versions.len(), 3);
}

#[tokio::test]
async fn publish_different_registries_do_not_interfere() {
    let Some(url) = db_url() else { return };
    let t = make_registry(&url).await;
    let other = format!("{}-other", t.registry);
    t.reg
        .publish(pkg(&t.registry, "shared-name", "1.0.0"))
        .await
        .unwrap();
    t.reg
        .commit_publish(&t.registry, "shared-name", "1.0.0")
        .await
        .unwrap();
    t.reg
        .publish(pkg(&other, "shared-name", "1.0.0"))
        .await
        .unwrap();
    t.reg
        .commit_publish(&other, "shared-name", "1.0.0")
        .await
        .unwrap();
    let v1 = t
        .reg
        .get_versions(&t.registry, "shared-name")
        .await
        .unwrap();
    let v2 = t.reg.get_versions(&other, "shared-name").await.unwrap();
    assert_eq!(v1.len(), 1);
    assert_eq!(v2.len(), 1);
}

// ── get_versions ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_versions_returns_empty_for_unknown_package() {
    let Some(url) = db_url() else { return };
    let t = make_registry(&url).await;
    let versions = t.reg.get_versions(&t.registry, "ghost").await.unwrap();
    assert!(versions.is_empty());
}

#[tokio::test]
async fn get_versions_returns_in_published_at_order() {
    let Some(url) = db_url() else { return };
    let t = make_registry(&url).await;
    // Publish a few versions with small sleeps to guarantee ordering
    for v in ["0.1.0", "0.2.0", "1.0.0"] {
        t.reg.publish(pkg(&t.registry, "ordered", v)).await.unwrap();
        t.reg
            .commit_publish(&t.registry, "ordered", v)
            .await
            .unwrap();
    }
    let versions = t.reg.get_versions(&t.registry, "ordered").await.unwrap();
    let vers: Vec<&str> = versions.iter().map(|v| v.version.as_str()).collect();
    assert_eq!(vers, ["0.1.0", "0.2.0", "1.0.0"]);
}

#[tokio::test]
async fn list_registry_versions_is_get_versions_for_every_name_in_one_read() {
    let Some(url) = db_url() else { return };
    let t = make_registry(&url).await;
    let other = make_registry(&url).await;
    for (name, version) in [("b", "1.0.0"), ("a", "2.0.0"), ("a", "1.0.0")] {
        t.reg
            .publish(pkg(&t.registry, name, version))
            .await
            .unwrap();
        t.reg
            .commit_publish(&t.registry, name, version)
            .await
            .unwrap();
    }
    // Pending, and another registry's: neither is listed.
    t.reg.publish(pkg(&t.registry, "c", "1.0.0")).await.unwrap();
    other
        .reg
        .publish(pkg(&other.registry, "a", "9.0.0"))
        .await
        .unwrap();
    other
        .reg
        .commit_publish(&other.registry, "a", "9.0.0")
        .await
        .unwrap();

    let all = t.reg.list_registry_versions(&t.registry).await.unwrap();
    let coords: Vec<_> = all
        .iter()
        .map(|p| (p.name.as_str(), p.version.as_str()))
        .collect();
    assert_eq!(coords, [("a", "2.0.0"), ("a", "1.0.0"), ("b", "1.0.0")]);
    assert_eq!(all[0].index_metadata["version"], "2.0.0");
}

#[tokio::test]
async fn get_versions_preserves_index_metadata() {
    let Some(url) = db_url() else { return };
    let t = make_registry(&url).await;
    let mut p = pkg(&t.registry, "meta-pkg", "1.0.0");
    p.index_metadata = serde_json::json!({ "custom_key": "custom_value", "num": 42 });
    t.reg.publish(p).await.unwrap();
    t.reg
        .commit_publish(&t.registry, "meta-pkg", "1.0.0")
        .await
        .unwrap();
    let versions = t.reg.get_versions(&t.registry, "meta-pkg").await.unwrap();
    assert_eq!(versions[0].index_metadata["custom_key"], "custom_value");
    assert_eq!(versions[0].index_metadata["num"], 42);
}

// ── yank / unyank ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn yank_sets_yanked_flag_and_updates_metadata() {
    let Some(url) = db_url() else { return };
    let t = make_registry(&url).await;
    t.reg
        .publish(pkg(&t.registry, "yank-me", "1.0.0"))
        .await
        .unwrap();
    t.reg
        .commit_publish(&t.registry, "yank-me", "1.0.0")
        .await
        .unwrap();
    t.reg.yank(&t.registry, "yank-me", "1.0.0").await.unwrap();
    let versions = t.reg.get_versions(&t.registry, "yank-me").await.unwrap();
    assert!(versions[0].yanked, "yanked DB column must be TRUE");
    assert_eq!(
        versions[0].index_metadata["yanked"],
        serde_json::json!(true),
        "index_metadata.yanked must be true"
    );
}

#[tokio::test]
async fn unyank_clears_yanked_flag_and_updates_metadata() {
    let Some(url) = db_url() else { return };
    let t = make_registry(&url).await;
    t.reg
        .publish(pkg(&t.registry, "unyank-me", "1.0.0"))
        .await
        .unwrap();
    t.reg
        .commit_publish(&t.registry, "unyank-me", "1.0.0")
        .await
        .unwrap();
    t.reg.yank(&t.registry, "unyank-me", "1.0.0").await.unwrap();
    t.reg
        .unyank(&t.registry, "unyank-me", "1.0.0")
        .await
        .unwrap();
    let versions = t.reg.get_versions(&t.registry, "unyank-me").await.unwrap();
    assert!(!versions[0].yanked, "yanked DB column must be FALSE");
    assert_eq!(
        versions[0].index_metadata["yanked"],
        serde_json::json!(false),
        "index_metadata.yanked must be false"
    );
}

#[tokio::test]
async fn yank_nonexistent_version_is_noop() {
    let Some(url) = db_url() else { return };
    let t = make_registry(&url).await;
    // Should not return an error even though no row matches
    t.reg.yank(&t.registry, "ghost", "9.9.9").await.unwrap();
}

// ── exists ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn exists_returns_false_for_unknown_package() {
    let Some(url) = db_url() else { return };
    let t = make_registry(&url).await;
    assert!(!t.reg.exists(&t.registry, "ghost").await.unwrap());
}

#[tokio::test]
async fn exists_returns_true_after_publish() {
    let Some(url) = db_url() else { return };
    let t = make_registry(&url).await;
    t.reg
        .publish(pkg(&t.registry, "exists-pkg", "1.0.0"))
        .await
        .unwrap();
    t.reg
        .commit_publish(&t.registry, "exists-pkg", "1.0.0")
        .await
        .unwrap();
    assert!(t.reg.exists(&t.registry, "exists-pkg").await.unwrap());
}

#[tokio::test]
async fn exists_is_false_for_different_registry() {
    let Some(url) = db_url() else { return };
    let t = make_registry(&url).await;
    t.reg
        .publish(pkg(&t.registry, "shared", "1.0.0"))
        .await
        .unwrap();
    t.reg
        .commit_publish(&t.registry, "shared", "1.0.0")
        .await
        .unwrap();
    assert!(!t
        .reg
        .exists("completely-different-registry", "shared")
        .await
        .unwrap());
}
