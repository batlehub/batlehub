//! In-memory [`AdvisoryRepository`] for tests and database-less deployments.
//!
//! The exposure report joins flags against the access log, so this store
//! reads the events from the [`PackageRepository`] it is handed — the same
//! join the Postgres adapter does in SQL, done here in Rust so a web test can
//! assert the report's shape without a database.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use uuid::Uuid;

use batlehub_core::{
    entities::{
        AccessAction, AccessResult, EventFilter, ExposurePage, ExposureQuery, ExposureRow,
        ExposureWhen, FlagFilter, FlagSourceCoverage, PackageFlag, RegistryScanState,
    },
    error::CoreError,
    ports::{AdvisoryRepository, PackageRepository},
};

#[derive(Default)]
pub struct InMemoryAdvisoryRepository {
    flags: RwLock<Vec<PackageFlag>>,
    scan_state: RwLock<BTreeMap<String, RegistryScanState>>,
    /// Where the pulls come from. `None`: the report is always empty.
    events: Option<Arc<dyn PackageRepository>>,
}

impl InMemoryAdvisoryRepository {
    pub fn new() -> Self {
        Self::default()
    }

    /// A store whose exposure report reads `events`' access log.
    pub fn with_events(events: Arc<dyn PackageRepository>) -> Self {
        Self {
            events: Some(events),
            ..Self::default()
        }
    }

    pub fn arc() -> Arc<dyn AdvisoryRepository> {
        Arc::new(Self::default())
    }
}

fn matches(f: &PackageFlag, filter: &FlagFilter, now: DateTime<Utc>) -> bool {
    filter.registry.as_deref().is_none_or(|r| r == f.registry)
        && filter
            .package_name
            .as_deref()
            .is_none_or(|p| p == f.package_name)
        && filter.source.as_deref().is_none_or(|s| s == f.source)
        && filter.effect.is_none_or(|e| e == f.effect)
        && (filter.include_dead || f.is_live(now))
}

#[async_trait]
impl AdvisoryRepository for InMemoryAdvisoryRepository {
    async fn upsert_flag(&self, mut flag: PackageFlag) -> Result<(Uuid, bool), CoreError> {
        let mut guard = self.flags.write().await;
        if let Some(existing) = guard
            .iter_mut()
            .find(|f| f.source == flag.source && f.external_id == flag.external_id)
        {
            flag.id = existing.id;
            flag.first_seen = existing.first_seen;
            flag.revoked_at = None;
            *existing = flag;
            return Ok((existing.id, false));
        }
        let id = flag.id;
        guard.push(flag);
        Ok((id, true))
    }

    async fn revoke_flag(
        &self,
        source: &str,
        external_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, CoreError> {
        let mut guard = self.flags.write().await;
        match guard
            .iter_mut()
            .find(|f| f.source == source && f.external_id == external_id && f.revoked_at.is_none())
        {
            Some(f) => {
                f.revoked_at = Some(now);
                f.updated_at = now;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn get_flag(
        &self,
        source: &str,
        external_id: &str,
    ) -> Result<Option<PackageFlag>, CoreError> {
        Ok(self
            .flags
            .read()
            .await
            .iter()
            .find(|f| f.source == source && f.external_id == external_id)
            .cloned())
    }

    async fn list_flags(&self, filter: &FlagFilter) -> Result<Vec<PackageFlag>, CoreError> {
        let now = Utc::now();
        let mut out: Vec<PackageFlag> = self
            .flags
            .read()
            .await
            .iter()
            .filter(|f| matches(f, filter, now))
            .cloned()
            .collect();
        out.sort_by_key(|f| std::cmp::Reverse(f.updated_at));
        let limit = if filter.limit == 0 {
            usize::MAX
        } else {
            filter.limit as usize
        };
        Ok(out
            .into_iter()
            .skip(filter.offset as usize)
            .take(limit)
            .collect())
    }

    async fn count_flags(&self, filter: &FlagFilter) -> Result<u64, CoreError> {
        let now = Utc::now();
        Ok(self
            .flags
            .read()
            .await
            .iter()
            .filter(|f| matches(f, filter, now))
            .count() as u64)
    }

    async fn live_flags_for_package(
        &self,
        registry: &str,
        name: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<PackageFlag>, CoreError> {
        Ok(self
            .flags
            .read()
            .await
            .iter()
            .filter(|f| f.registry == registry && f.package_name == name && f.is_live(now))
            .cloned()
            .collect())
    }

    async fn list_exposure(&self, query: &ExposureQuery) -> Result<ExposurePage, CoreError> {
        let Some(events) = &self.events else {
            return Ok(ExposurePage::default());
        };
        let flags: Vec<PackageFlag> = self
            .flags
            .read()
            .await
            .iter()
            .filter(|f| query.registry.as_deref().is_none_or(|r| r == f.registry))
            .filter(|f| {
                query
                    .package_name
                    .as_deref()
                    .is_none_or(|p| p == f.package_name)
            })
            .filter(|f| query.source.as_deref().is_none_or(|s| s == f.source))
            .filter(|f| query.min_effect.is_none_or(|e| f.effect >= e))
            .cloned()
            .collect();
        if flags.is_empty() {
            return Ok(ExposurePage::default());
        }
        let pulls = events
            .list_events(EventFilter {
                registry: query.registry.clone(),
                package_name: query.package_name.clone(),
                user_id: None,
                actions: vec![AccessAction::Download],
                from: query.from,
                to: query.to,
                denied_only: false,
                limit: 1_000_000,
                offset: 0,
            })
            .await?;

        // consumer × coordinate × flag → the row being accumulated.
        let mut rows: BTreeMap<(String, String, String, String, Uuid), ExposureRow> =
            BTreeMap::new();
        for e in pulls
            .iter()
            .filter(|e| matches!(e.result, AccessResult::Allowed))
        {
            let Some(pkg) = &e.package_id else { continue };
            let consumer = e.user_id.clone().unwrap_or_else(|| "anonymous".to_owned());
            for f in flags
                .iter()
                .filter(|f| f.registry == pkg.registry && f.package_name == pkg.name)
                .filter(|f| f.covers(&pkg.version))
            {
                let key = (
                    consumer.clone(),
                    pkg.registry.clone(),
                    pkg.name.clone(),
                    pkg.version.clone(),
                    f.id,
                );
                let before = u64::from(e.timestamp < f.first_seen);
                let row = rows.entry(key).or_insert_with(|| ExposureRow {
                    consumer: consumer.clone(),
                    consumer_role: e.user_role.to_string(),
                    registry: pkg.registry.clone(),
                    package_name: pkg.name.clone(),
                    version: pkg.version.clone(),
                    flag_id: f.id,
                    source: f.source.clone(),
                    external_id: f.external_id.clone(),
                    kind: f.kind.clone(),
                    effect: f.effect,
                    severity: f.severity,
                    summary: f.summary.clone(),
                    flag_first_seen: f.first_seen,
                    pulls: 0,
                    pulls_before_flag: 0,
                    first_pull: e.timestamp,
                    last_pull: e.timestamp,
                });
                row.pulls += 1;
                row.pulls_before_flag += before;
                row.first_pull = row.first_pull.min(e.timestamp);
                row.last_pull = row.last_pull.max(e.timestamp);
            }
        }
        let mut rows: Vec<ExposureRow> = rows
            .into_values()
            .filter(|r| match query.when {
                ExposureWhen::Any => true,
                ExposureWhen::BeforeFlag => r.pulls_before_flag > 0,
                ExposureWhen::AfterFlag => r.pulls > r.pulls_before_flag,
            })
            .collect();
        rows.sort_by(|a, b| {
            b.last_pull
                .cmp(&a.last_pull)
                .then_with(|| a.consumer.cmp(&b.consumer))
                .then_with(|| a.registry.cmp(&b.registry))
                .then_with(|| a.package_name.cmp(&b.package_name))
                .then_with(|| a.version.cmp(&b.version))
                .then_with(|| a.flag_id.cmp(&b.flag_id))
        });
        if let Some(after) = &query.after {
            // Everything up to and including the cursor's row is served.
            if let Some(pos) = rows.iter().position(|r| r.cursor() == *after) {
                rows.drain(..=pos);
            } else {
                rows.retain(|r| r.last_pull < after.last_pull);
            }
        }
        let limit = if query.limit == 0 {
            usize::MAX
        } else {
            query.limit as usize
        };
        let next = (rows.len() > limit).then(|| rows[limit - 1].cursor().encode());
        rows.truncate(limit);
        Ok(ExposurePage { rows, next })
    }

    async fn source_coverage(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<FlagSourceCoverage>, CoreError> {
        let mut by_source: BTreeMap<String, FlagSourceCoverage> = BTreeMap::new();
        for f in self.flags.read().await.iter() {
            let entry = by_source
                .entry(f.source.clone())
                .or_insert_with(|| FlagSourceCoverage {
                    source: f.source.clone(),
                    live_flags: 0,
                    last_push_at: None,
                });
            if f.is_live(now) {
                entry.live_flags += 1;
            }
            entry.last_push_at = Some(
                entry
                    .last_push_at
                    .map_or(f.updated_at, |t| t.max(f.updated_at)),
            );
        }
        Ok(by_source.into_values().collect())
    }

    async fn record_scan_state(&self, state: &RegistryScanState) -> Result<(), CoreError> {
        self.scan_state
            .write()
            .await
            .insert(state.registry.clone(), state.clone());
        Ok(())
    }

    async fn list_scan_state(&self) -> Result<Vec<RegistryScanState>, CoreError> {
        Ok(self.scan_state.read().await.values().cloned().collect())
    }
}
