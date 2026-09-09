use std::future::Future;

use super::AdminService;
use crate::entities::{ExploreEntry, ExploreFilter, RegistryStat};
use crate::error::CoreError;
use crate::services::explore_cache::{
    packages_cache_key, packages_entry_registries, stats_cache_key,
};

impl AdminService {
    /// Shared cache-first-then-repo-with-stale-fallback flow for
    /// [`Self::explore_packages`] and [`Self::count_explore_packages`], which
    /// both read/write the same `(items, count)`-shaped cache entry (each
    /// caller only cares about one half; see `PackageEntry` — every
    /// `set_packages` call carries real data for the half it computed and a
    /// placeholder for the other, then blindly overwrites the entry).
    ///
    /// `upstream_unavailable` (third tuple element) is `true` only when the DB
    /// is unreachable **and** no stale cache entry exists. When stale data is
    /// served the flag stays `false` so callers can distinguish "stale but
    /// usable" from "nothing at all".
    ///
    /// Not shared with [`Self::registry_explore_stats`]: that method reads a
    /// differently-shaped cache entry (`Vec<RegistryStat>` via
    /// `get_stats`/`set_stats`/`get_stale_stats`, no `(items, count)` pair), so
    /// folding it into this helper too would mean genericizing over the cache
    /// accessor methods as well as the fetch — not worth it for one call site.
    async fn cached_packages_query(
        &self,
        key: &str,
        filter: &ExploreFilter,
        fetch: impl Future<Output = Result<(Vec<ExploreEntry>, u64), CoreError>>,
    ) -> Result<(Vec<ExploreEntry>, u64, bool), CoreError> {
        if let Some((items, count)) = self.explore_cache.get_packages(key).await {
            return Ok((items, count, false));
        }
        match fetch.await {
            Ok((items, count)) => {
                let regs = packages_entry_registries(filter);
                self.explore_cache
                    .set_packages(key, items.clone(), count, regs)
                    .await;
                Ok((items, count, false))
            }
            Err(_) => {
                if let Some((items, count)) = self.explore_cache.get_stale_packages(key).await {
                    Ok((items, count, false))
                } else {
                    Ok((vec![], 0, true))
                }
            }
        }
    }

    /// Returns `(entries, upstream_unavailable)`.
    pub async fn explore_packages(
        &self,
        filter: ExploreFilter,
    ) -> Result<(Vec<ExploreEntry>, bool), CoreError> {
        // Namespaced so an items-lookup and a count-lookup can never collide on
        // the same cache entry, even if a future caller passes matching
        // limit/offset filters to both.
        let key = format!("items:{}", packages_cache_key(&filter));
        let fetch = async {
            let items = self.repo.explore_packages(filter.clone()).await?;
            Ok((items, 0))
        };
        let (items, _count, unavailable) = self.cached_packages_query(&key, &filter, fetch).await?;
        Ok((items, unavailable))
    }

    /// The catalogue rows for a **named set** of coordinates, uncached.
    ///
    /// [`Self::explore_packages`] puts every answer in the explore cache, which
    /// holds entries for ten minutes and frees them only on an explicit
    /// `invalidate`. That is right for the catalogue's own pages, whose keys
    /// repeat all day: a registry, a page number, a sort order.
    ///
    /// It is wrong for a filter keyed on `name_in`, because the key then
    /// carries *the exact set of names some other query returned* — for the
    /// upstream search's `already_cached` flag, whatever a third-party
    /// relevance search answered this second. Two such keys match only if the
    /// upstream returned the same set in the same order for the same viewer, so
    /// the entry is written, never read again, and never freed; and the key
    /// alone can carry hundreds of coordinates. A caller varying one query
    /// parameter would grow the map without bound and without ever getting a
    /// hit for it.
    ///
    /// So this one goes straight to the repository. The lookup is a
    /// primary-key-shaped `IN` over names the caller already named; it is the
    /// cheap half of that request.
    pub async fn explore_packages_uncached(
        &self,
        filter: ExploreFilter,
    ) -> Result<Vec<ExploreEntry>, CoreError> {
        self.repo.explore_packages(filter).await
    }

    /// Returns `(count, upstream_unavailable)`.
    pub async fn count_explore_packages(
        &self,
        filter: ExploreFilter,
    ) -> Result<(u64, bool), CoreError> {
        let key = format!("count:{}", packages_cache_key(&filter));
        let fetch = async {
            let count = self.repo.count_explore_packages(filter.clone()).await?;
            Ok((vec![], count))
        };
        let (_items, count, unavailable) = self.cached_packages_query(&key, &filter, fetch).await?;
        Ok((count, unavailable))
    }

    /// Returns `(stats, upstream_unavailable)`.
    ///
    /// **Keyed by the viewer as well as the registries** (RFC 0015 §4.4 rule 3).
    /// The numbers are filtered per identity now, so a key that named only the
    /// registries would replay one caller's counts to the next — finding 11's
    /// lesson, which §4.4 says an aggregate is *cheaper* to apply than a
    /// document because there are far fewer distinct tiles.
    pub async fn registry_explore_stats(
        &self,
        accessible_registries: &[String],
        viewer: &crate::entities::ExploreViewer,
    ) -> Result<(Vec<RegistryStat>, bool), CoreError> {
        let key = stats_cache_key(accessible_registries, viewer);
        if let Some(stats) = self.explore_cache.get_stats(&key).await {
            return Ok((stats, false));
        }
        match self
            .repo
            .registry_explore_stats(accessible_registries, viewer)
            .await
        {
            Ok(stats) => {
                self.explore_cache
                    .set_stats(&key, stats.clone(), accessible_registries.to_vec())
                    .await;
                Ok((stats, false))
            }
            Err(_) => {
                if let Some(stats) = self.explore_cache.get_stale_stats(&key).await {
                    Ok((stats, false))
                } else {
                    Ok((vec![], true))
                }
            }
        }
    }
}
