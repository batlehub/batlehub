use actix_web::{get, web, Responder};
use serde::Serialize;
use utoipa::ToSchema;

use crate::{extractors::AuthIdentity, RegistryHostMap, RegistryMap, RegistryModeMap};

#[derive(Serialize, ToSchema)]
pub struct RegistryInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub registry_type: String,
    pub mode: String,
    /// The registry's own hostname-rooted URL (`https://npm.acme.io`) when one is
    /// configured, otherwise `null` — clients then fall back to
    /// `{origin}/proxy/{name}`.
    ///
    /// Shown to anonymous callers too. The endpoint already filters by
    /// `accessible_registries_for`, so a caller only ever sees registries they
    /// can already reach; withholding the host from them would break the Setup
    /// Guide while hiding a name that is in public DNS anyway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    /// The upstream this registry proxies (`https://registry.npmjs.org`), or
    /// `null` for a local-only registry that has none.
    ///
    /// The design proof's catalog caption names it — *"hybrid ·
    /// registry.npmjs.org · 1,284 packages · 6.1 GB cached"* — because "which
    /// upstream is behind this" is the first thing an operator asks of a
    /// registry they did not configure themselves.
    ///
    /// Visible to anonymous callers on the same terms as `public_url` above,
    /// and for the same reason: this endpoint already filters by
    /// `accessible_registries_for`, so a caller only ever sees registries they
    /// can already pull through — and pulling through one reveals the upstream
    /// anyway, in the artifacts it returns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// Whether this registry mints signed download URLs (RFC 0012), and can
    /// therefore be closed to anonymous callers even though the client fetches
    /// artifacts without credentials.
    ///
    /// Here so the Setup Guide can stop telling a Terraform user to open their
    /// registry when it does not need opening. Not a secret: it changes the
    /// advice on a page the caller can already read, and a caller who can list
    /// a registry can discover the same fact by fetching from it.
    pub signed_downloads: bool,
    /// Whether the caller holds, at registry level, every permission a package
    /// manager needs to resolve and download from this registry (version
    /// listing, metadata, and the artifact — `source:read` where the artifact
    /// is a source archive), decided by the grants that enforce reads.
    /// `batlehub-cli setup apply` writes a client config only where this is
    /// `true` (RFC 0038 §4.4).
    ///
    /// A registry-level ceiling: a sealing namespace or a private package can
    /// still refuse one package, and a package-level grant can open one package
    /// on a registry answered `false`. Computed by
    /// `batlehub_core::services::authz::installable_registries` from
    /// `RegistryKind::install_actions`.
    pub installable: bool,
}

/// List configured registries visible to the current user.
#[utoipa::path(
    get,
    path = "/api/v1/registries",
    tag = "front-office",
    responses(
        (status = 200, description = "List of accessible registries", body = Vec<RegistryInfo>),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/registries")]
pub async fn list_registries(
    map: web::Data<RegistryMap>,
    modes: web::Data<RegistryModeMap>,
    hosts: Option<web::Data<RegistryHostMap>>,
    upstreams: Option<web::Data<crate::UpstreamMap>>,
    access: web::Data<crate::AccessConfigLock>,
    svc: Option<web::Data<std::sync::Arc<batlehub_core::services::ProxyService>>>,
    identity: AuthIdentity,
) -> impl Responder {
    let accessible = access.read().await.accessible_registries_for(&identity);
    // Read from the hot config rather than a parallel map, so a reload that
    // turns signing on — or changes a grant — cannot leave this endpoint
    // answering with the old value.
    let (signed, granted) = match svc.as_ref() {
        Some(svc) => (
            svc.hot.read().await.signed_downloads.clone(),
            Some(batlehub_core::services::authz::installable_registries(&svc.hot, &identity).await),
        ),
        None => (Default::default(), None),
    };
    let installable = |name: &str| match &granted {
        // No hierarchy for this registry: `authorize_grants` lets every read
        // through, so it is installable for whoever may see it.
        Some(granted) => granted.get(name).copied().unwrap_or(true),
        // No proxy service wired, so no grants to ask: the legacy answer.
        None => accessible.contains(name),
    };
    let mut registries: Vec<RegistryInfo> = map
        .entries()
        .into_iter()
        // The legacy filter, unchanged for the console, plus every registry the
        // grants let the caller install from: one opened only through
        // `[registries.grants]` is invisible to `accessible_registries_for`.
        // Only an actual grant widens the list — never the absence of one.
        .filter(|(name, _)| {
            accessible.contains(name.as_str())
                || granted.as_ref().is_some_and(|g| g.get(name) == Some(&true))
        })
        .map(|(name, registry_type)| RegistryInfo {
            mode: modes.get(&name).as_str().to_owned(),
            public_url: hosts.as_ref().and_then(|h| h.public_url_for(&name)),
            upstream: upstreams.as_ref().and_then(|u| u.upstream_for(&name)),
            signed_downloads: signed.get(&name).copied().unwrap_or(false),
            installable: installable(&name),
            name,
            registry_type,
        })
        .collect();
    registries.sort_by(|a, b| a.name.cmp(&b.name));
    web::Json(registries)
}
