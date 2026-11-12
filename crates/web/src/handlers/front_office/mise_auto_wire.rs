//! `GET /api/v1/mise/auto-wire` — the one mise rule, computed on the server so
//! the CLI's block and the rewrite cannot disagree (RFC 0045 §4.4).

use std::collections::BTreeSet;

use actix_web::{get, web, HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use batlehub_core::services::mise_wire::regex_escape;

use crate::error::AppError;
use crate::extractors::AuthIdentity;
use crate::handlers::proxy::common::registry_public_base;
use crate::RegistryHostMap;

/// The client side of the wire, filtered to what the caller may see.
#[derive(Debug, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct MiseAutoWire {
    /// `url_replacements` key, `regex:` prefixed.
    pub pattern: String,
    /// `url_replacements` value.
    pub replacement: String,
    /// The wired hosts in `pattern`, in table order.
    pub hosts: Vec<String>,
    /// Wired hosts left out because the caller cannot read their registries.
    pub hidden_hosts: usize,
    /// The air-gap catch-all key (every host to the wire).
    pub air_gap_pattern: String,
    /// `true` when the overlay is on: print `registry_floating = true`.
    pub registry_floating: bool,
    /// Overlay tools the caller may see (§15 Q16).
    pub overlay_tools: Vec<MiseOverlayToolInfo>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct MiseOverlayToolInfo {
    pub name: String,
    pub aliases: Vec<String>,
}

/// The mise rule of this instance's wire.
#[utoipa::path(
    get,
    path = "/api/v1/mise/auto-wire",
    tag = "front-office",
    responses(
        (status = 200, description = "The url_replacements rule, filtered to the caller's registries", body = MiseAutoWire),
        (status = 404, description = "No mise-versions registry has auto_wire enabled"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/mise/auto-wire")]
pub async fn mise_auto_wire(
    req: HttpRequest,
    hosts: Option<web::Data<RegistryHostMap>>,
    access: web::Data<crate::AccessConfigLock>,
    identity: AuthIdentity,
) -> Result<HttpResponse, AppError> {
    let Some(wire) = hosts.and_then(|h| h.wire()) else {
        return Err(AppError::not_found(
            "this instance has no mise-versions registry with auto_wire enabled",
        )
        .coded("no_wire"));
    };
    let accessible = access.read().await.accessible_registries_for(&identity);
    let mut all: Vec<&str> = Vec::new();
    let mut shown: BTreeSet<&str> = BTreeSet::new();
    for (host, _, registry) in wire.table.wired() {
        // The archive prefix is printed only with the overlay on: a client that
        // does not float its registry never asks for it (§4.5).
        if host == "mise.jdx.dev" && !wire.auto_wire.overlay {
            continue;
        }
        if !all.contains(&host) {
            all.push(host);
        }
        if accessible.contains(registry) {
            shown.insert(host);
        }
    }
    let hosts: Vec<String> = all
        .iter()
        .filter(|h| shown.contains(*h))
        .map(|h| (*h).to_owned())
        .collect();
    let alternation = hosts
        .iter()
        .map(|h| regex_escape(h))
        .collect::<Vec<_>>()
        .join("|");
    let base = registry_public_base(&req, &wire.registry);
    let tools_shown = |source: Option<&str>| match source {
        Some(src) => accessible.contains(src),
        None => accessible.contains(wire.registry.as_str()),
    };
    let overlay_tools = if wire.auto_wire.overlay {
        wire.auto_wire
            .tools
            .iter()
            .filter(|t| tools_shown(t.source.as_ref().map(|s| s.registry.as_str())))
            .map(|t| MiseOverlayToolInfo {
                name: t.name.clone(),
                aliases: t.aliases.clone(),
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(HttpResponse::Ok().json(MiseAutoWire {
        pattern: format!("regex:^https?://({alternation})/(.*)"),
        replacement: format!("{base}/mise-versions/_wire/$1/$2"),
        hidden_hosts: all.len() - hosts.len(),
        hosts,
        air_gap_pattern: "regex:^https?://([^/]+)/(.*)".to_owned(),
        registry_floating: wire.auto_wire.overlay,
        overlay_tools,
    }))
}
