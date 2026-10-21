//! The whole-registry document cache must not replay one caller's document to
//! another (RFC 0015 §4.4 rule 3).
//!
//! The key used to be `resolve(&[grants.registry], subject)` — the registry node
//! alone — while the document is filtered by four further identity-dependent
//! things: the instance and namespace tiers, `releases:list`, per-package
//! visibility (`team` group membership and `private`'s package-written grants),
//! and beta-channel membership. Two callers who agreed on that one node and on
//! nothing else shared an entry, and the first one in decided what the second
//! was served.
//!
//! `filter.rs`'s `document_key_tests` assert the key distinguishes each of those
//! dimensions. This file asserts the *server* does, through the routes a
//! `bundle install` actually calls, with the cache switched on — because a key
//! that is correct in isolation and a call site that passes it the wrong
//! resolution is precisely the shape of the bug (`cached_document`'s own
//! contract sentence named the namespace tier while the call site did not).
//!
//! Beta-channel membership is the dimension exercised here because it is the one
//! that no grant resolves at all: both callers hold `Action::ALL` at the registry
//! tier, so their grant sets are *identical* and the old key is *byte-identical*
//! for the two of them. Nothing about the disclosure is peculiar to beta
//! channels; it is the cleanest way to hold every other variable still.
//!
//! See `tests/common/mod.rs` for the shared app-factory infrastructure.

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, TestRequest};

use batlehub_adapters::auth::StaticTokenAuthProvider;
use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryBetaChannelStore, InMemoryPackageRepository as InMemoryRepo,
    InMemoryStorageBackend as InMemoryStorage, NoopArtifactMetaRepository as NoopArtifactMeta,
    NullUserTokenRepository as NullTokenRepository,
};
use batlehub_adapters::local_registry::InMemoryLocalRegistry;
use batlehub_config::schema::RegistryMode;
use batlehub_core::entities::{Identity, Role};
use batlehub_core::ports::{
    AuthProvider, BetaChannelEntry, BetaChannelPort, CacheStore, PackageRepository, StorageBackend,
    UserTokenRepository,
};
use batlehub_core::services::document_cache::DocumentCache;
use batlehub_core::services::local_registry::PublishRequest;
use batlehub_core::services::{
    new_hot_lock, AdminService, HotConfig, LocalRegistryService, ProxyMetrics, ProxyService,
};
use batlehub_web::RegistryModeMap;

const REGISTRY: &str = "local-gems";

/// The gem whose pre-release only a beta-channel member may see.
const GEM: &str = "rails";
const STABLE: &str = "1.0.0";
const PRERELEASE: &str = "2.0.0.beta1";

/// A beta-channel member.
const BETA_TOKEN: &str = "beta-user-token";
/// …and a caller who differs from them in **nothing else**: same role, no
/// groups, and the same grants, since the registry grants `Action::ALL` to
/// everyone.
const PLAIN_TOKEN: &str = "plain-user-token";

/// Two `role:user` callers with no groups.
///
/// The group-less part is load-bearing. `team_ns_auth_providers` would have
/// worked as a fixture and proved nothing: its two users differ in group
/// membership as well as in beta membership, so the key would tell them apart on
/// the group axis and the assertion below would pass with the beta axis missing
/// entirely. These two are identical to the resolver.
fn beta_and_plain_users() -> Vec<Arc<dyn AuthProvider>> {
    vec![Arc::new(StaticTokenAuthProvider::new([
        (
            ADMIN_TOKEN.to_owned(),
            Some("admin".to_owned()),
            Role::Admin,
        ),
        (
            BETA_TOKEN.to_owned(),
            Some("beta-user".to_owned()),
            Role::User,
        ),
        (
            PLAIN_TOKEN.to_owned(),
            Some("plain-user".to_owned()),
            Role::User,
        ),
    ]))]
}

fn publisher() -> Identity {
    Identity {
        user_id: Some("admin".to_owned()),
        role: Role::Admin,
        auth_provider: None,
        groups: vec![],
    }
}

/// A local RubyGems registry with the document cache switched on, a beta channel
/// configured, and `Action::ALL` granted to **everyone** at the registry tier.
///
/// The permissive hierarchy is the point rather than a shortcut: it makes both
/// callers resolve to exactly the same grant set, so the only thing that can
/// distinguish their documents — and therefore the only thing that can keep them
/// in separate cache entries — is something the grant set does not describe.
async fn beta_channel_app() -> (
    Arc<LocalRegistryService>,
    Arc<InMemoryBetaChannelStore>,
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
) {
    let backend = Arc::new(InMemoryLocalRegistry::new());
    let repo_dyn: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

    let beta = InMemoryBetaChannelStore::new();
    let beta_map: HashMap<String, Arc<dyn BetaChannelPort>> = [(
        REGISTRY.to_owned(),
        Arc::clone(&beta) as Arc<dyn BetaChannelPort>,
    )]
    .into();

    let grants = [(
        REGISTRY.to_owned(),
        Arc::new(permissive_grants(REGISTRY, "rubygems")),
    )]
    .into();

    let hot = new_hot_lock(HotConfig {
        instance: Some(Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        grants,
        beta_channel: beta_map,
        // Production installs this unconditionally (`server/src/hot_config.rs`),
        // and a fixture that leaves it `None` cannot see this bug at all: the
        // document is rebuilt per request and every caller gets a correct one.
        document_cache: Some(DocumentCache::new()),
        ..Default::default()
    });

    let local_svc = Arc::new(LocalRegistryService {
        backend: backend.clone(),
        storage: storage.clone(),
        hot: hot.clone(),
        quota: None,
        ownership: None,
        team_namespace: None,
        sbom: None,
        explore_cache: None,
        package_repo: None,
        readme: None,
    });

    let proxy_svc = Arc::new(ProxyService {
        hot,
        storage,
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
        explore_cache: None,
    });

    let mode_map = RegistryModeMap::default();
    mode_map.insert(REGISTRY.to_owned(), RegistryMode::Local);

    // `finish_test_app` rather than `build_local_registry_app`, because the two
    // callers this test needs — same role, same groups, same grants, different
    // beta membership — exist in no shared fixture, and the narrower builders
    // pin `test_auth_providers`.
    let app = finish_test_app(
        proxy_svc,
        Arc::new(AdminService::new(repo_dyn)),
        Arc::new(NullTokenRepository) as Arc<dyn UserTokenRepository>,
        access_config(&[], &[REGISTRY]),
        registry_map_for(&[(REGISTRY, "rubygems")]),
        Arc::clone(&local_svc),
        mode_map,
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults::default(),
        beta_and_plain_users(),
    )
    .await;

    (local_svc, beta, app)
}

async fn publish(local_svc: &LocalRegistryService, version: &str) {
    let artifact = bytes::Bytes::from_static(b"gem-bytes");
    let checksum = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&artifact));
    local_svc
        .publish(PublishRequest {
            registry: REGISTRY.to_owned(),
            name: GEM.to_owned(),
            version: version.to_owned(),
            artifact,
            checksum,
            index_metadata: serde_json::json!({ "name": GEM, "version": version }),
            unlisted: false,
            publisher: publisher(),
            signature_bytes: None,
            signature_type: None,
        })
        .await
        .expect("publish");
}

async fn versions_as<S: TestService>(app: &S, token: &str) -> String {
    let req = TestRequest::get()
        .uri(&format!("/proxy/{REGISTRY}/versions"))
        .insert_header(("Authorization", bearer(token)))
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 200, "the compact index should be served");
    String::from_utf8(actix_web::test::read_body(resp).await.to_vec())
        .expect("the compact index is UTF-8")
}

/// **The disclosure.** A beta member warms the cache; a non-member must not be
/// served their document.
///
/// Order matters and is the whole test: the member goes first, so the entry in
/// the cache is the *wide* one. Reversed, the bug hides — the non-member's
/// narrow document would be replayed to the member, which is a stale answer
/// rather than a leak and no assertion here would fail.
#[actix_web::test]
async fn a_beta_members_index_is_not_replayed_to_a_non_member() {
    let (local_svc, beta, app) = beta_channel_app().await;
    publish(&local_svc, STABLE).await;
    publish(&local_svc, PRERELEASE).await;
    beta.add_member(
        REGISTRY,
        BetaChannelEntry {
            principal_type: "user".to_owned(),
            principal_id: "beta-user".to_owned(),
            granted_by: None,
        },
    )
    .await
    .expect("add the member");

    let member = versions_as(&app, BETA_TOKEN).await;
    assert!(
        member.contains(PRERELEASE),
        "the positive control: a member does see the pre-release, so the \
         assertion below is about the cache rather than about the filter \
         refusing everyone.\n{member}"
    );

    let outsider = versions_as(&app, PLAIN_TOKEN).await;
    assert!(
        !outsider.contains(PRERELEASE),
        "the non-member was served the member's cached document. Both callers \
         hold `Action::ALL` at the registry tier, so they resolve to the same \
         grant set — which is exactly why the key cannot be the grant set \
         alone.\n{outsider}"
    );
    assert!(
        outsider.contains(STABLE),
        "…and the non-member still gets the stable release: the fix is a \
         separate entry, not an empty document.\n{outsider}"
    );
}

/// The other direction, so the fix cannot be "never cache".
///
/// Two requests from the same caller must hit the same entry — that is §11.7
/// arm 3's whole return, and a key that fragmented per request would pass the
/// test above while throwing the cache away.
#[actix_web::test]
async fn the_same_caller_reads_the_same_entry_twice() {
    let (local_svc, _beta, app) = beta_channel_app().await;
    publish(&local_svc, STABLE).await;

    let first = versions_as(&app, PLAIN_TOKEN).await;

    // Published *after* the first read, so it is only visible if the document
    // was rebuilt. The generation bump on publish is what makes a warm entry
    // stale, so a second gem must appear — and the assertion that matters is the
    // third read, which must be served from the entry the second one stored.
    publish(&local_svc, "3.0.0").await;
    let second = versions_as(&app, PLAIN_TOKEN).await;
    assert_ne!(
        first, second,
        "a publish must be visible on the next request, not the next expiry"
    );

    let third = versions_as(&app, PLAIN_TOKEN).await;
    assert_eq!(
        second, third,
        "one caller, one grant set, nothing written in between — this is the \
         hit the cache exists for"
    );
}
