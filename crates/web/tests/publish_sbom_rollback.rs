//! A required SBOM that fails rolls a local publish back whole.
//!
//! The rollback can only remove a *pending* row (RFC 0016: a published row is
//! tombstoned, never deleted), so the SBOM step has to run before the commit.
//! It ran after it once, and a failed SBOM then left a version listed with no
//! bytes behind it that refused every republish as "already published".
//!
//! Driven through the real `InMemoryLocalRegistry`, the only in-memory backend
//! that tells a pending row from a published one.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;

use batlehub_config::schema::RegistryMode;
use batlehub_core::entities::{Identity, Role};
use batlehub_core::error::CoreError;
use batlehub_core::services::hot_config::SbomConfig;
use batlehub_core::services::sbom::SbomService;
use batlehub_core::services::{LocalRegistryService, PublishRequest};

fn request() -> PublishRequest {
    let artifact = bytes::Bytes::from_static(b"no manifest in here");
    let checksum = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&artifact));
    PublishRequest {
        registry: "local-npm".to_owned(),
        name: "my-pkg".to_owned(),
        version: "1.0.0".to_owned(),
        artifact,
        checksum,
        index_metadata: serde_json::json!({}),
        unlisted: false,
        publisher: Identity {
            user_id: Some("user-1".to_owned()),
            role: Role::User,
            auth_provider: None,
            groups: vec![],
        },
        signature_bytes: None,
        signature_type: None,
    }
}

#[actix_web::test]
async fn a_failed_required_sbom_leaves_no_version_and_allows_a_republish() {
    let parts = local_registry_app_parts("local-npm", "npm", RegistryMode::Local, None);
    let cur = parts.local_svc.clone();
    // No extractor: a `required` SBOM finds no dependency manifest and fails.
    let svc = LocalRegistryService {
        backend: cur.backend.clone(),
        storage: cur.storage.clone(),
        hot: cur.hot.clone(),
        quota: cur.quota.clone(),
        ownership: cur.ownership.clone(),
        team_namespace: cur.team_namespace.clone(),
        sbom: Some(Arc::new(SbomService::new(
            Arc::new(batlehub_adapters::in_memory::NoopSbomRepository),
            None,
            None,
        ))),
        explore_cache: cur.explore_cache.clone(),
        package_repo: cur.package_repo.clone(),
        readme: cur.readme.clone(),
    };
    svc.hot.write().await.sbom.insert(
        "local-npm".to_owned(),
        SbomConfig {
            enabled: true,
            formats: vec!["spdx".to_owned()],
            required: true,
            fetch_upstream: false,
            registry_type: "npm".to_owned(),
        },
    );

    let err = svc.publish(request()).await.unwrap_err();
    assert!(matches!(err, CoreError::AccessDenied(_)), "got {err:?}");
    assert!(
        svc.backend
            .get_versions("local-npm", "my-pkg")
            .await
            .unwrap()
            .is_empty(),
        "a rolled-back publish must leave no version listed"
    );

    svc.hot.write().await.sbom.clear();
    svc.publish(request())
        .await
        .expect("the same version publishes once the SBOM is no longer required");
}
