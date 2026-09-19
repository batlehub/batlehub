//! The upstream-shaped NAR URL index (RFC 0028 §4.4).
//!
//! # The problem it solves
//!
//! This proxy rewrites a narinfo's `URL:` to `nar/{storeHash}/{basename}` so
//! that a NAR request carries the coordinate the block is enforced on. A client
//! that fetched the *same* narinfo before this registry existed — or before the
//! rewrite landed — holds the upstream's own `nar/{fileHash}.nar.zst` in its
//! disk cache, and Nix keeps a positive narinfo entry for
//! `narinfo-cache-positive-ttl`, which is **30 days**. So for a month after a
//! migration, real requests arrive in the upstream's shape, carrying no store
//! hash and therefore no coordinate.
//!
//! Serving those from the upstream unconditionally would be a hole straight
//! through the block: the whole point of the rewrite is that a NAR is never
//! fetched outside a coordinate.
//!
//! # The shape of the answer
//!
//! When a narinfo is served, the pair (upstream NAR basename → coordinate) is a
//! *derived fact of that document*. It is written to the metadata cache under
//! [`nar_url_index_key`], and the upstream-shape route reads it: a hit is served
//! as the coordinate, a miss is `404`.
//!
//! A `404` is the right miss, not a fallback to the upstream. Nix reads it as
//! "not in this cache", drops its cached narinfo, refetches it, gets the
//! rewritten `URL:`, and succeeds. One extra round trip, once per path, and
//! never a NAR served outside a coordinate.
//!
//! # Why it is a cache entry and not a table
//!
//! It is derived, not authoritative: it can always be recomputed by re-reading
//! the narinfo, and it is meaningless once that narinfo has expired. Giving it
//! the metadata TTL is what keeps the two in step without a second expiry rule
//! to reason about — and an entry that is lost is a `404`, which is the
//! protocol's own recoverable answer rather than an error.
//!
//! The value is a whole [`PackageMetadata`], because that is what a
//! [`CacheEntry`] holds and because the coordinate is exactly what the reader
//! needs: `id` carries `{name, version, artifact}` and the route can go straight
//! to the download path without re-reading anything.

use std::time::Duration;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    ports::{CacheEntry, CacheStore},
};

/// The cache key one upstream NAR basename is indexed under.
///
/// Keyed by registry as well as basename: two registries may front two caches
/// that both serve a NAR of the same file hash, and they are different bytes
/// under different coordinates.
///
/// The `narurl:` prefix rather than `doc:`, `meta:` or `artifact:` — this is
/// none of those, and sharing a prefix with one would make a prefix sweep take
/// it for that.
pub fn nar_url_index_key(registry: &str, basename: &str) -> String {
    format!("narurl:{registry}:{basename}")
}

/// The coordinate a NAR of `basename` is served under: `{hash}/{basename}`.
pub fn nar_artifact(store_hash: &str, basename: &str) -> String {
    format!("{store_hash}/{basename}")
}

/// Record that `basename` is the NAR of this coordinate.
///
/// Best-effort on purpose: a cache that cannot be written is a `404` on the
/// upstream-shape route later, which costs the client one refetch of a narinfo
/// it can always ask for again. Failing the narinfo request — the one every
/// substitution goes through — to protect a recovery path would be the wrong
/// trade by a wide margin.
pub async fn remember_nar_url(
    cache: &dyn CacheStore,
    registry: &str,
    basename: &str,
    coordinate: PackageId,
    ttl: Duration,
) {
    let key = nar_url_index_key(registry, basename);
    let now = chrono::Utc::now();
    let entry = CacheEntry {
        metadata: PackageMetadata {
            id: coordinate,
            published_at: None,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control: None,
        },
        cached_at: now,
        expires_at: Some(
            now + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::hours(1)),
        ),
    };
    if let Err(e) = cache.set(&key, entry, Some(ttl)).await {
        tracing::debug!(
            registry = %registry,
            basename = %basename,
            error = %e,
            "could not index an upstream NAR URL; a stale client will refetch its narinfo"
        );
    }
}

/// The coordinate an upstream-shaped NAR basename resolves to, if one was
/// indexed and has not expired.
pub async fn lookup_nar_url(
    cache: &dyn CacheStore,
    registry: &str,
    basename: &str,
) -> Option<PackageId> {
    let key = nar_url_index_key(registry, basename);
    match cache.get(&key).await {
        Ok(Some(entry)) => Some(entry.metadata.id),
        Ok(None) => None,
        Err(e) => {
            tracing::debug!(
                registry = %registry,
                basename = %basename,
                error = %e,
                "could not read the upstream NAR URL index"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_scoped_to_the_registry() {
        assert_ne!(
            nar_url_index_key("a", "075lhsj.nar.zst"),
            nar_url_index_key("b", "075lhsj.nar.zst"),
            "two caches may serve a NAR of the same file hash under different \
             coordinates, so the index cannot be registry-wide"
        );
    }

    #[test]
    fn the_prefix_is_not_shared_with_documents_or_metadata() {
        let key = nar_url_index_key("r1", "x.nar.zst");
        assert!(key.starts_with("narurl:"));
        for taken in ["doc:", "meta:", "artifact:"] {
            assert!(
                !key.starts_with(taken),
                "'{taken}' is another subsystem's prefix"
            );
        }
    }

    #[test]
    fn the_artifact_carries_the_store_hash_the_coordinate_needs() {
        let a = nar_artifact("0001npbf2n4z3pjy6vm2mw8ywkqixxs6", "075lhsj.nar.zst");
        assert_eq!(a, "0001npbf2n4z3pjy6vm2mw8ywkqixxs6/075lhsj.nar.zst");
        // Legal as a sub-coordinate: `validate_path_safe` refuses `..`, empty
        // and `.` segments, not separators.
        assert!(batlehub_core::services::validate_path_safe("artifact", &a).is_ok());
    }
}
