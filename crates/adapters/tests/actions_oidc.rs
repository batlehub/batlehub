#![cfg(feature = "auth-actions-oidc")]

use batlehub_adapters::auth::actions_oidc::ActionsOidcAuthProvider;
use batlehub_core::ports::{ActionsOidcAuthConfig, AuthProvider, RawAuthRequest};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

/// Audience every test provider requires and every test token carries.
/// `actions-oidc` makes it mandatory: its issuer is shared by every repository
/// on the forge, so `aud` is what identifies tokens meant for this deployment.
const TEST_AUDIENCE: &str = "https://batlehub.test";

// ── Test key material ─────────────────────────────────────────────────────────

const TEST_EC_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgWTFfCGljY6aw3Hrt\n\
kHmPRiazukxPLb6ilpRAewjW8nihRANCAATDskChT+Altkm9X7MI69T3IUmrQU0L\n\
950IxEzvw/x5BMEINRMrXLBJhqzO9Bm+d6JbqA21YQmd1Kt4RzLJR1W+\n\
-----END PRIVATE KEY-----";

const TEST_JWKS_JSON: &str = r#"{
  "keys": [{
    "kty": "EC",
    "crv": "P-256",
    "use": "sig",
    "kid": "test-kid",
    "x": "w7JAoU_gJbZJvV-zCOvU9yFJq0FNC_edCMRM78P8eQQ",
    "y": "wQg1EytcsEmGrM70Gb53oluoDbVhCZ3Uq3hHMslHVb4"
  }]
}"#;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn future_exp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 3600
}

fn signed_token(kid: Option<&str>, claims: serde_json::Value) -> String {
    let header = Header {
        alg: Algorithm::ES256,
        kid: kid.map(str::to_owned),
        ..Default::default()
    };
    let key = EncodingKey::from_ec_pem(TEST_EC_PRIVATE_KEY.as_bytes()).unwrap();
    encode(&header, &claims, &key).unwrap()
}

fn bearer(token: &str) -> RawAuthRequest {
    RawAuthRequest {
        headers: [("authorization".to_owned(), format!("Bearer {token}"))].into(),
        query_params: Default::default(),
        source_ip: None,
    }
}

fn discovery_json(base_url: &str) -> String {
    json!({
        "issuer": base_url,
        "jwks_uri": format!("{base_url}/jwks"),
    })
    .to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Full end-to-end: bootstrap the provider via real HTTP mocks, sign a JWT,
/// authenticate it, and verify the returned Identity.
#[tokio::test]
async fn full_bootstrap_and_authenticate() {
    let mut server = mockito::Server::new_async().await;
    let base = server.url();

    let _disc = server
        .mock("GET", "/.well-known/openid-configuration")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(discovery_json(&base))
        .create_async()
        .await;
    let _jwks = server
        .mock("GET", "/jwks")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(TEST_JWKS_JSON)
        .create_async()
        .await;

    let cfg = ActionsOidcAuthConfig {
        name: "forgejo-action".to_owned(),
        issuer_url: base.clone(),
        required: false,
        audience: TEST_AUDIENCE.to_owned(),
        user_id_claim: "sub".to_owned(),
        rules: vec![],
    };

    let provider = ActionsOidcAuthProvider::new(&cfg)
        .await
        .expect("provider construction failed");

    assert_eq!(provider.name(), "forgejo-action");

    let token = signed_token(
        Some("test-kid"),
        json!({
            "sub": "ci-bot",
            "repository": "myorg/myrepo",
            "exp": future_exp(),
            "aud": TEST_AUDIENCE,
            "iss": base,
        }),
    );
    let id = provider
        .authenticate(&bearer(&token))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(id.user_id.as_deref(), Some("ci-bot"));
    assert_eq!(id.auth_provider.as_deref(), Some("forgejo-action"));
}

/// Provider construction must fail when the discovery endpoint returns an error.
#[tokio::test]
async fn bootstrap_fails_on_discovery_500() {
    let mut server = mockito::Server::new_async().await;

    let _mock = server
        .mock("GET", "/.well-known/openid-configuration")
        .with_status(500)
        .create_async()
        .await;

    let cfg = ActionsOidcAuthConfig {
        name: "test".to_owned(),
        issuer_url: server.url(),
        required: false,
        audience: TEST_AUDIENCE.to_owned(),
        user_id_claim: "sub".to_owned(),
        rules: vec![],
    };

    assert!(ActionsOidcAuthProvider::new(&cfg).await.is_err());
}

/// Provider construction must fail when the JWKS endpoint returns an error.
#[tokio::test]
async fn bootstrap_fails_on_jwks_500() {
    let mut server = mockito::Server::new_async().await;
    let base = server.url();

    let _disc = server
        .mock("GET", "/.well-known/openid-configuration")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(discovery_json(&base))
        .create_async()
        .await;
    let _jwks = server
        .mock("GET", "/jwks")
        .with_status(500)
        .create_async()
        .await;

    let cfg = ActionsOidcAuthConfig {
        name: "test".to_owned(),
        issuer_url: base.clone(),
        required: false,
        audience: TEST_AUDIENCE.to_owned(),
        user_id_claim: "sub".to_owned(),
        rules: vec![],
    };

    assert!(ActionsOidcAuthProvider::new(&cfg).await.is_err());
}

/// Provider construction must fail when the discovery document is not valid JSON.
#[tokio::test]
async fn bootstrap_fails_on_malformed_discovery_json() {
    let mut server = mockito::Server::new_async().await;

    let _mock = server
        .mock("GET", "/.well-known/openid-configuration")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("not json at all")
        .create_async()
        .await;

    let cfg = ActionsOidcAuthConfig {
        name: "test".to_owned(),
        issuer_url: server.url(),
        required: false,
        audience: TEST_AUDIENCE.to_owned(),
        user_id_claim: "sub".to_owned(),
        rules: vec![],
    };

    assert!(ActionsOidcAuthProvider::new(&cfg).await.is_err());
}

/// A TOML-deserialised config (the normal runtime path) boots correctly and
/// the provider can authenticate a signed JWT.
#[tokio::test]
async fn toml_config_deserialises_and_boots() {
    let mut server = mockito::Server::new_async().await;
    let base = server.url();

    let _disc = server
        .mock("GET", "/.well-known/openid-configuration")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(discovery_json(&base))
        .create_async()
        .await;
    let _jwks = server
        .mock("GET", "/jwks")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(TEST_JWKS_JSON)
        .create_async()
        .await;

    let toml_src = format!(
        r#"
name = "ci-provider"
issuer_url = "{base}"
audience = "{aud}"
user_id_claim = "sub"

[[rules]]
group = "ci-users"
role = "user"
"#,
        aud = TEST_AUDIENCE,
    );

    let cfg: ActionsOidcAuthConfig = toml::from_str(&toml_src).expect("TOML parse failed");
    assert_eq!(cfg.name, "ci-provider");
    assert_eq!(cfg.rules.len(), 1);

    let provider = ActionsOidcAuthProvider::new(&cfg)
        .await
        .expect("provider construction failed");

    let token = signed_token(
        Some("test-kid"),
        json!({
            "sub": "pipeline",
            "exp": future_exp(),
            "aud": TEST_AUDIENCE,
            "iss": base,
        }),
    );
    let id = provider
        .authenticate(&bearer(&token))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(id.user_id.as_deref(), Some("pipeline"));
    assert_eq!(id.groups, vec!["ci-users"]);
}
