//! Who pulled a version (RFC 0018 §4.2 *Who pulled what*, decision 28):
//! the one query over `access_events` that the flip alert, the `pullers`
//! endpoint and `batlehub verdicts pullers` all read — one filter, one
//! aggregation, so the list an incident is handed is the list the alert
//! carried.
//!
//! The report is *exposure*, in RFC 0002's sense (decision 5): an allowed
//! download delivered bytes, a refused one did not, and the two are never
//! mixed. [`refused_for`] is the other question — who asked while the
//! version was held — and it is what `ArtifactReleased` is bounded by.
//!
//! Anonymous pulls are kept, grouped under `ip:<addr>` (RFC 0002 decision
//! 6): dropping them understates the blast radius in exactly the CI-heavy
//! deployments that need this most.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::entities::{AccessAction, AccessEvent, AccessResult, EventFilter, PackageId};
use crate::error::CoreError;
use crate::ports::PackageRepository;

/// Rows read per page while walking the log; bounded so a runaway window
/// on a popular package stays a query, not a table scan into memory.
const PAGE: u64 = 1_000;
/// Pages walked at most — a million events over one coordinate is a
/// window nobody asked for on purpose.
const MAX_PAGES: u64 = 1_000;

/// One identity's pulls of one version inside the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Puller {
    /// The user id, or `ip:<addr>` for an anonymous pull, or `anonymous`
    /// when not even the address was recorded.
    pub identity: String,
    pub role: String,
    pub first_pull: DateTime<Utc>,
    pub last_pull: DateTime<Utc>,
    pub count: u64,
}

/// The filter both callers use: downloads of the package since `since`,
/// refused ones only when `refused`.
pub fn pullers_filter(pkg: &PackageId, since: DateTime<Utc>, refused: bool) -> EventFilter {
    EventFilter {
        registry: Some(pkg.registry.clone()),
        package_name: Some(pkg.name.clone()),
        actions: vec![AccessAction::Download],
        from: Some(since),
        denied_only: refused,
        limit: PAGE,
        ..EventFilter::new()
    }
}

/// How an event names who did it.
pub fn identity_of(e: &AccessEvent) -> String {
    match (&e.user_id, &e.ip_address) {
        (Some(u), _) if !u.is_empty() => u.clone(),
        (_, Some(ip)) if !ip.is_empty() => format!("ip:{ip}"),
        _ => "anonymous".to_owned(),
    }
}

/// Group the events of one version by identity. `refused` selects the
/// denied events (who asked during a hold) instead of the allowed ones.
pub fn aggregate(events: &[AccessEvent], pkg: &PackageId, refused: bool) -> Vec<Puller> {
    let mut by_identity: BTreeMap<String, Puller> = BTreeMap::new();
    for e in events {
        let Some(id) = &e.package_id else {
            continue;
        };
        if id.name != pkg.name || id.version != pkg.version {
            continue;
        }
        let wanted = match &e.result {
            AccessResult::Allowed => !refused,
            AccessResult::Denied { .. } => refused,
            _ => false,
        };
        if !wanted {
            continue;
        }
        let identity = identity_of(e);
        let entry = by_identity
            .entry(identity.clone())
            .or_insert_with(|| Puller {
                identity,
                role: e.user_role.to_string(),
                first_pull: e.timestamp,
                last_pull: e.timestamp,
                count: 0,
            });
        entry.count += 1;
        entry.first_pull = entry.first_pull.min(e.timestamp);
        entry.last_pull = entry.last_pull.max(e.timestamp);
    }
    let mut out: Vec<Puller> = by_identity.into_values().collect();
    out.sort_by(|a, b| {
        b.last_pull
            .cmp(&a.last_pull)
            .then(a.identity.cmp(&b.identity))
    });
    out
}

/// Walk the log for `pkg` since `since` and aggregate. Pages by offset
/// under one filter, bounded by [`MAX_PAGES`].
pub async fn collect(
    repo: &dyn PackageRepository,
    pkg: &PackageId,
    since: DateTime<Utc>,
    refused: bool,
) -> Result<Vec<Puller>, CoreError> {
    let mut events: Vec<AccessEvent> = Vec::new();
    let mut filter = pullers_filter(pkg, since, refused);
    for _ in 0..MAX_PAGES {
        let page = repo.list_events(filter.clone()).await?;
        let n = page.len() as u64;
        events.extend(page);
        if n < PAGE {
            break;
        }
        filter.offset += PAGE;
    }
    Ok(aggregate(&events, pkg, refused))
}

/// Who pulled `pkg` (allowed downloads) since `since`.
pub async fn pullers_for(
    repo: &dyn PackageRepository,
    pkg: &PackageId,
    since: DateTime<Utc>,
) -> Result<Vec<Puller>, CoreError> {
    collect(repo, pkg, since, false).await
}

/// Who was refused `pkg` since `since` — the set `ArtifactReleased` is
/// bounded by (RFC 0018 §4.2).
pub async fn refused_for(
    repo: &dyn PackageRepository,
    pkg: &PackageId,
    since: DateTime<Utc>,
) -> Result<Vec<Puller>, CoreError> {
    collect(repo, pkg, since, true).await
}

/// The CSV the export and the CLI print: one header, one row per identity.
pub fn to_csv(rows: &[Puller]) -> String {
    use crate::services::csv::field;
    let mut out = String::from("identity,role,first_pull,last_pull,count\n");
    for r in rows {
        out.push_str(&format!(
            "{},{},{},{},{}\n",
            field(&r.identity),
            field(&r.role),
            r.first_pull.to_rfc3339(),
            r.last_pull.to_rfc3339(),
            r.count
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::Role;

    fn pkg() -> PackageId {
        PackageId::new("npm", "left-pad", "1.3.1")
    }

    fn pull(user: Option<&str>, ip: Option<&str>, version: &str, ago_secs: i64) -> AccessEvent {
        let mut e = AccessEvent::allowed_download(
            PackageId::new("npm", "left-pad", version),
            user.map(str::to_owned),
            Role::User,
        );
        e.ip_address = ip.map(str::to_owned);
        e.timestamp = Utc::now() - chrono::Duration::seconds(ago_secs);
        e
    }

    #[test]
    fn pulls_group_by_identity_and_skip_other_versions_and_refusals() {
        let mut denied = pull(Some("ci"), None, "1.3.1", 5);
        denied.result = AccessResult::Denied {
            reason: "held".into(),
        };
        let events = vec![
            pull(Some("alice"), None, "1.3.1", 100),
            pull(Some("alice"), None, "1.3.1", 10),
            pull(None, Some("10.0.0.7"), "1.3.1", 50),
            pull(Some("bob"), None, "1.3.0", 20), // another version
            denied,
        ];
        let rows = aggregate(&events, &pkg(), false);
        assert_eq!(rows.len(), 2, "{rows:?}");
        let alice = rows.iter().find(|r| r.identity == "alice").unwrap();
        assert_eq!(alice.count, 2);
        assert!(alice.first_pull < alice.last_pull);
        assert!(rows.iter().any(|r| r.identity == "ip:10.0.0.7"));
        assert_eq!(rows[0].identity, "alice", "newest last pull first");

        let refused = aggregate(&events, &pkg(), true);
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].identity, "ci");
    }

    #[test]
    fn the_csv_quotes_what_needs_quoting() {
        let mut e = pull(Some("a,b"), None, "1.3.1", 1);
        e.user_role = Role::Admin;
        let csv = to_csv(&aggregate(&[e], &pkg(), false));
        assert!(csv.starts_with("identity,role,first_pull,last_pull,count\n"));
        assert!(csv.contains("\"a,b\",admin,"), "{csv}");
    }

    #[test]
    fn the_filter_names_the_package_the_window_and_only_downloads() {
        let f = pullers_filter(&pkg(), Utc::now(), true);
        assert_eq!(f.package_name.as_deref(), Some("left-pad"));
        assert_eq!(f.actions, vec![AccessAction::Download]);
        assert!(f.denied_only);
        assert!(f.from.is_some());
    }
}
