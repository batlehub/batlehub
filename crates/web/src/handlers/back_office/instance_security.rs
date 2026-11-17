//! `GET /api/v1/admin/instance-security` (RFC 0050 §6.4): the security of
//! the instance itself — failed sign-ins, bans, tokens seen from a new
//! address, static tokens stored in clear, expiring exemptions and the
//! security config warnings. Read with `system:read`; IP addresses and user
//! ids only with `audit:read` (T3), bans with `blocks:read`, warnings with
//! `config:read`, exemptions with `quarantine:read` and per coordinate (T6).
//! No static token's value is ever read here: the config crate hands over
//! paths and user ids only (T4).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use actix_web::{get, web, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use batlehub_config::schema::warnings as codes;
use batlehub_core::entities::{AccessAction, AccessEvent, Action, EventFilter, PackageId};
use batlehub_core::ports::IpBlockStore;
use batlehub_core::services::hot_config::HotConfigLock;
use batlehub_core::services::security_overview::LineState;
use batlehub_core::services::AdminService;

use crate::error::AppError;
use crate::extractors::AuthIdentity;
use crate::handlers::security_console::{
    cache_of, coords_held, exemptions_source, holds_at, SourceCache, SOURCE_TIMEOUT, SOURCE_TTL,
};
use crate::services::ConfigReloadService;

/// The security warnings this page shows (RFC 0050 §6.4). Licence, air-gap
/// and visibility warnings stay on the Configuration page: they are about
/// what is served, not the instance's own exposure.
pub const SECURITY_WARNING_CODES: &[&str] = &[
    codes::AUDIT_FULL_IPS_KEPT,
    codes::AUDIT_NO_RETENTION,
    codes::AUDIT_SEALING_WITHOUT_STREAM,
    codes::AUTH_PLAIN_STATIC_TOKEN,
    codes::CORS_ANY_ORIGIN,
    codes::FLAG_SOURCE_CAN_HARD_BLOCK,
    codes::GRANTS_IN_SHADOW,
    codes::PROXY_TRUST_DEPRECATED_KEY_ONLY,
    codes::PROXY_TRUST_INVALID_DEPRECATED_ENTRY,
    codes::PROXY_TRUST_SHADOWED_DEPRECATED_KEY,
    codes::PROXY_TRUST_UNCONFIGURED,
    codes::SECURITY_ENRICHMENT_REQUIRED,
    codes::SECURITY_RULE_SETTINGS_IGNORED,
    codes::SECURITY_SCANNER_ERROR_OPEN,
    codes::SECURITY_TIMESTAMP_HOLD_UNAVAILABLE,
    codes::SECURITY_UNKNOWN_SERVED,
    codes::SECURITY_UNPROTECTED,
    codes::SIGNED_URLS_ANONYMOUS_STILL_GRANTED,
    codes::SIGNED_URLS_UNUSED,
];

/// The most auth events one window reads.
const EVENTS_READ: u64 = 10_000;
/// The most addresses listed.
const ADDRESSES_SHOWN: usize = 20;

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct InstanceSecurityQuery {
    /// `24h` or `7d`; anything else reads as `24h`.
    pub window: Option<String>,
}

/// An address and how many rejected credentials came from it.
#[derive(Debug, Serialize, ToSchema)]
pub struct AddressCount {
    pub ip: String,
    pub count: u64,
}

/// Rejected credentials in the window.
#[derive(Debug, Serialize, ToSchema)]
pub struct SignInBlock {
    pub state: LineState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<String>,
    pub rejected: u64,
    /// `sign_in_failed` (single sign-on) and `credential_rejected` (token).
    pub by_kind: BTreeMap<String, u64>,
    /// Only with `audit:read`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addresses: Option<Vec<AddressCount>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distinct_addresses: Option<u64>,
    /// The window held more events than one read takes: every count above is
    /// a floor ("at least"), never the exact figure.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
}

/// Active IP bans. Only with `blocks:read`.
#[derive(Debug, Serialize, ToSchema)]
pub struct BansBlock {
    pub active: u64,
    pub automatic: u64,
    /// Set when the ban store did not answer: the counts are then zero and
    /// mean nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<String>,
}

/// A token used from an address it had not been used from.
#[derive(Debug, Serialize, ToSchema)]
pub struct TokenSourceItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TokenSourcesBlock {
    pub new_source: u64,
    /// Only with `audit:read`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<TokenSourceItem>>,
    /// Set when the audit log did not answer: `new_source` is then zero and
    /// means nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<String>,
    /// The window held more events than one read takes (a rejection burst
    /// shares the read): `new_source` is a floor, not the exact figure.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
}

/// `[[auth.tokens]]` values stored in clear: config paths, never the value.
#[derive(Debug, Serialize, ToSchema)]
pub struct StaticTokensBlock {
    pub plain: u64,
    pub paths: Vec<String>,
    /// Only with `audit:read`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_ids: Option<Vec<String>>,
}

/// An exemption ending within seven days.
#[derive(Debug, Serialize, ToSchema)]
pub struct ExemptionItem {
    pub registry: String,
    pub name: String,
    pub version: String,
    pub expires_at: DateTime<Utc>,
}

/// Gate exemptions. Only with `quarantine:read`, each through its seal.
#[derive(Debug, Serialize, ToSchema)]
pub struct ExemptionsBlock {
    pub expiring_7d: u64,
    /// Live exemptions approved by the publisher of the version.
    pub self_approved: u64,
    pub items: Vec<ExemptionItem>,
    /// Set when the policy store did not answer (the other lines still do).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<String>,
}

/// A security config warning. Only with `config:read`.
#[derive(Debug, Serialize, ToSchema)]
pub struct SecurityWarning {
    pub code: String,
    pub path: String,
    pub message: String,
}

/// `GET /api/v1/admin/instance-security`.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstanceSecurity {
    pub window_hours: u32,
    pub computed_at: DateTime<Utc>,
    pub sign_ins: SignInBlock,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bans: Option<BansBlock>,
    pub token_sources: TokenSourcesBlock,
    pub static_tokens: StaticTokensBlock,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exemptions: Option<ExemptionsBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<SecurityWarning>>,
}

fn rejected_weight(e: &AccessEvent) -> u64 {
    1 + u64::from(e.throttled_count.unwrap_or(0))
}

/// The sign-in and token-source lines, from one audit read of the window.
fn audit_blocks(
    events: &Result<Arc<Vec<AccessEvent>>, String>,
    with_audit: bool,
) -> (SignInBlock, TokenSourcesBlock) {
    match events {
        Ok(events) => {
            let mut by_kind: BTreeMap<String, u64> = BTreeMap::new();
            let mut by_ip: HashMap<String, u64> = HashMap::new();
            let mut new_sources = Vec::new();
            for e in events.iter() {
                match e.action {
                    AccessAction::CredentialRejected | AccessAction::SignInFailed => {
                        *by_kind.entry(e.action.as_str().to_owned()).or_default() +=
                            rejected_weight(e);
                        if let Some(ip) = &e.ip_address {
                            *by_ip.entry(ip.clone()).or_default() += rejected_weight(e);
                        }
                    }
                    AccessAction::TokenNewSource => new_sources.push(e),
                    _ => {}
                }
            }
            let rejected: u64 = by_kind.values().sum();
            let partial = events.len() as u64 >= EVENTS_READ;
            let distinct = by_ip.len() as u64;
            let mut addresses: Vec<AddressCount> = by_ip
                .into_iter()
                .map(|(ip, count)| AddressCount { ip, count })
                .collect();
            addresses.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.ip.cmp(&b.ip)));
            addresses.truncate(ADDRESSES_SHOWN);
            (
                SignInBlock {
                    state: LineState::from_attention(rejected > 0),
                    unknown_reason: None,
                    rejected,
                    by_kind,
                    addresses: with_audit.then_some(addresses),
                    distinct_addresses: with_audit.then_some(distinct),
                    partial,
                },
                TokenSourcesBlock {
                    new_source: new_sources.len() as u64,
                    items: with_audit.then(|| {
                        new_sources
                            .iter()
                            .map(|e| TokenSourceItem {
                                user_id: e.user_id.clone(),
                                ip: e.ip_address.clone(),
                                at: e.timestamp,
                            })
                            .collect()
                    }),
                    unknown_reason: None,
                    partial,
                },
            )
        }
        Err(reason) => (
            SignInBlock {
                state: LineState::Unknown,
                unknown_reason: Some(reason.clone()),
                rejected: 0,
                by_kind: BTreeMap::new(),
                addresses: None,
                distinct_addresses: None,
                partial: false,
            },
            TokenSourcesBlock {
                new_source: 0,
                items: None,
                unknown_reason: Some(reason.clone()),
                partial: false,
            },
        ),
    }
}

/// The security of the instance itself (RFC 0050 §4.5).
#[utoipa::path(
    get,
    path = "/api/v1/admin/instance-security",
    tag = "security",
    params(InstanceSecurityQuery),
    responses(
        (status = 200, description = "Sign-ins, bans, token sources, static tokens, exemptions and warnings", body = InstanceSecurity),
        (status = 403, description = "`system:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/instance-security")]
#[allow(clippy::too_many_arguments)]
pub async fn instance_security(
    query: web::Query<InstanceSecurityQuery>,
    identity: AuthIdentity,
    hot: web::Data<HotConfigLock>,
    admin: Option<web::Data<Arc<AdminService>>>,
    ip_blocks: Option<web::Data<Arc<dyn IpBlockStore>>>,
    reload: Option<web::Data<Arc<ConfigReloadService>>>,
    cache: Option<web::Data<SourceCache>>,
) -> Result<HttpResponse, AppError> {
    let id = &identity.0;
    if !holds_at(&hot, id, None, Action::SystemRead).await {
        return Err(AppError::forbidden(
            "this endpoint requires the 'system:read' permission",
        ));
    }
    let with_audit = holds_at(&hot, id, None, Action::AuditRead).await;
    let (window_hours, key): (u32, &'static str) = match query.window.as_deref() {
        Some("7d") => (168, "auth_events_7d"),
        _ => (24, "auth_events_24h"),
    };
    let cache = cache_of(cache);
    let now = Utc::now();

    // Failed sign-ins and new token sources: one audit read per window.
    let events = match admin {
        Some(admin) => {
            let admin = Arc::clone(admin.get_ref());
            cache
                .get(key, SOURCE_TTL, async move {
                    admin
                        .list_events(EventFilter {
                            actions: vec![
                                AccessAction::CredentialRejected,
                                AccessAction::SignInFailed,
                                AccessAction::TokenNewSource,
                            ],
                            from: Some(Utc::now() - Duration::hours(i64::from(window_hours))),
                            limit: EVENTS_READ,
                            ..Default::default()
                        })
                        .await
                })
                .await
        }
        None => Err("no audit log in this process".to_owned()),
    };
    let (sign_ins, token_sources) = audit_blocks(&events, with_audit);

    let bans = match (
        ip_blocks,
        holds_at(&hot, id, None, Action::BlocksRead).await,
    ) {
        (Some(store), true) => {
            let unix_now = u64::try_from(now.timestamp()).unwrap_or(0);
            // The same 2 s budget as every other source of the page.
            let read = tokio::time::timeout(SOURCE_TIMEOUT, store.list_blocked()).await;
            Some(
                match read.unwrap_or_else(|_| {
                    Err(batlehub_core::error::CoreError::Cache(
                        "the ban store did not answer within 2 s".into(),
                    ))
                }) {
                    Ok(blocked) => {
                        let active: Vec<_> =
                            blocked.iter().filter(|b| b.unblock_at > unix_now).collect();
                        BansBlock {
                            active: active.len() as u64,
                            automatic: active.iter().filter(|b| b.reason == "auto").count() as u64,
                            unknown_reason: None,
                        }
                    }
                    Err(e) => BansBlock {
                        active: 0,
                        automatic: 0,
                        unknown_reason: Some(e.to_string()),
                    },
                },
            )
        }
        _ => None,
    };

    let plain = Arc::clone(&hot.read().await.plain_static_tokens);
    let static_tokens = StaticTokensBlock {
        plain: plain.len() as u64,
        paths: plain.iter().map(|(p, _)| p.clone()).collect(),
        user_ids: with_audit.then(|| plain.iter().filter_map(|(_, u)| u.clone()).collect()),
    };

    let exemptions = if holds_at(&hot, id, None, Action::QuarantineRead).await {
        match exemptions_source(&cache, &hot).await {
            Err(reason) => Some(ExemptionsBlock {
                expiring_7d: 0,
                self_approved: 0,
                items: Vec::new(),
                unknown_reason: Some(reason),
            }),
            Ok(rows) => {
                let coords: Vec<PackageId> = rows
                    .iter()
                    .map(|e| PackageId::new(&e.registry, &e.package, &e.version))
                    .collect();
                let held = coords_held(&hot, id, &coords, Action::QuarantineRead).await;
                let soon = now + Duration::days(7);
                let mut self_approved = 0;
                let mut items = Vec::new();
                for (e, _) in rows
                    .iter()
                    .zip(held)
                    .filter(|(e, h)| *h && e.exempt_until > now)
                {
                    self_approved += u64::from(e.self_approved);
                    if e.exempt_until <= soon {
                        items.push(ExemptionItem {
                            registry: e.registry.clone(),
                            name: e.package.clone(),
                            version: e.version.clone(),
                            expires_at: e.exempt_until,
                        });
                    }
                }
                items.sort_by_key(|i| i.expires_at);
                Some(ExemptionsBlock {
                    expiring_7d: items.len() as u64,
                    self_approved,
                    items,
                    unknown_reason: None,
                })
            }
        }
    } else {
        None
    };

    let warnings = match (reload, holds_at(&hot, id, None, Action::ConfigRead).await) {
        (Some(reload), true) => Some(
            reload
                .warnings()
                .into_iter()
                .filter(|w| SECURITY_WARNING_CODES.contains(&w.code.as_str()))
                .map(|w| SecurityWarning {
                    code: w.code,
                    path: w.path,
                    message: w.message,
                })
                .collect(),
        ),
        _ => None,
    };

    Ok(HttpResponse::Ok().json(InstanceSecurity {
        window_hours,
        computed_at: now,
        sign_ins,
        bans,
        token_sources,
        static_tokens,
        exemptions,
        warnings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_core::entities::{AccessResult, CallerNet, Role};

    fn event(action: AccessAction) -> AccessEvent {
        AccessEvent::about_identity(
            action,
            None,
            Role::Anonymous,
            AccessResult::Allowed,
            CallerNet::default(),
            None,
        )
    }

    /// An audit log that did not answer is *unknown*, not zero new sources.
    #[test]
    fn an_audit_read_failure_leaves_token_sources_unknown() {
        let (_, sources) = audit_blocks(&Err("down".to_owned()), true);
        assert_eq!(sources.unknown_reason.as_deref(), Some("down"));
    }

    /// A rejection burst that fills the shared read leaves `new_source` a
    /// floor: it is marked, as the sign-in count is.
    #[test]
    fn a_full_read_marks_token_sources_partial() {
        let mut events = vec![event(AccessAction::TokenNewSource)];
        events.resize_with(EVENTS_READ as usize, || {
            event(AccessAction::CredentialRejected)
        });
        let (sign_ins, sources) = audit_blocks(&Ok(Arc::new(events)), false);
        assert!(sign_ins.partial && sources.partial);
        assert_eq!(sources.new_source, 1);
        let (_, sources) = audit_blocks(
            &Ok(Arc::new(vec![event(AccessAction::TokenNewSource)])),
            false,
        );
        assert!(!sources.partial && sources.unknown_reason.is_none());
    }
}
