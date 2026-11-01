use std::sync::Arc;

use actix_web::{post, web, Responder};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use batlehub_core::{
    entities::{
        resolve, Action, ActionParseError, Identity, PackageId, PackageMetadata, Role, Subject,
    },
    ports::{IpBlockStore, UserBlockRepository},
    rules::{RuleContext, RuleDecision},
    services::ProxyService,
};

use crate::{error::AppError, extractors::AuthIdentity};

#[derive(Deserialize, ToSchema)]
pub struct AccessCheckRequest {
    pub registry: String,
    pub package_name: String,
    pub version: String,
    /// The permission to simulate, e.g. `releases:read`.
    ///
    /// Parsed into [`Action`] rather than compared as a string. Before RFC 0015
    /// phase 1 an unknown value here was answered rather than refused: the rule
    /// chain compared it to the config's strings, nothing matched, and the
    /// simulator returned **deny** with a rule name attached — a confident
    /// answer about a permission that does not exist. The page whose whole
    /// purpose is "would this identity be allowed" is the worst possible place
    /// for a typo to look like a policy decision.
    pub resource_type: String,
    /// Simulated user id (optional).
    pub user_id: Option<String>,
    /// Simulated role: "anonymous", "user", or "admin". Defaults to "anonymous".
    pub role: Option<String>,
    /// Simulated OIDC groups the identity belongs to.
    #[serde(default)]
    pub groups: Vec<String>,
    /// Simulated client address, checked against the IP block list.
    ///
    /// Optional, and its absence is reported rather than assumed: a simulation
    /// with no address cannot answer for the IP block layer, and answering
    /// "allow" because none was supplied is the same defect this endpoint had
    /// one level down (RFC 0004-bis B4). See `covers` on the response.
    pub client_ip: Option<String>,
}

/// Which enforcement layers the answer actually accounts for.
///
/// The simulator used to evaluate `policy.rules` and nothing else, so an admin
/// who blocked `alice` on `/admin/security/blocks` and simulated `alice` on the
/// next tab was told **allow** — the page whose entire purpose is "would this
/// identity be allowed" contradicted by the section it lives in.
///
/// Stating coverage is part of the fix rather than a nicety: two of the three
/// layers are only checkable when the caller supplies the input they key on, so
/// a bare `allow` is ambiguous between "nothing denies this" and "nothing I
/// looked at denies this".
#[derive(Serialize, ToSchema)]
pub struct SimulationCoverage {
    /// Always true — the registry's rule chain is evaluated in full.
    pub rules: bool,
    /// True when `user_id` was supplied, so the account block list was consulted.
    pub account_blocks: bool,
    /// True when `client_ip` was supplied, so the IP block list was consulted.
    pub ip_blocks: bool,
}

/// What denied the request, when something did.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlockedBy {
    /// The account is on the manual user block list.
    Account,
    /// The address is on the IP block list.
    Ip,
    /// A registry rule denied it.
    Rule,
}

#[derive(Serialize, ToSchema)]
pub struct AccessSimulationResponse {
    /// "allow" or "deny".
    pub decision: String,
    /// Present when decision is "deny".
    pub reason: Option<String>,
    /// Name of the rule that triggered the deny, if any.
    pub rule_matched: Option<String>,
    /// Which layer denied, when one did.
    pub blocked_by: Option<BlockedBy>,
    /// The layers this answer accounts for.
    pub covers: SimulationCoverage,
}

fn parse_role(s: Option<&str>) -> Result<Role, AppError> {
    match s {
        Some(value) => value
            .parse::<Role>()
            .map_err(|_| AppError::bad_request("role must be 'anonymous', 'user', or 'admin'")),
        None => Ok(Role::Anonymous),
    }
}

/// Simulate whether a given identity would be allowed to perform an operation
/// against a registry's policy without issuing a real request (admin).
#[utoipa::path(
    post,
    path = "/api/v1/admin/access-check",
    tag = "back-office",
    request_body = AccessCheckRequest,
    responses(
        (status = 200, description = "Simulation result", body = AccessSimulationResponse),
        (status = 403, description = "`authz:read` required"),
        (status = 404, description = "Registry not configured"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/access-check")]
pub async fn admin_access_check(
    identity: AuthIdentity,
    proxy_svc: web::Data<Arc<ProxyService>>,
    user_blocks: web::Data<Arc<dyn UserBlockRepository>>,
    ip_blocks: web::Data<Arc<dyn IpBlockStore>>,
    body: web::Json<AccessCheckRequest>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::AuthzRead,
        None,
        &hot,
    )
    .await?;

    let policy = {
        let hot = proxy_svc.hot.read().await;
        // The chain the package is judged by, namespace overrides included —
        // the same selection a real download makes.
        batlehub_core::services::authz::policy_in(&hot, &body.registry, &body.package_name)
    };

    let policy = policy.ok_or_else(|| AppError::not_found("registry not configured"))?;

    let covers = SimulationCoverage {
        rules: true,
        account_blocks: body.user_id.is_some(),
        ip_blocks: body.client_ip.is_some(),
    };

    /* The block layers first, and in this order, because that is where a real
    request meets them: `IpBlockMiddleware` and `UserBlockMiddleware` both
    reject *before* any rule is evaluated, so a simulator that starts at the
    rule chain is answering a different question than the one it was asked.
    Ordering matters for `blocked_by` too — an address on the IP list never
    reaches the account check in production, so it must not here either. */
    if let Some(ip) = body.client_ip.as_deref() {
        if let Some(unblock_at) = ip_blocks.blocked_until(ip).await? {
            return Ok(web::Json(AccessSimulationResponse {
                decision: "deny".to_owned(),
                reason: Some(format!("IP {ip} is blocked until {unblock_at}")),
                rule_matched: None,
                blocked_by: Some(BlockedBy::Ip),
                covers,
            }));
        }
    }

    if let Some(user_id) = body.user_id.as_deref() {
        if user_blocks.is_blocked(user_id).await? {
            return Ok(web::Json(AccessSimulationResponse {
                decision: "deny".to_owned(),
                reason: Some(format!("account '{user_id}' is blocked")),
                rule_matched: None,
                blocked_by: Some(BlockedBy::Account),
                covers,
            }));
        }
    }

    let sim_identity = Identity {
        user_id: body.user_id.clone(),
        role: parse_role(body.role.as_deref())?,
        auth_provider: None,
        groups: body.groups.clone(),
    };

    let package_id = PackageId::new(&body.registry, &body.package_name, &body.version);
    let metadata = PackageMetadata {
        id: package_id.clone(),
        published_at: None,
        download_url: None,
        checksum: None,
        is_signed: None,
        extra: serde_json::Value::Null,
        cache_control: None,
    };

    let action: Action = body
        .resource_type
        .parse()
        .map_err(|e: ActionParseError| AppError::bad_request(format!("resource_type: {e}")))?;

    // Grants first, exactly as a real request meets them.
    //
    // Not optional, and not cosmetic. `RbacRule` left the chain in RFC 0015
    // phase 3 (§5.1), so a simulator that evaluates `policy.rules` alone no
    // longer sees the layer that answers "may this caller do this at all" — it
    // reported **allow** for a caller grants refuse. That is the same defect
    // RFC 0004-bis B4 records one layer down, on the same endpoint: the page
    // whose entire purpose is "would this identity be allowed" contradicting the
    // system it describes.
    let grants = {
        let hot = proxy_svc.hot.read().await;
        hot.grants.get(body.registry.as_str()).cloned()
    };
    if let Some(grants) = grants {
        let subject = Subject::Identity(sim_identity.clone());
        // The whole path — the instance tier above, and the stored package and
        // version tiers below. See `authz_explain`. This is the endpoint RFC
        // 0004-bis B4 records for answering *allow* where the system answered
        // *deny*; resolving against a partial hierarchy is the same defect from
        // the other direction, and a simulator that stops before the tiers RFC
        // 0017's editor writes simulates a server nobody is running.
        let path = batlehub_core::services::authz::resolution_path_for_coordinate(
            &proxy_svc.hot,
            &grants,
            &package_id,
        )
        .await
        .map_err(AppError::from)?;
        let resolved = resolve(&path, &subject);
        if !resolved.holds(action) {
            return Ok(web::Json(AccessSimulationResponse {
                decision: "deny".to_owned(),
                reason: Some(format!(
                    "no grant for '{action}' on registry '{}'",
                    body.registry
                )),
                rule_matched: Some("grants".to_owned()),
                blocked_by: Some(BlockedBy::Rule),
                covers,
            }));
        }
    }

    let ctx = RuleContext {
        identity: &sim_identity,
        package: &metadata,
        action,
        cache_entry: None,
        requested_version: Some(&body.version),
    };

    // The real chain's own evaluation — gate exemptions included — so the
    // simulator cannot answer differently from the download it simulates.
    let (decision, rule_matched) = batlehub_core::services::authz::evaluate_chain(
        &proxy_svc.hot,
        &policy.rules,
        &metadata.id,
        &ctx,
    )
    .await;

    let response = match decision {
        RuleDecision::Allow => AccessSimulationResponse {
            decision: "allow".to_owned(),
            reason: None,
            rule_matched,
            blocked_by: None,
            covers,
        },
        RuleDecision::Deny { reason } => AccessSimulationResponse {
            decision: "deny".to_owned(),
            reason: Some(reason),
            rule_matched,
            blocked_by: Some(BlockedBy::Rule),
            covers,
        },
    };

    Ok(web::Json(response))
}
