//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::{Arc, Mutex};

use actix_web::test::{call_service, read_body_json, TestRequest};
use async_trait::async_trait;
use serde_json::Value;

use batlehub_core::entities::Identity;
use batlehub_core::error::CoreError;
use batlehub_core::ports::{BetaChannelEntry, BetaChannelPort};
use batlehub_web::RegistryModeMap;

// ── In-memory BetaChannelPort ─────────────────────────────────────────────────

/// (registry, type, id, granted_by)
type BetaChannelRow = (String, String, String, Option<String>);

#[derive(Default)]
struct InMemoryBetaChannelStore {
    entries: Mutex<Vec<BetaChannelRow>>,
}

impl InMemoryBetaChannelStore {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl BetaChannelPort for InMemoryBetaChannelStore {
    async fn is_member(&self, registry: &str, identity: &Identity) -> Result<bool, CoreError> {
        let Some(user_id) = identity.user_id.as_deref() else {
            return Ok(false);
        };
        let guard = self.entries.lock().unwrap();
        Ok(guard
            .iter()
            .any(|(r, t, id, _)| r == registry && t == "user" && id == user_id))
    }

    async fn add_member(&self, registry: &str, entry: BetaChannelEntry) -> Result<(), CoreError> {
        self.entries.lock().unwrap().push((
            registry.to_owned(),
            entry.principal_type,
            entry.principal_id,
            entry.granted_by,
        ));
        Ok(())
    }

    async fn remove_member(
        &self,
        registry: &str,
        principal_type: &str,
        principal_id: &str,
    ) -> Result<(), CoreError> {
        self.entries
            .lock()
            .unwrap()
            .retain(|(r, t, id, _)| !(r == registry && t == principal_type && id == principal_id));
        Ok(())
    }

    async fn list_members(&self, registry: &str) -> Result<Vec<BetaChannelEntry>, CoreError> {
        let guard = self.entries.lock().unwrap();
        Ok(guard
            .iter()
            .filter(|(r, _, _, _)| r == registry)
            .map(|(_, t, id, gb)| BetaChannelEntry {
                principal_type: t.clone(),
                principal_id: id.clone(),
                granted_by: gb.clone(),
            })
            .collect())
    }
}
async fn make_app_with_beta_store(
    beta_store: Arc<dyn BetaChannelPort>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    make_app_with_providers(beta_store, &[]).await
}

/// The same app, with `providers` as the identity providers that yield user ids.
async fn make_app_with_providers(
    beta_store: Arc<dyn BetaChannelPort>,
    providers: &[&str],
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
    local_svc.hot.write().await.user_providers =
        providers.iter().map(|p| (*p).to_owned()).collect();

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
        beta_store,
        test_auth_providers(),
    )
    .await
}

// ── /api/v1/admin/registries/{r}/beta-channel ────────────────────────────────

#[actix_web::test]
async fn beta_channel_list_empty_returns_200() {
    let store: Arc<dyn BetaChannelPort> = InMemoryBetaChannelStore::new();
    let app = make_app_with_beta_store(store).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/my-npm/beta-channel")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body, serde_json::json!([]));
}

#[actix_web::test]
async fn beta_channel_add_member_returns_204() {
    let store: Arc<dyn BetaChannelPort> = InMemoryBetaChannelStore::new();
    let app = make_app_with_beta_store(store).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-npm/beta-channel")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"principal_type": "user", "principal_id": "alice"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 204);
}

#[actix_web::test]
async fn beta_channel_list_shows_added_member() {
    let store: Arc<dyn BetaChannelPort> = InMemoryBetaChannelStore::new();
    let app = make_app_with_beta_store(Arc::clone(&store)).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-npm/beta-channel")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"principal_type": "user", "principal_id": "bob", "granted_by": "admin"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/my-npm/beta-channel")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let list = body.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["principal_type"], "user");
    assert_eq!(list[0]["principal_id"], "bob");
    assert_eq!(list[0]["granted_by"], "admin");
}

#[actix_web::test]
async fn beta_channel_remove_member_returns_204() {
    let store: Arc<dyn BetaChannelPort> = InMemoryBetaChannelStore::new();
    let app = make_app_with_beta_store(Arc::clone(&store)).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-npm/beta-channel")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"principal_type": "user", "principal_id": "charlie"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/my-npm/beta-channel/user/charlie")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/my-npm/beta-channel")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    let body: Value = read_body_json(resp).await;
    assert_eq!(body, serde_json::json!([]));
}

#[actix_web::test]
async fn beta_channel_add_invalid_principal_type_returns_400() {
    let store: Arc<dyn BetaChannelPort> = InMemoryBetaChannelStore::new();
    let app = make_app_with_beta_store(store).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-npm/beta-channel")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"principal_type": "org", "principal_id": "acme"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn beta_channel_requires_admin() {
    let store: Arc<dyn BetaChannelPort> = InMemoryBetaChannelStore::new();
    let app = make_app_with_beta_store(store).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-npm/beta-channel")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({"principal_type": "user", "principal_id": "eve"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

/// A bare user id is stored as `<provider>:<id>` when one provider yields user
/// ids, and refused when several do: a bare id would match every provider's.
#[actix_web::test]
async fn beta_channel_scopes_a_bare_user_to_its_provider() {
    let add = |id: &str| {
        TestRequest::post()
            .uri("/api/v1/admin/registries/my-npm/beta-channel")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(serde_json::json!({"principal_type": "user", "principal_id": id}))
            .to_request()
    };

    let store: Arc<dyn BetaChannelPort> = InMemoryBetaChannelStore::new();
    let app = make_app_with_providers(Arc::clone(&store), &["corp"]).await;
    assert_eq!(call_service(&app, add("alice")).await.status(), 204);
    let ids: Vec<String> = store
        .list_members("my-npm")
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.principal_id)
        .collect();
    assert_eq!(ids, vec!["corp:alice"]);

    let store: Arc<dyn BetaChannelPort> = InMemoryBetaChannelStore::new();
    let app = make_app_with_providers(store, &["corp", "signup"]).await;
    assert_eq!(call_service(&app, add("alice")).await.status(), 400);
    assert_eq!(call_service(&app, add("signup:alice")).await.status(), 204);
}
