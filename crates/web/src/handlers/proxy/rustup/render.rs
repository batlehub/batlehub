//! Composing the manifest this instance serves (RFC 0024 §4.4, §6.5).
//!
//! One function, because three routes must agree byte for byte about what the
//! answer is: the manifest itself, the `.sha256` sidecar computed over it, and
//! the `.asc` that is relayed beside it. The sidecar route does not fetch
//! upstream's sidecar — it renders the manifest exactly as the manifest route
//! would and hashes the result, which is the only sidecar that lets rustup
//! proceed when the document was edited at all.

use std::sync::Arc;

use actix_web::web;
use batlehub_core::{
    entities::{Action, PackageId, RegistryKind},
    ports::DocumentKind,
    services::rustup::{
        listing_package, render_manifest, Manifest, ManifestsTxt, ToolchainName, BACKTRACK_DAYS,
        RUST_PACKAGE,
    },
    services::ProxyService,
};

use crate::handlers::proxy::common::fetch_proxy_document;
use crate::{error::AppError, extractors::AuthIdentity};

/// What `X-BatleHub-Manifest` says about the body.
pub(super) const UPSTREAM: &str = "upstream";
pub(super) const FILTERED: &str = "filtered";
pub(super) const REPAIRED: &str = "repaired";

/// The tree every unedited manifest's URLs name, and the prefix rustup rewrites
/// to `RUSTUP_DIST_SERVER` on the client side.
const CANONICAL_BASE: &str = "https://static.rust-lang.org";

/// Everything one manifest render needs, as a struct rather than eight
/// positional parameters: `registry`, `public_base` and `upstream_base` are all
/// `&str`, and transposing two of them would produce a document that looked
/// plausible and named the wrong tree.
pub(super) struct RenderRequest<'a> {
    pub registry: &'a str,
    pub name: &'a ToolchainName,
    /// The dated directory, when the request carried one.
    pub date: Option<&'a str>,
    pub identity: &'a AuthIdentity,
    pub public_base: &'a str,
    /// This registry's configured upstream, when it is not the default tree.
    pub upstream_base: Option<&'a str>,
    pub deny: &'a [String],
}

pub(super) struct Rendered {
    pub body: String,
    pub state: &'static str,
    /// The release actually served, when it is not the one the name asked for.
    pub served_version: Option<String>,
}

/// The channel string a manifest document is addressed by: `stable`, or
/// `2026-09-05/nightly` for a dated one.
fn channel_string(name: &ToolchainName, date: Option<&str>) -> String {
    match date {
        Some(d) => format!("{d}/{}", name.as_name()),
        None => name.as_name(),
    }
}

/// Fetch the manifest for one channel and edit it as policy requires.
///
/// The order is the RFC's: **is this document's own release blocked** decides
/// between a `404`, a repair, and serving it; only then is the deny list
/// applied. A blocked release is never served with its components edited — it
/// is not served at all.
pub(super) async fn render(
    svc: &web::Data<Arc<ProxyService>>,
    req: &RenderRequest<'_>,
) -> Result<Rendered, AppError> {
    let RenderRequest {
        registry,
        name,
        date,
        identity,
        public_base,
        upstream_base,
        deny,
    } = *req;
    let channel = channel_string(name, date);
    let body = fetch_manifest(svc, registry, &channel, identity, public_base).await?;

    let blocked = svc
        .blocked_versions_for(registry, RUST_PACKAGE, RegistryKind::Rustup)
        .await;
    let coordinate = Manifest::parse(&body).coordinate();

    // A dated request is exact whatever the name says, because the directory
    // pins the release.
    let exact = name.is_exact() || date.is_some();
    let is_blocked = coordinate.as_deref().is_some_and(|c| blocked.contains(c));

    if !is_blocked {
        return Ok(finish(body, upstream_base, deny, None));
    }
    if exact {
        // rustup's own "could not download nonexistent rust version", the same
        // message an unpublished version gets.
        return Err(AppError::not_found(format!(
            "Rust release '{}' is not available from this registry",
            coordinate.unwrap_or_else(|| name.as_name())
        )));
    }

    // An alias moves: serve the newest release it could denote instead.
    let manifests_body = fetch_manifests_txt(svc, registry, identity, public_base).await?;
    let manifests = ManifestsTxt::parse(&manifests_body);
    let Some(row) = manifests.newest_allowed(name, &|v| blocked.contains(v), BACKTRACK_DAYS) else {
        return Err(AppError::not_found(format!(
            "no release '{}' could denote is available from this registry",
            name.as_name()
        )));
    };

    let repaired_channel = format!("{}/{}", row.date, row.name);
    let repaired = fetch_manifest(svc, registry, &repaired_channel, identity, public_base).await?;
    let served = Manifest::parse(&repaired).coordinate().or(row.coordinate);
    Ok(finish(repaired, upstream_base, deny, served))
}

/// The last two edits, and the header that says whether either happened.
fn finish(
    body: String,
    upstream_base: Option<&str>,
    deny: &[String],
    served_version: Option<String>,
) -> Rendered {
    // A mirror's manifest names the mirror in every component URL, and rustup
    // only rewrites the canonical host — so an unnormalised document sends
    // every tarball straight to the mirror, with this instance having mediated
    // the policy and none of the bytes (§4.4).
    let normalised = match upstream_base {
        Some(base) if base != CANONICAL_BASE && body.contains(base) => {
            Some(body.replace(base, CANONICAL_BASE))
        }
        _ => None,
    };
    let edited_urls = normalised.is_some();
    let body = normalised.unwrap_or(body);

    let rendered = render_manifest(&body, deny);
    let edited_components = matches!(rendered, std::borrow::Cow::Owned(_));
    let body = rendered.into_owned();

    let state = if served_version.is_some() {
        REPAIRED
    } else if edited_urls || edited_components {
        FILTERED
    } else {
        UPSTREAM
    };
    Rendered {
        body,
        state,
        served_version,
    }
}

async fn fetch_manifest(
    svc: &web::Data<Arc<ProxyService>>,
    registry: &str,
    channel: &str,
    identity: &AuthIdentity,
    public_base: &str,
) -> Result<String, AppError> {
    let doc = fetch_proxy_document(
        svc.clone(),
        PackageId::new(registry, listing_package(channel), "manifest"),
        identity.clone(),
        Action::ReleasesList,
        DocumentKind::MANIFEST,
        public_base.to_owned(),
    )
    .await?;
    doc.body
        .as_text()
        .map(str::to_owned)
        .ok_or_else(|| AppError::not_found(format!("channel '{channel}' has no manifest")))
}

async fn fetch_manifests_txt(
    svc: &web::Data<Arc<ProxyService>>,
    registry: &str,
    identity: &AuthIdentity,
    public_base: &str,
) -> Result<String, AppError> {
    let doc = fetch_proxy_document(
        svc.clone(),
        PackageId::new(registry, RUST_PACKAGE, "manifests"),
        identity.clone(),
        Action::ReleasesList,
        DocumentKind::Versions,
        public_base.to_owned(),
    )
    .await?;
    doc.body.as_text().map(str::to_owned).ok_or_else(|| {
        AppError::not_found("manifests.txt is unavailable, so an alias cannot be repaired")
    })
}
