//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use actix_web::test::{call_service, read_body_json, TestRequest};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;

use batlehub_adapters::auth::StaticTokenAuthProvider;
use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta,
};
use batlehub_core::{
    entities::{AccessEvent, PackageId, PackageStatus, Role},
    error::CoreError,
    ports::{
        AuthProvider, CacheStore, PackageRepository, RegistryClient, StorageBackend, TokenOwner,
        UserToken, UserTokenRepository,
    },
    services::{new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService, RegistryPolicy},
};
use batlehub_web::RegistryModeMap;
use uuid::Uuid;

// ── InMemoryTokenRepository ───────────────────────────────────────────────────

struct InMemoryTokenRepository {
    /// `(token_hash, token)`. The hash is kept beside the row rather than on
    /// `UserToken` — as in Postgres, where it is a column no `SELECT` in the
    /// repository reads back — so `find_by_hash` works and a token this suite
    /// mints can actually be presented as a credential.
    tokens: Mutex<Vec<(String, UserToken)>>,
}

impl InMemoryTokenRepository {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            tokens: Mutex::new(vec![]),
        })
    }
}

#[async_trait]
impl UserTokenRepository for InMemoryTokenRepository {
    async fn create_token(
        &self,
        id: Uuid,
        owner: &TokenOwner,
        name: &str,
        token_hash: &str,
        role: Role,
        expires_at: chrono::DateTime<Utc>,
        groups: &[String],
    ) -> Result<UserToken, CoreError> {
        // Names are unique per *principal*, and a principal is (provider,
        // user_id) — matching the `uq_user_token_name` index in Postgres.
        let mut tokens = self.tokens.lock().unwrap();
        if tokens
            .iter()
            .any(|(_, t)| owns(t, owner) && t.name == name && t.revoked_at.is_none())
        {
            return Err(CoreError::Conflict(format!(
                "a token named '{}' already exists",
                name
            )));
        }
        let tok = UserToken {
            id,
            user_id: owner.user_id.clone(),
            provider: owner.provider.clone(),
            name: name.to_owned(),
            role,
            expires_at,
            created_at: Utc::now(),
            revoked_at: None,
            last_used_at: None,
            groups: groups.to_vec(),
            last_used_ip: None,
        };
        tokens.push((token_hash.to_owned(), tok));
        Ok(tokens.last().unwrap().1.clone_token())
    }

    async fn find_by_hash(&self, token_hash: &str) -> Result<Option<UserToken>, CoreError> {
        let tokens = self.tokens.lock().unwrap();
        Ok(tokens
            .iter()
            .find(|(h, t)| h == token_hash && t.revoked_at.is_none() && t.expires_at > Utc::now())
            .map(|(_, t)| t.clone_token()))
    }

    async fn list_for_user(&self, owner: &TokenOwner) -> Result<Vec<UserToken>, CoreError> {
        let tokens = self.tokens.lock().unwrap();
        Ok(tokens
            .iter()
            .filter(|(_, t)| owns(t, owner) && t.revoked_at.is_none())
            .map(|(_, t)| t.clone_token())
            .collect())
    }

    async fn touch_last_used(&self, _id: Uuid, _source_ip: Option<&str>) -> Result<(), CoreError> {
        Ok(())
    }

    async fn revoke(&self, id: Uuid, owner: &TokenOwner) -> Result<bool, CoreError> {
        let mut tokens = self.tokens.lock().unwrap();
        for (_, t) in tokens.iter_mut() {
            if t.id == id && owns(t, owner) && t.revoked_at.is_none() {
                t.revoked_at = Some(Utc::now());
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Ownership is both halves. Matching on `user_id` alone is exactly the bug the
/// `provider` column exists to close, so the test double must not take the
/// shortcut either.
fn owns(token: &UserToken, owner: &TokenOwner) -> bool {
    token.user_id == owner.user_id && token.provider == owner.provider
}

// UserToken doesn't derive Clone; add a helper method instead.
trait CloneToken {
    fn clone_token(&self) -> UserToken;
}

impl CloneToken for UserToken {
    fn clone_token(&self) -> UserToken {
        UserToken {
            id: self.id,
            user_id: self.user_id.clone(),
            provider: self.provider.clone(),
            name: self.name.clone(),
            role: self.role.clone(),
            expires_at: self.expires_at,
            created_at: self.created_at,
            revoked_at: self.revoked_at,
            last_used_at: self.last_used_at,
            groups: self.groups.clone(),
            last_used_ip: None,
        }
    }
}

// ── OIDC-style test auth provider ─────────────────────────────────────────────
// The token endpoint only accepts identities coming from a *configured* OIDC
// provider. StaticTokenAuthProvider sets "static-token", so we use a thin
// wrapper — parameterised by name, because the provider name is operator-chosen
// (`name = "authentik"`) and the endpoint must not hardcode any one value.

use batlehub_core::ports::RawAuthRequest;

const OIDC_USER_TOKEN: &str = "oidc-user-token";
const OIDC_ADMIN_TOKEN: &str = "oidc-admin-token";

/// The name the OIDC-style provider reports for most of this suite. Deliberately
/// *not* `"oidc"`: a renamed provider is the common deployment, and the endpoint
/// used to 403 every one of them.
const OIDC_PROVIDER_NAME: &str = "authentik";

struct OidcStyleAuthProvider(&'static str);

#[async_trait]
impl AuthProvider for OidcStyleAuthProvider {
    fn name(&self) -> &str {
        self.0
    }

    async fn authenticate(
        &self,
        req: &RawAuthRequest,
    ) -> Result<Option<batlehub_core::entities::Identity>, CoreError> {
        use batlehub_core::entities::Identity;
        let auth = req
            .headers
            .get("authorization")
            .or_else(|| req.headers.get("Authorization"))
            .and_then(|v| v.strip_prefix("Bearer "));
        match auth {
            // The groups are the point of the PAT-snapshot tests below: a
            // creator with none can only ever mint a token with none, which is
            // the state this whole feature exists to move off.
            Some(OIDC_USER_TOKEN) => Ok(Some(Identity {
                user_id: Some("oidc-user".to_owned()),
                role: Role::User,
                auth_provider: Some(self.0.to_owned()),
                groups: vec![
                    "authentik:eng".to_owned(),
                    "authentik:qa".to_owned(),
                    "platform team".to_owned(),
                ],
            })),
            Some(OIDC_ADMIN_TOKEN) => Ok(Some(Identity {
                user_id: Some("oidc-admin".to_owned()),
                role: Role::Admin,
                auth_provider: Some(self.0.to_owned()),
                groups: vec!["authentik:admins".to_owned()],
            })),
            _ => Ok(None),
        }
    }
}

/// Build an app wired with both static + OIDC-style providers and an in-memory token repo.
async fn make_app_with_tokens(
    repo: Arc<InMemoryRepo>,
    token_repo: Arc<InMemoryTokenRepository>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    make_app_with_oidc_provider(repo, token_repo, OIDC_PROVIDER_NAME, &[OIDC_PROVIDER_NAME]).await
}

/// As [`make_app_with_tokens`], but the caller chooses what the OIDC-style
/// provider calls itself and which names the server has configured. The two
/// differ in the negative tests: an identity claiming a provider the server does
/// not know must not be able to mint a token.
async fn make_app_with_oidc_provider(
    repo: Arc<InMemoryRepo>,
    token_repo: Arc<InMemoryTokenRepository>,
    provider_name: &'static str,
    configured_names: &[&str],
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let repo_dyn: Arc<dyn PackageRepository> = repo.clone();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

    let registries: HashMap<String, Arc<dyn RegistryClient>> = [(
        "npm".to_owned(),
        FixedRegistry::new("npm") as Arc<dyn RegistryClient>,
    )]
    .into();
    let policies: HashMap<String, Arc<RegistryPolicy>> =
        [("npm".to_owned(), Arc::new(rbac_policy(repo_dyn.clone()).0))].into();

    let hot = new_hot_lock(HotConfig {
        // RFC 0015 §4.2's instance tier, wired exactly as production wires it:
        // `instance_node` is §10 rule 5's own translation, so the fixture's admin
        // holds the control verbs and nobody else does. Without it every
        // `require_verb` on a control endpoint refuses, including the admin the
        // suite is asserting about — a fixture that does not build the model
        // tests a server nobody runs (§13.5).
        instance: Some(std::sync::Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        registries,
        policies,
        ..Default::default()
    });
    let local_svc = make_local_svc(hot.clone(), storage.clone());
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage,
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn));
    let tok_repo: Arc<dyn UserTokenRepository> = token_repo;
    let access_config = access_config_for(&["npm"]);
    let registry_map = registry_map_for(&[("npm", "npm")]);
    let cargo_indexes = batlehub_web::CargoIndexMap::default();

    let providers: Vec<Arc<dyn AuthProvider>> = vec![
        Arc::new(StaticTokenAuthProvider::new([
            (
                ADMIN_TOKEN.to_owned(),
                Some("admin".to_owned()),
                Role::Admin,
            ),
            (USER_TOKEN.to_owned(), Some("user-1".to_owned()), Role::User),
        ])),
        Arc::new(OidcStyleAuthProvider(provider_name)),
        // Last in the chain: the providers above answer for their own
        // credentials, and a minted PAT matches none of them. Wired to the same
        // repository the endpoint writes to, so `create → present → identity`
        // is one path here rather than three separately-mocked halves.
        Arc::new(batlehub_adapters::auth::UserTokenAuthProvider::new(
            tok_repo.clone(),
        )),
    ];

    finish_test_app(
        proxy_svc,
        admin_svc,
        tok_repo,
        access_config,
        registry_map,
        local_svc,
        RegistryModeMap::default(),
        cargo_indexes,
        ConfigureAppDefaults {
            oidc_provider_names: batlehub_web::OidcProviderNames::new(
                configured_names.iter().copied(),
            ),
            ..Default::default()
        },
        providers,
    )
    .await
}

// ── Token API tests ───────────────────────────────────────────────────────────

#[actix_web::test]
async fn create_token_returns_403_for_anonymous() {
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .set_json(serde_json::json!({"name": "ci", "expires_in_days": 7, "role": "user"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn create_token_returns_403_for_static_token_user() {
    // Static token provider sets auth_provider = "static-token", not "oidc"
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({"name": "ci", "expires_in_days": 7, "role": "user"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn create_token_succeeds_for_oidc_user() {
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "ci-token", "expires_in_days": 30, "role": "user"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 201);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["name"], "ci-token");
    assert!(body["token"].is_string(), "raw token should be returned");
}

// ── Provider-name matching (regression) ───────────────────────────────────────
// `create_token` used to compare `auth_provider` against the literal `"oidc"`.
// The name is operator-chosen and defaults to `"oidc"` only when unset, so every
// deployment that wrote `name = "authentik"` — or configured two providers — got
// a blanket 403 with no explanation, and fell back to the non-expiring static
// tokens in config.toml. The whole suite above now runs on a provider named
// "authentik"; these three pin the edges.

#[actix_web::test]
async fn create_token_succeeds_for_a_second_configured_oidc_provider() {
    // Two providers configured, the caller arrives through the second one.
    let app = make_app_with_oidc_provider(
        InMemoryRepo::new(),
        InMemoryTokenRepository::new(),
        "keycloak",
        &["authentik", "keycloak"],
    )
    .await;
    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "ci-token", "expires_in_days": 30, "role": "user"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        201,
        "a caller from any configured OIDC provider may mint a token"
    );
}

#[actix_web::test]
async fn create_token_rejects_provider_name_the_server_did_not_configure() {
    // The fix is an allow-list, not a rename: an identity reporting a provider
    // the server has no `[[auth]] type = "oidc"` entry for is still refused —
    // including one that calls itself the historical "oidc".
    let app = make_app_with_oidc_provider(
        InMemoryRepo::new(),
        InMemoryTokenRepository::new(),
        "oidc",
        &["authentik"],
    )
    .await;
    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "ci-token", "expires_in_days": 30, "role": "user"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn create_token_rejects_everyone_when_no_oidc_provider_is_configured() {
    // Absent configuration denies rather than admits.
    let app = make_app_with_oidc_provider(
        InMemoryRepo::new(),
        InMemoryTokenRepository::new(),
        "authentik",
        &[],
    )
    .await;
    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "ci-token", "expires_in_days": 30, "role": "user"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

// ── Ownership is (provider, user_id) ──────────────────────────────────────────
// `user_id` is a bare string each provider picks for itself, so it is not an
// identity on its own. Listing and revoking used to match on it alone, and
// accepted any authenticated caller — so a static `[[auth.tokens]]` entry with
// `user_id = "oidc-user"`, or a service account whose username happened to equal
// an OIDC `sub`, could enumerate and destroy that person's tokens.

#[actix_web::test]
async fn tokens_are_invisible_to_the_same_user_id_from_another_provider() {
    let token_repo = InMemoryTokenRepository::new();
    let app = make_app_with_oidc_provider(
        InMemoryRepo::new(),
        token_repo.clone(),
        "authentik",
        // Both providers are legitimate OIDC providers here — the point is that
        // the *same* user_id under a different one is a different principal.
        &["authentik", "keycloak"],
    )
    .await;

    let create = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "mine", "expires_in_days": 7, "role": "user"}))
        .to_request();
    assert_eq!(call_service(&app, create).await.status(), 201);

    // Same user_id ("oidc-user"), different provider.
    let other = make_app_with_oidc_provider(
        InMemoryRepo::new(),
        token_repo.clone(),
        "keycloak",
        &["authentik", "keycloak"],
    )
    .await;
    let list = TestRequest::get()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&other, list).await).await;
    assert_eq!(
        body.as_array().unwrap().len(),
        0,
        "the same user_id from another provider is a different principal"
    );
}

#[actix_web::test]
async fn a_token_cannot_be_revoked_from_another_provider() {
    let token_repo = InMemoryTokenRepository::new();
    let app = make_app_with_oidc_provider(
        InMemoryRepo::new(),
        token_repo.clone(),
        "authentik",
        &["authentik", "keycloak"],
    )
    .await;

    let create = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "mine", "expires_in_days": 7, "role": "user"}))
        .to_request();
    let created: Value = read_body_json(call_service(&app, create).await).await;
    let id = created["id"].as_str().unwrap();

    let other = make_app_with_oidc_provider(
        InMemoryRepo::new(),
        token_repo.clone(),
        "keycloak",
        &["authentik", "keycloak"],
    )
    .await;
    let revoke = TestRequest::delete()
        .uri(&format!("/api/v1/auth/tokens/{id}"))
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&other, revoke).await.status(), 404);

    // And the owner can still revoke it, so the scoping did not simply break.
    let mine = TestRequest::delete()
        .uri(&format!("/api/v1/auth/tokens/{id}"))
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, mine).await.status(), 204);
}

#[actix_web::test]
async fn a_static_token_session_cannot_list_or_revoke() {
    // Managing tokens takes an interactive login. A leaked machine credential
    // must not be able to enumerate its victim's other tokens, nor revoke them.
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;
    for req in [
        TestRequest::get()
            .uri("/api/v1/auth/tokens")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
        TestRequest::delete()
            .uri(&format!("/api/v1/auth/tokens/{}", Uuid::new_v4()))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    ] {
        assert_eq!(call_service(&app, req).await.status(), 403);
    }
}

#[actix_web::test]
async fn the_same_name_is_free_for_a_different_provider() {
    // Uniqueness is per principal, matching the `uq_user_token_name` index.
    let token_repo = InMemoryTokenRepository::new();
    let body = serde_json::json!({"name": "ci", "expires_in_days": 7, "role": "user"});

    for provider in ["authentik", "keycloak"] {
        let app = make_app_with_oidc_provider(
            InMemoryRepo::new(),
            token_repo.clone(),
            // `make_app_with_oidc_provider` takes &'static str, so match on it.
            if provider == "authentik" {
                "authentik"
            } else {
                "keycloak"
            },
            &["authentik", "keycloak"],
        )
        .await;
        let req = TestRequest::post()
            .uri("/api/v1/auth/tokens")
            .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
            .set_json(&body)
            .to_request();
        assert_eq!(
            call_service(&app, req).await.status(),
            201,
            "'ci' must be free under {provider}"
        );
    }
}

#[actix_web::test]
async fn create_token_rejects_zero_days() {
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "bad", "expires_in_days": 0, "role": "user"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn create_token_rejects_91_days() {
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "bad", "expires_in_days": 91, "role": "user"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn create_token_rejects_empty_name() {
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "   ", "expires_in_days": 7, "role": "user"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn create_token_rejects_invalid_role() {
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "t", "expires_in_days": 7, "role": "superadmin"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn create_token_user_cannot_escalate_to_admin_role() {
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "escalate", "expires_in_days": 7, "role": "admin"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn list_tokens_returns_created_tokens() {
    let tok_repo = InMemoryTokenRepository::new();
    let app = make_app_with_tokens(InMemoryRepo::new(), tok_repo).await;

    // Create a token
    let create_req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "my-token", "expires_in_days": 7, "role": "user"}))
        .to_request();
    let create_resp = call_service(&app, create_req).await;
    assert_eq!(create_resp.status(), 201);

    // List tokens
    let list_req = TestRequest::get()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .to_request();
    let list_resp = call_service(&app, list_req).await;
    assert_eq!(list_resp.status(), 200);
    let body: Value = read_body_json(list_resp).await;
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "my-token");
}

#[actix_web::test]
async fn revoke_token_returns_204() {
    let tok_repo = InMemoryTokenRepository::new();
    let app = make_app_with_tokens(InMemoryRepo::new(), tok_repo).await;

    let create_req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "to-revoke", "expires_in_days": 7, "role": "user"}))
        .to_request();
    let create_resp = call_service(&app, create_req).await;
    assert_eq!(create_resp.status(), 201);
    let created: Value = read_body_json(create_resp).await;
    let id = created["id"].as_str().unwrap();

    let revoke_req = TestRequest::delete()
        .uri(&format!("/api/v1/auth/tokens/{id}"))
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .to_request();
    let revoke_resp = call_service(&app, revoke_req).await;
    assert_eq!(revoke_resp.status(), 204);
}

#[actix_web::test]
async fn revoke_nonexistent_token_returns_404() {
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;
    let fake_id = Uuid::new_v4();
    let req = TestRequest::delete()
        .uri(&format!("/api/v1/auth/tokens/{fake_id}"))
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn duplicate_token_name_returns_conflict() {
    let tok_repo = InMemoryTokenRepository::new();
    let app = make_app_with_tokens(InMemoryRepo::new(), tok_repo).await;

    for _ in 0..2 {
        let req = TestRequest::post()
            .uri("/api/v1/auth/tokens")
            .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
            .set_json(serde_json::json!({"name": "dup", "expires_in_days": 7, "role": "user"}))
            .to_request();
        let _ = call_service(&app, req).await;
    }

    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "dup", "expires_in_days": 7, "role": "user"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 409);
}

// ── PAT group snapshot (RFC 0011-bis §4.4, G1) ────────────────────────────────
// Before this, `UserTokenAuthProvider::to_identity` returned `groups: vec![]`
// for every token, so a `group:` subject in RFC 0015's grant hierarchy could
// never match a PAT and every automation read as an authenticated user
// belonging to no team — including the team that created it.

/// The whole chain in one test: an OIDC session mints a token naming groups it
/// holds, and the token, presented as a credential, resolves to them. The three
/// halves — endpoint, column, provider — are only correct together.
#[actix_web::test]
async fn a_pat_carries_the_groups_it_was_minted_with() {
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;

    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({
            "name": "ci", "expires_in_days": 30, "role": "user",
            "groups": ["authentik:eng"],
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 201);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["groups"], serde_json::json!(["authentik:eng"]));
    let raw = body["token"].as_str().expect("raw token").to_owned();

    let req = TestRequest::get()
        .uri("/api/v1/me")
        .insert_header(("Authorization", bearer(&raw)))
        .to_request();
    let me: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(me["user_id"], "oidc-user");
    assert_eq!(
        me["groups"],
        serde_json::json!(["authentik:eng"]),
        "the snapshot is what the token resolves to"
    );
    assert_eq!(
        me["auth_provider"], "user-token",
        "still a PAT session, so it cannot mint or revoke tokens of its own"
    );
}

/// The subset invariant RFC 0015 §4.3 states and `pat_is_within_owner` checks.
/// A token that can exceed its owner is a privilege-escalation primitive.
#[actix_web::test]
async fn a_pat_cannot_be_minted_with_a_group_its_creator_lacks() {
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;

    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({
            "name": "escalate", "expires_in_days": 7, "role": "user",
            "groups": ["authentik:eng", "authentik:admins"],
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        403,
        "refused, not silently narrowed — a quietly weaker token is a pipeline \
         that cannot see a package with nothing to explain why"
    );

    let req = TestRequest::get()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .to_request();
    let list: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(
        list.as_array().map(Vec::len),
        Some(0),
        "a refused request mints nothing"
    );
}

/// Omitting the field is what every token minted before this feature carried,
/// and what it must keep carrying (§10: existing PATs lose no access).
#[actix_web::test]
async fn a_pat_with_no_groups_requested_carries_none() {
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;

    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "quiet", "expires_in_days": 7, "role": "user"}))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["groups"], serde_json::json!([]));
    let raw = body["token"].as_str().expect("raw token").to_owned();

    let req = TestRequest::get()
        .uri("/api/v1/me")
        .insert_header(("Authorization", bearer(&raw)))
        .to_request();
    let me: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(me["groups"], serde_json::json!([]));
}

/// Space-stripped on both sides (§4.4), and stored as the *owner* holds it — so
/// every later comparison sees the same bytes the OIDC session would have
/// carried, including the SQL listing predicate.
#[actix_web::test]
async fn a_requested_group_matches_space_stripped_and_is_stored_as_held() {
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;

    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({
            "name": "spaced", "expires_in_days": 7, "role": "user",
            "groups": ["platformteam"],
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 201);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["groups"], serde_json::json!(["platform team"]));
}

/// The listing shows the snapshot, because a snapshot goes stale silently:
/// nothing tells an owner their token still carries a team they left.
#[actix_web::test]
async fn the_token_listing_shows_the_snapshot() {
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;

    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({
            "name": "listed", "expires_in_days": 7, "role": "user",
            "groups": ["authentik:qa"],
        }))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 201);

    let req = TestRequest::get()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .to_request();
    let list: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(list[0]["groups"], serde_json::json!(["authentik:qa"]));
}

/// A PAT is a credential, not a session: it cannot mint another one, so it
/// cannot mint one with groups either. The check that stops it is the same
/// `oidc_session_owner` gate, asserted here against the group path specifically
/// — a PAT that could re-mint itself would launder a snapshot past its TTL.
#[actix_web::test]
async fn a_pat_cannot_mint_a_second_pat_from_its_own_snapshot() {
    let app = make_app_with_tokens(InMemoryRepo::new(), InMemoryTokenRepository::new()).await;

    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({
            "name": "first", "expires_in_days": 7, "role": "user",
            "groups": ["authentik:eng"],
        }))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    let raw = body["token"].as_str().expect("raw token").to_owned();

    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(&raw)))
        .set_json(serde_json::json!({
            "name": "second", "expires_in_days": 90, "role": "user",
            "groups": ["authentik:eng"],
        }))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

// ── Pagination / Filtering tests ──────────────────────────────────────────────

#[actix_web::test]
async fn admin_packages_list_blocked_only_filter() {
    let repo = InMemoryRepo::new();

    let available = PackageId::new("npm", "lodash", "4.17.21");
    let blocked = PackageId::new("npm", "evil-pkg", "1.0.0");

    repo.record_access(AccessEvent::allowed_download(
        available,
        Some("u".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();
    repo.set_status(
        &blocked,
        PackageStatus::Blocked {
            reason: "vuln".to_owned(),
            blocked_by: "admin".to_owned(),
            blocked_at: Utc::now(),
        },
    )
    .await
    .unwrap();

    let app = make_app(repo).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/packages?blocked_only=true")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let items = body["items"].as_array().unwrap();
    assert!(
        items.iter().all(|i| i["status"]["status"] == "blocked"),
        "only blocked packages expected"
    );
}

#[actix_web::test]
async fn audit_log_denied_only_filter() {
    let repo = InMemoryRepo::new();
    let app = make_app(repo.clone()).await;

    // Cause a denied event (anonymous accessing tarball = source:read denied)
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash/4.17.21/tarball")
        .to_request();
    let _ = call_service(&app, req).await;

    // Also cause an allowed event
    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    let _ = call_service(&app, req).await;

    let audit_req = TestRequest::get()
        .uri("/api/v1/admin/audit-log?denied_only=true")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, audit_req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let events = body["items"].as_array().unwrap();
    assert!(!events.is_empty(), "at least one denied event expected");
    assert!(
        events.iter().all(|e| e["result"]["outcome"] == "denied"),
        "only denied events expected"
    );
}

#[actix_web::test]
async fn registries_endpoint_returns_list_for_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get().uri("/api/v1/registries").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let registries = body.as_array().unwrap();
    // Anonymous has access to github, npm, cargo in make_app
    assert!(!registries.is_empty(), "should see at least one registry");
    let names: Vec<&str> = registries
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(names.contains(&"npm"));
}

#[actix_web::test]
async fn registries_endpoint_returns_200_for_admin() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/registries")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let registries = body.as_array().unwrap();
    assert!(registries.len() >= 3, "admin should see github, npm, cargo");
}

// ── RFC 0036 §6.1: authentication events in the audit trail ─────────────────

/// Every audit row carrying `action`, newest first.
async fn rows_for(
    repo: &InMemoryRepo,
    action: batlehub_core::entities::AccessAction,
) -> Vec<AccessEvent> {
    let filter = batlehub_core::entities::EventFilter {
        actions: vec![action],
        ..batlehub_core::entities::EventFilter::new()
    };
    repo.list_events(filter).await.unwrap()
}

#[actix_web::test]
async fn minting_and_revoking_a_token_are_audit_events_without_the_secret() {
    use batlehub_core::entities::AccessAction;
    let repo = InMemoryRepo::new();
    let app = make_app_with_tokens(Arc::clone(&repo), InMemoryTokenRepository::new()).await;

    let req = TestRequest::post()
        .uri("/api/v1/auth/tokens")
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .set_json(serde_json::json!({"name": "ci-token", "expires_in_days": 30, "role": "user"}))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    let (id, raw) = (
        body["id"].as_str().unwrap(),
        body["token"].as_str().unwrap(),
    );

    let created = rows_for(&repo, AccessAction::TokenCreate).await;
    assert_eq!(created.len(), 1);
    let detail = created[0].detail.as_deref().unwrap();
    assert!(
        detail.contains(id) && detail.contains("ci-token"),
        "{detail}"
    );
    assert!(
        !detail.contains(raw),
        "the token value must never reach the trail"
    );
    assert_eq!(created[0].user_id.as_deref(), Some("oidc-user"));

    let req = TestRequest::delete()
        .uri(&format!("/api/v1/auth/tokens/{id}"))
        .insert_header(("Authorization", bearer(OIDC_USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);
    let revoked = rows_for(&repo, AccessAction::TokenRevoke).await;
    assert_eq!(revoked.len(), 1);
    assert!(revoked[0].detail.as_deref().unwrap().contains(id));
}

/// A refused credential falls back to anonymous, so the middleware is the only
/// place the refusal is visible — and a burst of them is one row, not twenty.
#[actix_web::test]
async fn a_rejected_credential_is_one_throttled_row_per_source() {
    use batlehub_core::entities::AccessAction;
    let repo = InMemoryRepo::new();
    let app = make_app_with_tokens(Arc::clone(&repo), InMemoryTokenRepository::new()).await;

    for _ in 0..21 {
        let req = TestRequest::get()
            .uri("/api/v1/registries")
            .peer_addr("203.0.113.9:4000".parse().unwrap())
            .insert_header(("Authorization", bearer("bh_pat_not-a-real-token")))
            .to_request();
        // The request itself goes on as anonymous.
        assert_eq!(call_service(&app, req).await.status(), 200);
    }
    let rejected = rows_for(&repo, AccessAction::CredentialRejected).await;
    assert_eq!(
        rejected.len(),
        1,
        "the throttle holds a burst to one row a minute"
    );
    assert_eq!(rejected[0].ip_address.as_deref(), Some("203.0.113.9"));
    assert!(rejected[0].result.is_denied());

    // An accepted credential and no credential at all are not rejections.
    for auth in [Some(bearer(ADMIN_TOKEN)), None] {
        let mut req = TestRequest::get()
            .uri("/api/v1/registries")
            .peer_addr("198.51.100.7:4000".parse().unwrap());
        if let Some(auth) = auth {
            req = req.insert_header(("Authorization", auth));
        }
        call_service(&app, req.to_request()).await;
    }
    assert_eq!(
        rows_for(&repo, AccessAction::CredentialRejected)
            .await
            .len(),
        1
    );
}
