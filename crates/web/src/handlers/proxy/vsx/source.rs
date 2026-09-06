//! Where gallery entries come from — and the one place blocked versions are
//! removed.
//!
//! Both client protocols and every document they render resolve through the two
//! functions here, so a version an operator blocked cannot reach a client by
//! any of them. That is the same arrangement `jetbrains_marketplace/ide.rs`
//! uses for its three plugin documents, and the reason is the same: filtering
//! per rendered document is one chance per document to forget.

use std::sync::Arc;

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{PackageId, RegistryKind},
    error::CoreError,
    services::{LocalRegistryService, ProxyRequest, ProxyService},
};

use super::protocol::GalleryQuery;
use super::render::GalleryEntry;
use crate::{error::AppError, extractors::AuthIdentity};
use batlehub_core::entities::Action;

/// Everything one extension's documents are rendered from.
///
/// Local/hybrid reads the local registry; proxy (and a hybrid miss) resolves
/// upstream metadata. Blocked versions are removed either way — on the local
/// path by `load_visible_versions`'s `filter_blocked`, on the proxy path here.
///
/// `Ok(None)` when the extension is not known, which every caller turns into a
/// `404` or an empty result set as its protocol requires.
pub async fn extension_entry(
    svc: &Arc<ProxyService>,
    local_svc: &Arc<LocalRegistryService>,
    mode: RegistryMode,
    registry: &str,
    kind: RegistryKind,
    extension_id: &str,
    identity: &AuthIdentity,
) -> Result<Option<GalleryEntry>, AppError> {
    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        // The registry rule chain, which a local read would otherwise skip —
        // `get_openvsx_versions` checks per-package visibility, not
        // `[registries.rbac]`. Without this the gallery would list a private
        // registry's extensions to anyone who could reach the port, while the
        // download of the same extension was refused: a metadata leak dressed
        // up as a working catalogue.
        //
        // `source:read`, matching the download route, so "can browse" and "can
        // install" are one decision. An editor sends no credentials at all (see
        // the module header), so a gallery registry grants this to `anonymous`
        // or is unusable — better that be one grant than two.
        authorize_gallery_read(svc, registry, extension_id, identity).await?;
        match local_svc
            .get_openvsx_versions(registry, extension_id, &identity.0)
            .await
        {
            // The local path is already filtered: `get_openvsx_versions` goes
            // through `load_visible_versions_or_not_found`, which runs
            // `filter_unlisted → filter_blocked → filter_for_identity`.
            Ok(versions) => {
                let mut entry = GalleryEntry::from_local(&versions);
                if let Some(e) = entry.as_mut() {
                    apply_registry_key(local_svc, registry, e).await;
                }
                return Ok(entry);
            }
            Err(CoreError::NotFound(_)) if mode == RegistryMode::Hybrid => {}
            Err(CoreError::NotFound(_)) => return Ok(None),
            Err(e) => return Err(AppError::from(e)),
        }
    }
    if mode == RegistryMode::Local {
        return Ok(None);
    }

    let req = ProxyRequest {
        package_id: PackageId::new(registry, extension_id, "latest"),
        identity: identity.0.clone(),
        action: Action::SourceRead.to_owned(),
        ip_address: None,
        user_agent: None,
    };
    let meta = match svc.resolve_metadata_for(&req).await {
        Ok(m) => m,
        Err(CoreError::NotFound(_)) => return Ok(None),
        Err(e) => return Err(AppError::from(e)),
    };

    let Some(mut entry) = entry_from_metadata(extension_id, &meta) else {
        return Ok(None);
    };
    filter_blocked(svc, registry, kind, &mut entry).await;
    Ok((!entry.versions.is_empty()).then_some(entry))
}

/// The entries a search should return, and the total *before* paging.
///
/// The total is what the editor pages against, so it counts what survived
/// filtering and not what the page happens to hold — get that wrong and "load
/// more" either stops a page early or never stops.
pub async fn search_entries(
    svc: &Arc<ProxyService>,
    local_svc: &Arc<LocalRegistryService>,
    mode: RegistryMode,
    registry: &str,
    kind: RegistryKind,
    query: &GalleryQuery,
    identity: &AuthIdentity,
) -> Result<(Vec<GalleryEntry>, usize), AppError> {
    // A query this registry cannot honestly answer returns nothing rather than
    // the whole catalogue — see `GalleryQuery::is_unanswerable`.
    if query.is_unanswerable() {
        return Ok((Vec::new(), 0));
    }

    // An exact *name* lookup is the hot path: install and update both take it,
    // and it resolves one extension rather than scanning the registry.
    //
    // A lookup by `extensionId` (filterType 4) cannot take this path — the id is
    // a uuid derived from the name, so there is nothing to resolve it back
    // through. Those fall through to the scan below, where `GalleryEntry::
    // matches` compares the id properly. Slower, and correct; taking the fast
    // path on an empty `extension_names` would answer every id lookup with
    // nothing.
    if !query.extension_names.is_empty() {
        let mut found = Vec::new();
        for name in &query.extension_names {
            if let Some(e) =
                extension_entry(svc, local_svc, mode.clone(), registry, kind, name, identity)
                    .await?
            {
                found.push(e);
            }
        }
        let total = found.len();
        return Ok((paginate(found, query), total));
    }

    let mut entries = Vec::new();
    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        authorize_gallery_read(svc, registry, "__search__", identity).await?;
        let local = local_svc
            .get_openvsx_extensions(
                registry,
                query.search_text.as_deref().unwrap_or(""),
                &identity.0,
            )
            .await
            .map_err(AppError::from)?;
        // One group per extension, carrying every visible version — an entry
        // built from the newest version alone would answer a uuid lookup with a
        // one-version history, and the editor needs the whole list to fall back
        // from a pre-release to the last release.
        let key_id = registry_key_id(local_svc, registry).await;
        for versions in local {
            if let Some(mut e) = GalleryEntry::from_local(&versions) {
                if let Some(id) = &key_id {
                    e.sign_locally(id);
                }
                entries.push(e);
            }
        }
    }

    // Upstream search is deliberately not implemented for the free-text case
    // yet: forwarding an `extensionquery` verbatim and re-rendering it is the
    // right design (it keeps upstream's relevance ranking) but it is a distinct
    // piece of work. Until it lands, a proxy-mode registry answers free-text
    // search from nothing and exact lookups from upstream — which is what makes
    // install-by-id and update work, and leaves discovery to the upstream's own
    // site. Stated here rather than silently returning empty.
    if mode == RegistryMode::Proxy {
        tracing::debug!(
            registry,
            "free-text extension search is not forwarded upstream yet; \
             exact lookups resolve normally"
        );
    }

    entries.retain(|e| e.matches(query));
    sort_entries(&mut entries, query);
    let total = entries.len();
    Ok((paginate(entries, query), total))
}

/// Run the registry's rule chain before a local read.
///
/// The proxy path gets this from `resolve_metadata_for`; the local path has to
/// ask for it, exactly as `serve_local_or_proxy_artifact` does.
async fn authorize_gallery_read(
    svc: &Arc<ProxyService>,
    registry: &str,
    coordinate: &str,
    identity: &AuthIdentity,
) -> Result<(), AppError> {
    svc.authorize_read(
        &PackageId::new(registry, coordinate, "latest"),
        &identity.0,
        Action::SourceRead,
    )
    .await
    .map_err(AppError::from)
}

/// Order results as the editor asked (RFC 0009 §7.6).
///
/// `sortBy` was parsed and then ignored, so every query came back in qualified-name
/// order whatever the editor requested — which for "sort by recently updated"
/// is not a slower answer but a wrong one.
///
/// Relevance keeps the qualified-name order: this proxy has no relevance signal
/// of its own, and a stable, predictable order is a better answer than a
/// fabricated ranking.
fn sort_entries(entries: &mut [GalleryEntry], query: &GalleryQuery) {
    use super::protocol::sort_by;

    /// Newest `lastUpdated` across an entry's versions, or empty when it has
    /// none — an entry with no versions sorts last rather than crashing.
    fn newest(e: &GalleryEntry) -> &str {
        e.versions
            .iter()
            .map(|v| v.last_updated.as_str())
            .max()
            .unwrap_or("")
    }
    /// What the editor shows, which is what "sort by title" means to a user.
    fn title(e: &GalleryEntry) -> &str {
        e.display_name.as_deref().unwrap_or(&e.extension_name)
    }

    match query.sort_by {
        sort_by::TITLE => entries.sort_by_key(|e| title(e).to_lowercase()),
        sort_by::PUBLISHER => entries.sort_by(|a, b| {
            a.publisher
                .to_lowercase()
                .cmp(&b.publisher.to_lowercase())
                .then_with(|| a.qualified_name().cmp(&b.qualified_name()))
        }),
        // RFC 3339 sorts correctly as text, which is why `last_updated` is
        // stored that way rather than parsed here.
        sort_by::LAST_UPDATED => entries.sort_by(|a, b| newest(b).cmp(newest(a))),
        _ => entries.sort_by_key(|e| e.qualified_name()),
    }

    // `sortOrder` 2 is an explicit descending request; 1 and 0 leave the
    // per-field default above, which is already descending for recency.
    if query.sort_order == 2 && query.sort_by != sort_by::LAST_UPDATED {
        entries.reverse();
    }
}

fn paginate(entries: Vec<GalleryEntry>, query: &GalleryQuery) -> Vec<GalleryEntry> {
    let (skip, take) = query.page();
    entries.into_iter().skip(skip).take(take).collect()
}

/// Remove administratively blocked versions from an upstream-derived entry.
///
/// Fails open through `blocked_versions_for`, which logs and returns an empty
/// set on a repository error — a database blip should show more versions than
/// intended, never blank the gallery. The download gate re-checks the concrete
/// coordinate on every request, so nothing here can make blocked bytes
/// retrievable.
async fn filter_blocked(
    svc: &Arc<ProxyService>,
    registry: &str,
    kind: RegistryKind,
    entry: &mut GalleryEntry,
) {
    let blocked = svc
        .blocked_versions_for(registry, &entry.qualified_name(), kind)
        .await;
    if blocked.is_empty() {
        return;
    }
    let before = entry.versions.len();
    entry.versions.retain(|v| !blocked.contains(&v.version));
    if entry.versions.len() < before {
        tracing::debug!(
            registry,
            extension = %entry.qualified_name(),
            removed = before - entry.versions.len(),
            "hid blocked versions from the extension gallery"
        );
    }
}

/// The id of this registry's VSIX signing key, when it holds one
/// (`[registries.vsx_signing]`, RFC 0020).
async fn registry_key_id(local_svc: &LocalRegistryService, registry: &str) -> Option<String> {
    super::signing::registry_key(local_svc, registry)
        .await
        .map(|k| k.key_id().to_owned())
}

/// RFC 0020 §5.1's local branch: every version this registry holds is signed
/// by its key, so every version advertises the signature and the key.
async fn apply_registry_key(
    local_svc: &LocalRegistryService,
    registry: &str,
    entry: &mut GalleryEntry,
) {
    if let Some(id) = registry_key_id(local_svc, registry).await {
        entry.sign_locally(&id);
    }
}

/// Build an entry from the metadata an adapter resolved.
///
/// Both adapters populate `PackageMetadata.extra`; OpenVSX's is the richer of
/// the two (`crates/adapters/src/registry/openvsx.rs`), and the VS Code
/// Marketplace's carries only the resolved version, display name and
/// description. Whatever is absent renders as absent rather than blocking the
/// entry — an extension that installs with a plain tile beats one the editor
/// cannot see.
fn entry_from_metadata(
    extension_id: &str,
    meta: &batlehub_core::entities::PackageMetadata,
) -> Option<GalleryEntry> {
    let (publisher, name) = extension_id.split_once('.')?;
    let extra = &meta.extra;
    let s = |k: &str| {
        extra
            .get(k)
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };

    let version = s("resolved_version").unwrap_or_else(|| meta.id.version.clone());
    let last_updated = meta
        .published_at
        .map(|t| t.to_rfc3339())
        .unwrap_or_default();

    Some(GalleryEntry {
        publisher: publisher.to_owned(),
        extension_name: name.to_owned(),
        extension_id: super::render::synthetic_extension_uuid(publisher, name),
        display_name: s("display_name"),
        short_description: s("description"),
        categories: Vec::new(),
        tags: Vec::new(),
        install_count: extra
            .get("download_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        versions: vec![super::render::GalleryVersion {
            version,
            last_updated,
            engine: None,
            extension_pack: Vec::new(),
            extension_dependencies: Vec::new(),
            pre_release: false,
            // RFC 0020 §5.1's relay branch: the upstream signed it, this
            // registry advertises the archive at its own route and fetches
            // it on request. Never re-signed.
            signature: (meta.is_signed == Some(true)).then(|| {
                super::render::SignatureSource::Upstream {
                    public_key: s("public_key_url").is_some(),
                }
            }),
        }],
        upstream: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_core::entities::PackageMetadata;

    fn metadata(extra: serde_json::Value) -> PackageMetadata {
        PackageMetadata {
            id: PackageId::new("vsx", "acme.tool", "latest"),
            published_at: None,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra,
            cache_control: None,
        }
    }

    #[test]
    fn upstream_metadata_becomes_an_entry() {
        let e = entry_from_metadata(
            "acme.tool",
            &metadata(serde_json::json!({
                "resolved_version": "2.1.0",
                "display_name": "Acme Tool",
                "description": "Does the thing",
                "download_count": 1234
            })),
        )
        .expect("entry");

        assert_eq!(e.publisher, "acme");
        assert_eq!(e.extension_name, "tool");
        assert_eq!(e.install_count, 1234);
        assert_eq!(e.versions[0].version, "2.1.0");
    }

    /// The VS Code Marketplace adapter reports only three `extra` keys; the
    /// entry must still render.
    #[test]
    fn sparse_metadata_still_becomes_an_entry() {
        let e = entry_from_metadata("acme.tool", &metadata(serde_json::json!({}))).expect("entry");
        assert_eq!(e.versions[0].version, "latest");
        assert!(e.display_name.is_none());
        assert_eq!(e.install_count, 0);
    }

    #[test]
    fn an_id_without_a_publisher_is_not_an_extension() {
        assert!(entry_from_metadata("nopublisher", &metadata(serde_json::json!({}))).is_none());
    }
}
