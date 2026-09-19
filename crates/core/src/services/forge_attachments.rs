//! Forgejo's attachment endpoint: `{forge}/attachments/{uuid}` (RFC 0019 §4.2).
//!
//! Forgejo gives every release asset a uuid and serves it from a repository-less
//! path beside the API. Clients build that URL themselves rather than follow the
//! `browser_download_url` in the release document — `mise`'s `forgejo:` backend
//! does exactly that, and *only* that:
//!
//! ```text
//! {api_url minus /api/v1}/attachments/{uuid}
//! ```
//!
//! so rewriting the document's URLs — which is what routes every other forge
//! client here — leaves that client going straight to the forge. In a closed
//! world it simply fails; in an open one it silently bypasses the policy, the
//! cache and the audit trail. BatleHub therefore answers the same shape.
//!
//! The uuid names no repository, so the coordinate cannot be read out of the
//! request: it is remembered instead, from the release document this proxy
//! rewrote, where the uuid and the `(owner/repo, tag, filename)` it belongs to
//! are in the same object. A uuid nothing has been remembered for is a `404` —
//! the route resolves what this instance has served, and is not an opaque relay
//! for the forge's whole attachment space.
//!
//! The remembered coordinate is the one the by-name download route builds, so
//! the two paths share a storage key, a rule chain and an audit row: an asset
//! fetched by uuid and the same asset fetched by name are one cached artifact.

use std::time::Duration;

use chrono::Utc;

use crate::entities::{PackageId, PackageMetadata};
use crate::ports::{CacheEntry, CacheStore};

/// One release asset, as the forge's release document named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeAttachment {
    /// Forgejo's own handle for the asset — the last segment of the URL a
    /// client builds.
    pub uuid: String,
    /// The release's tag, which the coordinate's version is.
    pub tag: String,
    /// The asset's filename, which its artifact selector is.
    pub filename: String,
}

/// The attachments a forge release document names.
///
/// Reads one release object or a list of them — the two shapes the release
/// routes answer with — and yields an entry per asset that has all three of a
/// uuid, a filename and a release tag.
///
/// Gated on that shape rather than on the registry's kind: the handlers that
/// call this serve every kind there is, and only a forge release object carries
/// an `assets` array whose entries have a `uuid` beside a `tag_name`. Anything
/// else walks two fields deep and yields nothing.
pub fn from_document(doc: &serde_json::Value) -> Vec<ForgeAttachment> {
    let mut found = Vec::new();
    match doc {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_one(item, &mut found);
            }
        }
        other => collect_one(other, &mut found),
    }
    found
}

fn collect_one(release: &serde_json::Value, found: &mut Vec<ForgeAttachment>) {
    let Some(tag) = release.get("tag_name").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(assets) = release.get("assets").and_then(|v| v.as_array()) else {
        return;
    };
    for asset in assets {
        let (Some(uuid), Some(filename)) = (
            asset.get("uuid").and_then(|v| v.as_str()),
            asset.get("name").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        // The filename comes from a document upstream wrote, and becomes an
        // artifact selector: a name carrying a separator or a dot segment is
        // not an asset name, it is an attempt at another key. `ProxyService`
        // and `ensure_safe_key` refuse it further down, but an entry that
        // cannot be a legitimate asset has no business being remembered at
        // all — dropping it here is what keeps the uuid unanswerable rather
        // than answerable-and-refused.
        if !is_plain_filename(filename) {
            tracing::warn!(
                %filename,
                "forge release document names an asset with a path in its name; not remembered"
            );
            continue;
        }
        found.push(ForgeAttachment {
            uuid: uuid.to_owned(),
            tag: tag.to_owned(),
            filename: filename.to_owned(),
        });
    }
}

fn is_plain_filename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

/// How long a remembered attachment stays resolvable.
///
/// A uuid identifies one immutable attachment, so the mapping does not go
/// stale; the bound is there so an instance that has served a great many
/// release documents does not remember every asset of every one of them
/// forever. A client that comes back after it lapses re-reads the release
/// document — which is what it did to learn the uuid in the first place.
const TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// How long "these uuids are already remembered" is believed.
///
/// Deliberately tiny beside [`TTL`]. It exists to stop a release listing
/// rewriting its thousand entries on every single request, and five minutes of
/// that is the whole saving; anything longer only risks outliving an entry the
/// store evicted early, which would pin that attachment at `404`.
const SENTINEL_TTL: Duration = Duration::from_secs(300);

/// Where one attachment's coordinate is remembered.
///
/// Scoped by registry: two registries may proxy two forges, and a uuid is only
/// unique within one of them.
pub fn attachment_key(registry: &str, uuid: &str) -> String {
    format!("forge-attachment:{registry}:{uuid}")
}

/// The coordinate a uuid stands for: the one `…/releases/download/{tag}/{file}`
/// builds for the same asset.
pub fn coordinate(registry: &str, owner_repo: &str, attachment: &ForgeAttachment) -> PackageId {
    PackageId::new(registry, owner_repo, &attachment.tag)
        .with_artifact(format!("filename/{}", attachment.filename))
}

/// Remember what each uuid in a release document stands for.
///
/// Best-effort: a cache that refuses the write costs a later `404` on the
/// attachment route, which is the same answer as never having seen the
/// document. It must not cost the client the release document it asked for.
pub async fn remember(
    cache: &dyn CacheStore,
    registry: &str,
    owner_repo: &str,
    attachments: &[ForgeAttachment],
) {
    // **Once per distinct document, not once per request.** This runs inline on
    // the response path of every release listing — including one answered
    // entirely from the document cache — and a 50-release listing names about a
    // thousand assets. Writing them again on each request bought nothing: the
    // uuids are immutable and the entries are already there.
    //
    // The sentinel is keyed by a digest of the uuids, so a listing that gains a
    // release has a different key and is written; one that has not changed
    // costs a single cache read.
    //
    // It expires far sooner than the entries it stands for ([`SENTINEL_TTL`]
    // against [`TTL`]) on purpose: the store may evict an individual entry
    // before its own TTL, and a sentinel outliving what it vouches for would
    // pin that uuid at `404` for a month. A short one collapses the repeated
    // writes — which is all of the cost — and lets the entries be re-asserted
    // regularly anyway.
    let sentinel = sentinel_key(registry, owner_repo, attachments);
    if matches!(cache.get(&sentinel).await, Ok(Some(_))) {
        return;
    }

    let now = Utc::now();
    let expires_at = chrono::Duration::from_std(TTL).ok().map(|d| now + d);

    // Concurrent rather than one awaited round trip after another: these are a
    // thousand independent writes to the same store, and serialising them put
    // the whole latency of the slowest backend on the client's response.
    use futures::stream::StreamExt;
    const MAX_CONCURRENT_WRITES: usize = 16;
    futures::stream::iter(attachments)
        .for_each_concurrent(MAX_CONCURRENT_WRITES, |attachment| {
            let entry = CacheEntry {
                metadata: PackageMetadata::minimal(
                    coordinate(registry, owner_repo, attachment),
                    serde_json::Value::Null,
                ),
                cached_at: now,
                expires_at,
            };
            async move {
                if let Err(e) = cache
                    .set(
                        &attachment_key(registry, &attachment.uuid),
                        entry,
                        Some(TTL),
                    )
                    .await
                {
                    tracing::debug!(
                        uuid = %attachment.uuid,
                        error = %e,
                        "could not remember a forge attachment; it will answer 404 until the release document is read again"
                    );
                }
            }
        })
        .await;

    // Written last, so a run that failed part way is retried by the next
    // request rather than suppressed by its own sentinel.
    let entry = CacheEntry {
        metadata: PackageMetadata::minimal(
            PackageId::new(registry, owner_repo, "__attachments__"),
            serde_json::Value::Null,
        ),
        cached_at: now,
        expires_at: chrono::Duration::from_std(SENTINEL_TTL)
            .ok()
            .map(|d| now + d),
    };
    if let Err(e) = cache.set(&sentinel, entry, Some(SENTINEL_TTL)).await {
        tracing::debug!(
            owner_repo = %owner_repo,
            error = %e,
            "could not record that a release document's attachments were remembered"
        );
    }
}

/// A key that stands for "these exact uuids have already been remembered".
///
/// Digested rather than listed: the set runs to a thousand uuids for a busy
/// repository, and the key has to be bounded. Order-independent, because the
/// same document read twice must produce the same key and nothing guarantees
/// the extraction order — each uuid is hashed on its own and the digests are
/// summed by XOR.
fn sentinel_key(registry: &str, owner_repo: &str, attachments: &[ForgeAttachment]) -> String {
    use sha2::{Digest, Sha256};
    let mut acc = [0u8; 32];
    for attachment in attachments {
        let digest = Sha256::digest(attachment.uuid.as_bytes());
        for (a, d) in acc.iter_mut().zip(digest.iter()) {
            *a ^= d;
        }
    }
    format!(
        "forge-attachments-seen:{registry}:{owner_repo}:{}",
        hex::encode(acc)
    )
}

/// Read a uuid back into the coordinate it was remembered as.
pub async fn resolve(cache: &dyn CacheStore, registry: &str, uuid: &str) -> Option<PackageId> {
    match cache.get(&attachment_key(registry, uuid)).await {
        Ok(Some(entry)) => Some(entry.metadata.id),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(uuid = %uuid, error = %e, "forge attachment lookup failed");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment() -> ForgeAttachment {
        ForgeAttachment {
            uuid: "26305e39-a8d3-43ae-b846-f1958634ada6".to_owned(),
            tag: "v16.0.4".to_owned(),
            filename: "forgejo-16.0.4-linux-amd64".to_owned(),
        }
    }

    /// The point of the whole mechanism: by uuid and by name are one artifact.
    #[test]
    fn a_uuid_resolves_to_the_coordinate_the_by_name_route_builds() {
        let by_uuid = coordinate("fj", "forgejo/forgejo", &attachment());

        assert_eq!(by_uuid.registry, "fj");
        assert_eq!(by_uuid.name, "forgejo/forgejo");
        assert_eq!(by_uuid.version, "v16.0.4");
        assert_eq!(
            by_uuid.artifact.as_deref(),
            Some("filename/forgejo-16.0.4-linux-amd64")
        );
    }

    /// GitHub's release document is the same shape without the uuids, and a
    /// packument or a Maven listing is not this shape at all.
    #[test]
    fn a_document_with_no_forgejo_assets_yields_nothing() {
        let github = serde_json::json!([{
            "tag_name": "v1.0.0",
            "assets": [{ "id": 9, "name": "app.bin" }]
        }]);
        assert!(from_document(&github).is_empty());

        let npm = serde_json::json!({ "versions": { "1.0.0": { "dist": {} } } });
        assert!(from_document(&npm).is_empty());
    }

    /// Both release shapes: the listing is an array, the release by tag is one
    /// object, and a client may read either before it downloads.
    #[test]
    fn both_release_document_shapes_are_read() {
        let release = serde_json::json!({
            "tag_name": "v16.0.4",
            "assets": [
                { "name": "forgejo-16.0.4-linux-amd64",
                  "uuid": "26305e39-a8d3-43ae-b846-f1958634ada6" },
                { "name": "forgejo-16.0.4-linux-amd64.sha256",
                  "uuid": "70b18022-6534-43ab-b4d6-b900c4dd99cd" }
            ]
        });

        let from_one = from_document(&release);
        assert_eq!(from_one.len(), 2);
        assert_eq!(from_one[0], attachment());

        let from_list = from_document(&serde_json::json!([release]));
        assert_eq!(from_list, from_one);
    }

    /// The asset name comes from upstream and becomes an artifact selector, so
    /// a name that is a path is not an asset name.
    #[test]
    fn an_asset_named_with_a_path_is_not_remembered() {
        for hostile in ["../../etc/passwd", "a/b", "..", "", ".", "a\\b"] {
            let doc = serde_json::json!({
                "tag_name": "v1.0.0",
                "assets": [{ "name": hostile, "uuid": "u-1" }]
            });
            assert!(
                from_document(&doc).is_empty(),
                "remembered an asset named {hostile:?}"
            );
        }
    }

    /// A uuid is unique to one forge, not to this process.
    #[test]
    fn the_key_is_scoped_by_registry() {
        let a = attachment_key("fj-one", &attachment().uuid);
        let b = attachment_key("fj-two", &attachment().uuid);

        assert_ne!(a, b);
    }

    /// The sentinel says "*these* uuids", and it has to mean it: a listing that
    /// gained a release must not be skipped because the old one was written.
    /// Order-independent, because nothing promises the extraction order and the
    /// same document read twice has to produce the same key.
    #[test]
    fn the_sentinel_names_the_exact_set_and_ignores_its_order() {
        let one = attachment();
        let two = ForgeAttachment {
            uuid: "f1f1f1f1-0000-0000-0000-00000000000f".to_owned(),
            tag: "v16.0.5".to_owned(),
            filename: "forgejo-16.0.5-linux-amd64".to_owned(),
        };

        let forwards = sentinel_key("fj", "forgejo/forgejo", &[one.clone(), two.clone()]);
        let backwards = sentinel_key("fj", "forgejo/forgejo", &[two.clone(), one.clone()]);
        assert_eq!(forwards, backwards, "the same set is the same key");

        let smaller = sentinel_key("fj", "forgejo/forgejo", std::slice::from_ref(&one));
        assert_ne!(
            forwards, smaller,
            "a listing that gained a release must be written, not skipped"
        );

        // And it is scoped like the entries it vouches for.
        assert_ne!(
            forwards,
            sentinel_key("fj-two", "forgejo/forgejo", &[one.clone(), two.clone()])
        );
        assert_ne!(forwards, sentinel_key("fj", "other/repo", &[one, two]));
    }

    /// The sentinel must expire long before the entries it stands for: a store
    /// may evict one of them early, and a sentinel that outlived it would pin
    /// that attachment at `404` for the rest of its month.
    #[test]
    fn the_sentinel_expires_far_sooner_than_what_it_vouches_for() {
        assert!(SENTINEL_TTL < TTL / 100, "{SENTINEL_TTL:?} vs {TTL:?}");
    }
}
