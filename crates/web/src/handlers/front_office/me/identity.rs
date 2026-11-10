use actix_web::{get, web, Responder};
use serde::Serialize;
use utoipa::ToSchema;

use batlehub_core::entities::Role;

use crate::extractors::AuthIdentity;

#[derive(Serialize, ToSchema)]
pub struct MeResponse {
    pub user_id: Option<String>,
    pub role: Role,
    pub auth_provider: Option<String>,
    /// Whether the current user has access to at least one configured registry.
    pub has_registry_access: bool,
    /// Dynamic groups the user belongs to (populated by OIDC providers).
    pub groups: Vec<String>,
    /// The verbs this caller holds at the instance tier — what a control
    /// endpoint naming no registry checks (RFC 0042 §4.3). Spelled as in
    /// `[grants]`.
    pub verbs: Vec<String>,
    /// Per registry with a grant hierarchy, the verbs held there (instance
    /// and registry tiers). A registry absent here is "not shown to hold
    /// anything", never "holds everything". Package- and version-tier grants
    /// are not reflected: the server still decides per package.
    pub registries: std::collections::BTreeMap<String, Vec<String>>,
}

/// Return the current caller's identity and role.
#[utoipa::path(
    get,
    path = "/api/v1/me",
    tag = "front-office",
    responses(
        (status = 200, description = "Current identity", body = MeResponse),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/me")]
pub async fn me(
    identity: AuthIdentity,
    access: web::Data<crate::AccessConfigLock>,
    svc: Option<web::Data<std::sync::Arc<batlehub_core::services::ProxyService>>>,
) -> impl Responder {
    let spell = |set: std::collections::BTreeSet<batlehub_core::entities::Action>| {
        set.into_iter().map(|a| a.as_str().to_owned()).collect()
    };
    let (verbs, registries) = match svc {
        Some(svc) => {
            let held = batlehub_core::services::authz::effective_verbs(&svc.hot, &identity).await;
            let registries = held
                .registries
                .into_iter()
                .map(|(name, set)| (name, spell(set)))
                .collect();
            (spell(held.instance), registries)
        }
        None => Default::default(),
    };
    web::Json(MeResponse {
        user_id: identity.user_id.clone(),
        role: identity.role.clone(),
        auth_provider: identity.auth_provider.clone(),
        has_registry_access: access.read().await.has_registry_access(&identity),
        groups: identity.groups.clone(),
        verbs,
        registries,
    })
}
