use actix_web::{get, web, Responder};
use serde::Serialize;
use utoipa::ToSchema;

use batlehub_core::services::authz::RegistryAccess;

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
    /// Shown only to a caller holding `upstreams:read` on the registry, which
    /// translation gives the administrator and anyone else gets by a grant: an
    /// upstream can name an internal host, and being able to install from a
    /// mirror is not being told what it fronts. Credentials, a query and a
    /// fragment are stripped whoever holds it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// Every upstream a path-addressed registry (`generic`, `deb`, …) mirrors,
    /// path for path; omitted for the other kinds. `batlehub-cli setup apply`
    /// turns each into a mise rule `^{upstream}/(.+)` → `{proxy}/$1`
    /// (RFC 0038 §6.4). Shown on the same terms as `upstream`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub upstreams: Vec<String>,
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
    // The first read guard is dropped at the end of its own statement, before
    // `installable_registries` takes another: holding one across that await
    // lets a reload's queued `write()` deadlock both on tokio's fair RwLock.
    let (signed, granted) = match svc.as_ref() {
        Some(svc) => {
            let signed = svc.hot.read().await.signed_downloads.clone();
            let granted =
                batlehub_core::services::authz::installable_registries(&svc.hot, &identity).await;
            (signed, Some(granted))
        }
        None => (Default::default(), None),
    };
    // No hierarchy for a registry: `authorize_grants` lets every read through,
    // so it is installable, and its upstreams readable, for whoever may see it.
    // No proxy service wired, so no grants to ask: the legacy answer.
    let access = |name: &str| match &granted {
        Some(granted) => granted.get(name).copied().unwrap_or(RegistryAccess {
            installable: true,
            upstreams: true,
        }),
        None => RegistryAccess {
            installable: accessible.contains(name),
            upstreams: true,
        },
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
                || granted
                    .as_ref()
                    .is_some_and(|g| g.get(name).is_some_and(|a| a.installable))
        })
        .map(|(name, registry_type)| {
            let access = access(&name);
            let shown = upstreams.as_ref().filter(|_| access.upstreams);
            RegistryInfo {
                mode: modes.get(&name).as_str().to_owned(),
                public_url: hosts.as_ref().and_then(|h| h.public_url_for(&name)),
                upstream: shown
                    .and_then(|u| u.upstream_for(&name))
                    .map(|u| public_upstream(&u)),
                upstreams: shown
                    .map(|u| u.upstreams_for(&name))
                    .unwrap_or_default()
                    .iter()
                    .map(|u| public_upstream(u))
                    .collect(),
                signed_downloads: signed.get(&name).copied().unwrap_or(false),
                installable: access.installable,
                name,
                registry_type,
            }
        })
        .collect();
    registries.sort_by(|a, b| a.name.cmp(&b.name));
    web::Json(registries)
}

/// An upstream URL as the registry list may show it: no userinfo, no query and
/// no fragment, any of which can carry a credential the operator configured.
fn public_upstream(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or_default();
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{scheme}://{host}{path}")
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use actix_web::{test, web, App};

    use super::*;
    use crate::{new_access_lock, AccessConfig, UpstreamMap};

    /// A path-addressed registry lists every upstream it mirrors; the others
    /// carry no `upstreams` key at all.
    #[actix_web::test]
    async fn a_generic_registry_lists_every_upstream() {
        let open: HashSet<String> = ["g", "n"].into_iter().map(str::to_owned).collect();
        let access = new_access_lock(AccessConfig {
            anonymous: open.clone(),
            user: open.clone(),
            admin: open,
            groups: HashMap::new(),
            explore_anonymous: HashSet::new(),
            explore_user: HashSet::new(),
            explore_admin: HashSet::new(),
        });
        let types = RegistryMap::new(HashMap::from([
            ("g".to_owned(), "generic".to_owned()),
            ("n".to_owned(), "npm".to_owned()),
        ]));
        let upstreams = UpstreamMap::new(HashMap::from([(
            "n".to_owned(),
            "https://registry.npmjs.org".to_owned(),
        )]))
        .with_all(HashMap::from([(
            "g".to_owned(),
            vec![
                "https://releases.hashicorp.com".to_owned(),
                "https://dl.k8s.io".to_owned(),
            ],
        )]));
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(types))
                .app_data(web::Data::new(RegistryModeMap::default()))
                .app_data(web::Data::new(upstreams))
                .app_data(web::Data::new(access))
                .service(list_registries),
        )
        .await;
        let body: Vec<serde_json::Value> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/registries")
                .to_request(),
        )
        .await;
        assert_eq!(
            body[0]["upstreams"],
            serde_json::json!(["https://releases.hashicorp.com", "https://dl.k8s.io"])
        );
        assert!(body[1].get("upstreams").is_none(), "{}", body[1]);
    }

    #[::core::prelude::v1::test]
    fn an_upstream_is_shown_without_credentials() {
        assert_eq!(
            public_upstream("https://user:tok@artifactory.corp/generic?token=x#f"),
            "https://artifactory.corp/generic"
        );
        assert_eq!(
            public_upstream("https://a@b@host:8443"),
            "https://host:8443"
        );
        assert_eq!(
            public_upstream("https://dl.k8s.io/release/a@b"),
            "https://dl.k8s.io/release/a@b"
        );
    }
}
