use super::*;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

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

fn test_jwks() -> JwkSet {
    serde_json::from_str(TEST_JWKS_JSON).unwrap()
}

fn future_exp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 3600
}

fn past_exp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - 3600
}

fn signed_token(extra_header_kid: Option<&str>, claims: serde_json::Value) -> String {
    let header = Header {
        alg: Algorithm::ES256,
        kid: extra_header_kid.map(str::to_owned),
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

fn no_auth() -> RawAuthRequest {
    RawAuthRequest {
        headers: Default::default(),
        query_params: Default::default(),
        source_ip: None,
    }
}

fn no_rules_provider() -> ActionsOidcAuthProvider {
    ActionsOidcAuthProvider::for_testing("actions", "sub", vec![], test_jwks())
}

// ── Template rendering ────────────────────────────────────────────────────

fn claims_map(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match v {
        serde_json::Value::Object(m) => m,
        _ => panic!("expected object"),
    }
}

#[test]
fn template_repo_and_branch() {
    let claims = claims_map(json!({
        "repository": "batleforc/batlehub",
        "ref": "refs/heads/main"
    }));
    let out = render_group_template("{name}/{repository}/{ref_name}", "forgejo-action", &claims);
    assert_eq!(out, "forgejo-action/batleforc-batlehub/main");
}

#[test]
fn template_tag_ref_name() {
    let claims = claims_map(json!({ "ref": "refs/tags/v1.2.3" }));
    let out = render_group_template("{name}/{ref_name}", "ci", &claims);
    assert_eq!(out, "ci/v1.2.3");
}

#[test]
fn template_bare_ref_without_prefix() {
    let claims = claims_map(json!({ "ref": "main" }));
    let out = render_group_template("{ref_name}", "ci", &claims);
    assert_eq!(out, "main");
}

#[test]
fn template_missing_claim_left_as_placeholder() {
    let claims = claims_map(json!({}));
    let out = render_group_template("{missing}", "ci", &claims);
    assert_eq!(out, "{missing}");
}

#[test]
fn template_provider_name_slash_replaced() {
    let claims = claims_map(json!({}));
    let out = render_group_template("{name}", "a/b", &claims);
    assert_eq!(out, "a-b");
}

// ── Pattern matching ──────────────────────────────────────────────────────

#[test]
fn glob_matches_wildcard() {
    let c = batlehub_core::ports::Condition {
        claim: "repository".to_owned(),
        pattern: "myorg/*".to_owned(),
        match_type: ConditionMatchType::Auto,
    };
    let cc = CompiledCondition::compile(&c).unwrap();
    let claims = claims_map(json!({ "repository": "myorg/foo" }));
    assert!(cc.matches(&claims));
    let claims2 = claims_map(json!({ "repository": "other/foo" }));
    assert!(!cc.matches(&claims2));
}

#[test]
fn regex_matches_tag_pattern() {
    let c = batlehub_core::ports::Condition {
        claim: "ref".to_owned(),
        pattern: "^refs/tags/v[0-9]+".to_owned(),
        match_type: ConditionMatchType::Auto,
    };
    let cc = CompiledCondition::compile(&c).unwrap();
    let claims = claims_map(json!({ "ref": "refs/tags/v1.0.0" }));
    assert!(cc.matches(&claims));
    let claims2 = claims_map(json!({ "ref": "refs/heads/main" }));
    assert!(!cc.matches(&claims2));
}

#[test]
fn auto_detect_regex_on_caret() {
    assert!(detect_is_regex("^refs/tags/"));
    assert!(!detect_is_regex("refs/heads/*"));
}

#[test]
fn absent_claim_does_not_match() {
    let c = batlehub_core::ports::Condition {
        claim: "missing".to_owned(),
        pattern: "something".to_owned(),
        match_type: ConditionMatchType::Glob,
    };
    let cc = CompiledCondition::compile(&c).unwrap();
    let claims = claims_map(json!({}));
    assert!(!cc.matches(&claims));
}

#[test]
fn explicit_glob_with_regex_chars() {
    let c = batlehub_core::ports::Condition {
        claim: "ref".to_owned(),
        pattern: "refs/heads/main".to_owned(),
        match_type: ConditionMatchType::Glob,
    };
    let cc = CompiledCondition::compile(&c).unwrap();
    let claims = claims_map(json!({ "ref": "refs/heads/main" }));
    assert!(cc.matches(&claims));
}

// ── CompiledRule ──────────────────────────────────────────────────────────

fn make_rule(
    static_group: Option<&str>,
    template: Option<&str>,
    role: Option<Role>,
    conditions: Vec<CompiledCondition>,
    match_mode: RuleMatch,
) -> CompiledRule {
    CompiledRule {
        static_group: static_group.map(str::to_owned),
        group_template: template.map(str::to_owned),
        role,
        conditions,
        match_mode,
    }
}

#[test]
fn rule_all_requires_all_conditions() {
    let cond1 = CompiledCondition::compile(&batlehub_core::ports::Condition {
        claim: "repository_owner".to_owned(),
        pattern: "myorg".to_owned(),
        match_type: ConditionMatchType::Glob,
    })
    .unwrap();
    let cond2 = CompiledCondition::compile(&batlehub_core::ports::Condition {
        claim: "ref".to_owned(),
        pattern: "refs/heads/main".to_owned(),
        match_type: ConditionMatchType::Glob,
    })
    .unwrap();
    let rule = make_rule(
        Some("g"),
        None,
        Some(Role::User),
        vec![cond1, cond2],
        RuleMatch::All,
    );

    let both = claims_map(json!({ "repository_owner": "myorg", "ref": "refs/heads/main" }));
    assert!(rule.evaluate(&both));

    let only_one = claims_map(json!({ "repository_owner": "myorg", "ref": "refs/heads/dev" }));
    assert!(!rule.evaluate(&only_one));
}

#[test]
fn rule_any_requires_one_condition() {
    let cond1 = CompiledCondition::compile(&batlehub_core::ports::Condition {
        claim: "ref_type".to_owned(),
        pattern: "branch".to_owned(),
        match_type: ConditionMatchType::Glob,
    })
    .unwrap();
    let cond2 = CompiledCondition::compile(&batlehub_core::ports::Condition {
        claim: "ref_type".to_owned(),
        pattern: "tag".to_owned(),
        match_type: ConditionMatchType::Glob,
    })
    .unwrap();
    let rule = make_rule(
        Some("g"),
        None,
        Some(Role::User),
        vec![cond1, cond2],
        RuleMatch::Any,
    );

    let branch = claims_map(json!({ "ref_type": "branch" }));
    assert!(rule.evaluate(&branch));
    let tag = claims_map(json!({ "ref_type": "tag" }));
    assert!(rule.evaluate(&tag));
    let other = claims_map(json!({ "ref_type": "other" }));
    assert!(!rule.evaluate(&other));
}

#[test]
fn empty_conditions_always_match() {
    let rule = make_rule(Some("g"), None, Some(Role::User), vec![], RuleMatch::All);
    assert!(rule.evaluate(&claims_map(json!({}))));
}

#[test]
fn collect_groups_static_only() {
    let rule = make_rule(
        Some("my-group"),
        None,
        Some(Role::User),
        vec![],
        RuleMatch::All,
    );
    let groups = rule.collect_groups("prov", &claims_map(json!({})));
    assert_eq!(groups, vec!["my-group"]);
}

#[test]
fn collect_groups_template_only() {
    let rule = make_rule(
        None,
        Some("{name}/{ref_name}"),
        Some(Role::User),
        vec![],
        RuleMatch::All,
    );
    let claims = claims_map(json!({ "ref": "refs/heads/main" }));
    let groups = rule.collect_groups("prov", &claims);
    assert_eq!(groups, vec!["prov/main"]);
}

#[test]
fn collect_groups_both() {
    let rule = make_rule(
        Some("static"),
        Some("{name}/{ref_name}"),
        Some(Role::User),
        vec![],
        RuleMatch::All,
    );
    let claims = claims_map(json!({ "ref": "refs/heads/feat" }));
    let groups = rule.collect_groups("ci", &claims);
    assert_eq!(groups, vec!["static", "ci/feat"]);
}

// ── Provider authenticate ─────────────────────────────────────────────────

#[tokio::test]
async fn no_auth_header_returns_none() {
    let p = no_rules_provider();
    assert!(p.authenticate(&no_auth()).await.unwrap().is_none());
}

#[tokio::test]
async fn basic_auth_header_returns_none() {
    let p = no_rules_provider();
    let req = RawAuthRequest {
        headers: [("authorization".to_owned(), "Basic dXNlcjpwYXNz".to_owned())].into(),
        query_params: Default::default(),
        source_ip: None,
    };
    assert!(p.authenticate(&req).await.unwrap().is_none());
}

#[tokio::test]
async fn malformed_token_returns_auth_error() {
    let p = no_rules_provider();
    let err = p
        .authenticate(&bearer("not.a.valid.jwt"))
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Auth(_)));
}

#[tokio::test]
async fn valid_jwt_no_rules_returns_anonymous_empty_groups() {
    let p = no_rules_provider();
    let token = signed_token(
        Some("test-kid"),
        json!({ "sub": "ci-bot", "repository": "myorg/myrepo", "ref": "refs/heads/main", "exp": future_exp(), "aud": TEST_AUDIENCE }),
    );
    let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
    assert_eq!(id.role, Role::Anonymous);
    assert!(id.groups.is_empty());
    assert_eq!(id.user_id.as_deref(), Some("ci-bot"));
    assert_eq!(id.auth_provider.as_deref(), Some("actions"));
}

#[tokio::test]
async fn matching_rule_grants_group_and_role() {
    let cond = CompiledCondition::compile(&batlehub_core::ports::Condition {
        claim: "repository_owner".to_owned(),
        pattern: "myorg".to_owned(),
        match_type: ConditionMatchType::Glob,
    })
    .unwrap();
    let rule = make_rule(
        Some("ci-group"),
        None,
        Some(Role::User),
        vec![cond],
        RuleMatch::All,
    );
    let p = ActionsOidcAuthProvider::for_testing("actions", "sub", vec![rule], test_jwks());

    let token = signed_token(
        Some("test-kid"),
        json!({ "sub": "bot", "repository_owner": "myorg", "exp": future_exp(), "aud": TEST_AUDIENCE }),
    );
    let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
    assert_eq!(id.role, Role::User);
    assert_eq!(id.groups, vec!["ci-group"]);
}

#[tokio::test]
async fn two_matching_rules_union_groups_max_role() {
    let cond1 = CompiledCondition::compile(&batlehub_core::ports::Condition {
        claim: "repository_owner".to_owned(),
        pattern: "myorg".to_owned(),
        match_type: ConditionMatchType::Glob,
    })
    .unwrap();
    let cond2 = CompiledCondition::compile(&batlehub_core::ports::Condition {
        claim: "repository_owner".to_owned(),
        pattern: "myorg".to_owned(),
        match_type: ConditionMatchType::Glob,
    })
    .unwrap();
    let rule1 = make_rule(
        Some("group-a"),
        None,
        Some(Role::User),
        vec![cond1],
        RuleMatch::All,
    );
    let rule2 = make_rule(
        Some("group-b"),
        None,
        Some(Role::Admin),
        vec![cond2],
        RuleMatch::All,
    );
    let p = ActionsOidcAuthProvider::for_testing("actions", "sub", vec![rule1, rule2], test_jwks());

    let token = signed_token(
        Some("test-kid"),
        json!({ "sub": "bot", "repository_owner": "myorg", "exp": future_exp(), "aud": TEST_AUDIENCE }),
    );
    let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
    assert_eq!(id.role, Role::Admin);
    assert!(id.groups.contains(&"group-a".to_owned()));
    assert!(id.groups.contains(&"group-b".to_owned()));
}

#[tokio::test]
async fn template_rule_renders_dynamic_group() {
    let rule = make_rule(
        None,
        Some("{name}/{repository}/{ref_name}"),
        Some(Role::User),
        vec![],
        RuleMatch::All,
    );
    let p = ActionsOidcAuthProvider::for_testing("forgejo-action", "sub", vec![rule], test_jwks());

    let token = signed_token(
        Some("test-kid"),
        json!({
            "sub": "bot",
            "repository": "batleforc/batlehub",
            "ref": "refs/heads/main",
            "exp": future_exp(), "aud": TEST_AUDIENCE
        }),
    );
    let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
    assert_eq!(id.groups, vec!["forgejo-action/batleforc-batlehub/main"]);
}

#[tokio::test]
async fn non_matching_rule_contributes_nothing() {
    let cond = CompiledCondition::compile(&batlehub_core::ports::Condition {
        claim: "repository_owner".to_owned(),
        pattern: "other-org".to_owned(),
        match_type: ConditionMatchType::Glob,
    })
    .unwrap();
    let rule = make_rule(
        Some("group"),
        None,
        Some(Role::Admin),
        vec![cond],
        RuleMatch::All,
    );
    let p = ActionsOidcAuthProvider::for_testing("actions", "sub", vec![rule], test_jwks());

    let token = signed_token(
        Some("test-kid"),
        json!({ "sub": "bot", "repository_owner": "myorg", "exp": future_exp(), "aud": TEST_AUDIENCE }),
    );
    let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
    assert_eq!(id.role, Role::Anonymous);
    assert!(id.groups.is_empty());
}

#[tokio::test]
async fn expired_jwt_returns_none() {
    let p = no_rules_provider();
    let token = signed_token(
        Some("test-kid"),
        json!({ "sub": "bot", "exp": past_exp(), "aud": TEST_AUDIENCE }),
    );
    assert!(p.authenticate(&bearer(&token)).await.unwrap().is_none());
}

#[tokio::test]
async fn unknown_kid_returns_auth_error() {
    let p = no_rules_provider();
    let token = signed_token(
        Some("bad-kid"),
        json!({ "sub": "bot", "exp": future_exp(), "aud": TEST_AUDIENCE }),
    );
    assert!(matches!(
        p.authenticate(&bearer(&token)).await.unwrap_err(),
        CoreError::Auth(_)
    ));
}

// ── Network bootstrap (mockito) ───────────────────────────────────────────

fn discovery_json(base_url: &str) -> String {
    serde_json::json!({
        "issuer": base_url,
        "jwks_uri": format!("{base_url}/jwks"),
    })
    .to_string()
}

#[tokio::test]
async fn new_bootstraps_from_discovery_document() {
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
        name: "test".to_owned(),
        issuer_url: base.clone(),
        required: false,
        audience: TEST_AUDIENCE.to_owned(),
        user_id_claim: "sub".to_owned(),
        rules: vec![],
    };

    let provider = ActionsOidcAuthProvider::new(&cfg)
        .await
        .expect("provider construction failed");
    assert_eq!(provider.name(), "test");
}

// ── Header variant tests ──────────────────────────────────────────────────

#[tokio::test]
async fn bearer_lowercase_prefix_accepted() {
    let p = no_rules_provider();
    let token = signed_token(
        Some("test-kid"),
        json!({ "sub": "bot", "exp": future_exp(), "aud": TEST_AUDIENCE }),
    );
    let req = RawAuthRequest {
        headers: [("authorization".to_owned(), format!("bearer {token}"))].into(),
        query_params: Default::default(),
        source_ip: None,
    };
    let id = p.authenticate(&req).await.unwrap().unwrap();
    assert_eq!(id.user_id.as_deref(), Some("bot"));
}

#[tokio::test]
async fn authorization_capitalized_header_accepted() {
    let p = no_rules_provider();
    let token = signed_token(
        Some("test-kid"),
        json!({ "sub": "bot", "exp": future_exp(), "aud": TEST_AUDIENCE }),
    );
    let req = RawAuthRequest {
        headers: [("Authorization".to_owned(), format!("Bearer {token}"))].into(),
        query_params: Default::default(),
        source_ip: None,
    };
    let id = p.authenticate(&req).await.unwrap().unwrap();
    assert_eq!(id.user_id.as_deref(), Some("bot"));
}

// ── JWKS key lookup ───────────────────────────────────────────────────────

#[tokio::test]
async fn no_kid_in_token_uses_first_jwk() {
    let p = no_rules_provider();
    // Token signed with the same key but no kid in the header
    let token = signed_token(
        None,
        json!({ "sub": "bot", "exp": future_exp(), "aud": TEST_AUDIENCE }),
    );
    let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
    assert_eq!(id.user_id.as_deref(), Some("bot"));
}

// ── detect_is_regex remaining triggers ───────────────────────────────────

#[test]
fn detect_is_regex_remaining_triggers() {
    assert!(detect_is_regex("foo$"));
    assert!(detect_is_regex("foo(?:bar)"));
    assert!(detect_is_regex("\\d+"));
    assert!(detect_is_regex("\\w+"));
    assert!(detect_is_regex("[abc]"));
    assert!(detect_is_regex("a(b)c"));
    assert!(detect_is_regex("a+b"));
    assert!(!detect_is_regex("plain-glob-*"));
}

// ── Template edge cases ───────────────────────────────────────────────────

#[test]
fn template_unclosed_brace_preserved() {
    let claims = claims_map(json!({}));
    let out = render_group_template("{unclosed", "ci", &claims);
    assert_eq!(out, "{unclosed");
}

#[test]
fn template_claim_with_slash_replaced() {
    let claims = claims_map(json!({ "repository": "org/sub-project" }));
    let out = render_group_template("{repository}", "ci", &claims);
    assert_eq!(out, "org-sub-project");
}

// ── Rule edge cases ───────────────────────────────────────────────────────

#[tokio::test]
async fn rule_with_no_role_contributes_group_only() {
    let rule = make_rule(Some("group-only"), None, None, vec![], RuleMatch::All);
    let p = ActionsOidcAuthProvider::for_testing("actions", "sub", vec![rule], test_jwks());

    let token = signed_token(
        Some("test-kid"),
        json!({ "sub": "bot", "exp": future_exp(), "aud": TEST_AUDIENCE }),
    );
    let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
    assert_eq!(id.role, Role::Anonymous);
    assert_eq!(id.groups, vec!["group-only"]);
}

#[test]
fn compiled_rule_compile_error_neither_group_nor_template() {
    let rule_cfg = batlehub_core::ports::ActionsGroupRule {
        group: None,
        group_template: None,
        role: None,
        conditions: vec![],
        match_mode: batlehub_core::ports::RuleMatch::All,
    };
    assert!(CompiledRule::compile(&rule_cfg).is_err());
}

// ── role parsing ──────────────────────────────────────────────────────────

#[test]
fn compiled_rule_compile_error_unknown_role() {
    let rule_cfg = batlehub_core::ports::ActionsGroupRule {
        group: Some("g".to_owned()),
        group_template: None,
        role: Some("superadmin".to_owned()),
        conditions: vec![],
        match_mode: batlehub_core::ports::RuleMatch::All,
    };
    assert!(CompiledRule::compile(&rule_cfg).is_err());
}

// ── user_id claim edge cases ──────────────────────────────────────────────

#[tokio::test]
async fn user_id_missing_claim_gives_none() {
    let p = no_rules_provider(); // user_id_claim = "sub"
    let token = signed_token(
        Some("test-kid"),
        json!({ "exp": future_exp(), "aud": TEST_AUDIENCE }), // no "sub"
    );
    let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
    assert!(id.user_id.is_none());
}

#[tokio::test]
async fn user_id_non_string_claim_gives_none() {
    let p = no_rules_provider(); // user_id_claim = "sub"
    let token = signed_token(
        Some("test-kid"),
        json!({ "sub": 42, "exp": future_exp(), "aud": TEST_AUDIENCE }), // "sub" is a number
    );
    let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
    assert!(id.user_id.is_none());
}

// ── JWKS stale cache refresh ──────────────────────────────────────────────

#[tokio::test]
async fn jwks_stale_cache_triggers_refresh() {
    let mut server = mockito::Server::new_async().await;

    let jwks_mock = server
        .mock("GET", "/jwks")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(TEST_JWKS_JSON)
        .create_async()
        .await;

    // Start with an empty JWKS (no keys) and a stale cache so the refresh path fires.
    let empty_jwks: JwkSet = serde_json::from_str(r#"{"keys":[]}"#).unwrap();
    let p = ActionsOidcAuthProvider::for_testing_stale(
        "actions",
        "sub",
        vec![],
        empty_jwks,
        format!("{}/jwks", server.url()),
    );

    let token = signed_token(
        Some("test-kid"),
        json!({ "sub": "bot", "exp": future_exp(), "aud": TEST_AUDIENCE }),
    );
    let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
    assert_eq!(id.user_id.as_deref(), Some("bot"));
    jwks_mock.assert_async().await;
}

// ── Audience ──────────────────────────────────────────────────────────────────
// The load-bearing check for this provider. Its issuer —
// `token.actions.githubusercontent.com` — signs for every repository on GitHub,
// and `aud` is the one claim the calling workflow chooses. `validate_aud` used
// to be off, which left `rules` as the only thing standing between any workflow
// on the forge and an authenticated identity here.

#[tokio::test]
async fn a_token_minted_for_another_audience_is_refused() {
    let p = ActionsOidcAuthProvider::for_testing("gh", "sub", vec![], test_jwks());
    let token = signed_token(
        Some("test-kid"),
        json!({
            "sub": "repo:someone-else/theirs:ref:refs/heads/main",
            "repository": "someone-else/theirs",
            "exp": future_exp(),
            "aud": "https://some-other-deployment.example",
        }),
    );
    assert!(
        p.authenticate(&bearer(&token)).await.unwrap().is_none(),
        "a correctly-signed token from this issuer is not enough — any repo can get one"
    );
}

#[tokio::test]
async fn a_token_with_no_audience_is_refused() {
    let p = ActionsOidcAuthProvider::for_testing("gh", "sub", vec![], test_jwks());
    let token = signed_token(
        Some("test-kid"),
        json!({ "sub": "repo:o/r:ref:refs/heads/main", "exp": future_exp() }),
    );
    assert!(p.authenticate(&bearer(&token)).await.unwrap().is_none());
}

#[tokio::test]
async fn an_empty_audience_is_rejected_at_construction() {
    // Rejected before any network call: this is a config error, and reporting it
    // as "the identity provider was unreachable" would send the operator to the
    // wrong place. `new` warns-and-continues at the call site in `setup.rs`, so
    // the message has to say what to fix.
    let cfg = ActionsOidcAuthConfig {
        name: "forgejo".to_owned(),
        issuer_url: "https://forge.invalid".to_owned(),
        required: false,
        audience: "   ".to_owned(),
        user_id_claim: "sub".to_owned(),
        rules: vec![],
    };
    let Err(err) = ActionsOidcAuthProvider::new(&cfg).await else {
        panic!("an empty audience must not produce a working provider");
    };
    let msg = err.to_string();
    assert!(msg.contains("audience"), "got: {msg}");
    assert!(
        msg.contains("forgejo"),
        "names the offending provider: {msg}"
    );
}
