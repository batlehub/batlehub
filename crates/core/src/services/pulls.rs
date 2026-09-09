//! What one identity pulled (RFC 0018 §4.2 *Who pulled what*): the
//! identity-scoped half of the report, and the transpose of
//! [`crate::services::pullers`].
//!
//! The two questions look alike and invert every part of the query. `pullers`
//! pins a coordinate and groups by identity — *who pulled this version*. This
//! pins an identity and groups by coordinate — *what did this account pull*. The
//! filter and the aggregation therefore cannot be shared; the **pager** can, and
//! is, so the bound on a wide window is defined once
//! ([`crate::services::pullers::read_all`]).
//!
//! Exposure in RFC 0002's sense, like its sibling: an allowed download delivered
//! bytes and is counted, a refusal did not and is not. An auditor asking what an
//! account received is not asking what it was refused.
//!
//! Anonymous pulls keep the `ip:<addr>` grouping, so an `--identity ip:10.0.0.7`
//! is a question this can answer — which is often the only handle an incident
//! has on a CI runner that presents no credential.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::entities::{AccessAction, AccessEvent, AccessResult, EventFilter};
use crate::error::CoreError;
use crate::ports::PackageRepository;
use crate::services::pullers::{identity_of, read_all};

/// One coordinate an identity pulled inside the window.
///
/// `user_agent` and `source_ip` are **last seen**, not a distinct set. Both are
/// multi-valued across a window — a runner is redeployed, an agent is upgraded —
/// and RFC 0018 §4.2 names them singular. The last value is the one an incident
/// acts on ("where is it pulling from *now*"); the full set is a different
/// report, and pretending one field carries it would be worse than saying which
/// one this is.
///
/// Both are recorded on **every** delivered download, local and proxied alike:
/// the proxy path threads them through `ProxyRequest`, and the local read path
/// takes a `CallerNet` down to `LocalRegistryService::record_download`. They were
/// empty for a local or hybrid registry's own downloads until that second half
/// landed, which is why an old row can still carry a count and no values —
/// nothing backfills a column that was never written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Pull {
    pub registry: String,
    pub package_name: String,
    /// Absent when the event recorded no version — a whole-package coordinate.
    pub version: Option<String>,
    pub first_pull: DateTime<Utc>,
    pub last_pull: DateTime<Utc>,
    pub count: u64,
    /// The User-Agent of the most recent pull, when one was recorded.
    pub client_user_agent: Option<String>,
    /// The address of the most recent pull, when one was recorded.
    pub source_ip: Option<String>,
}

/// The filter: this identity's downloads since `since`, optionally narrowed to
/// one registry or one package name.
///
/// `user_id` is only set for a real account. An `ip:<addr>` identity has no
/// `user_id` column to match, so it is filtered after the read by
/// [`identity_of`] — the alternative is a repository method for a shape the
/// database does not index anyway.
pub fn pulls_filter(
    identity: &str,
    since: DateTime<Utc>,
    registry: Option<&str>,
    package: Option<&str>,
) -> EventFilter {
    EventFilter {
        registry: registry.map(str::to_owned),
        package_name: package.map(str::to_owned),
        user_id: (!identity.starts_with("ip:") && identity != "anonymous")
            .then(|| identity.to_owned()),
        actions: vec![AccessAction::Download],
        from: Some(since),
        ..EventFilter::new()
    }
}

/// Group one identity's events by coordinate, newest pull first.
pub fn aggregate(events: &[AccessEvent], identity: &str) -> Vec<Pull> {
    // Keyed by the coordinate rather than by a formatted string, so two
    // packages whose names differ only by registry never merge.
    let mut by_coordinate: BTreeMap<(String, String, Option<String>), Pull> = BTreeMap::new();

    for e in events {
        if !matches!(e.result, AccessResult::Allowed) {
            continue;
        }
        if identity_of(e) != identity {
            continue;
        }
        let Some(id) = &e.package_id else {
            // An event with no coordinate is an account-wide admin action, not
            // a pull; `actions` already excludes those, so this is belt.
            continue;
        };
        let version = (!id.version.is_empty()).then(|| id.version.clone());
        let key = (id.registry.clone(), id.name.clone(), version.clone());
        by_coordinate
            .entry(key)
            .and_modify(|p| {
                p.count += 1;
                if e.timestamp < p.first_pull {
                    p.first_pull = e.timestamp;
                }
                if e.timestamp >= p.last_pull {
                    p.last_pull = e.timestamp;
                    // Last seen: only the newer event replaces them, and an
                    // event that recorded neither does not blank what an
                    // earlier one knew.
                    if e.user_agent.is_some() {
                        p.client_user_agent = e.user_agent.clone();
                    }
                    if e.ip_address.is_some() {
                        p.source_ip = e.ip_address.clone();
                    }
                }
            })
            .or_insert_with(|| Pull {
                registry: id.registry.clone(),
                package_name: id.name.clone(),
                version,
                first_pull: e.timestamp,
                last_pull: e.timestamp,
                count: 1,
                client_user_agent: e.user_agent.clone(),
                source_ip: e.ip_address.clone(),
            });
    }

    let mut out: Vec<Pull> = by_coordinate.into_values().collect();
    out.sort_by(|a, b| {
        b.last_pull
            .cmp(&a.last_pull)
            .then(a.registry.cmp(&b.registry))
            .then(a.package_name.cmp(&b.package_name))
            .then(a.version.cmp(&b.version))
    });
    out
}

/// What `identity` pulled since `since`.
pub async fn pulls_for(
    repo: &dyn PackageRepository,
    identity: &str,
    since: DateTime<Utc>,
    registry: Option<&str>,
    package: Option<&str>,
) -> Result<Vec<Pull>, CoreError> {
    let events = read_all(repo, pulls_filter(identity, since, registry, package)).await?;
    Ok(aggregate(&events, identity))
}

/// The CSV the endpoint and the CLI print: one header, one row per coordinate.
pub fn to_csv(rows: &[Pull]) -> String {
    use crate::services::csv::field;
    let mut out = String::from(
        "registry,package_name,version,first_pull,last_pull,count,client_user_agent,source_ip\n",
    );
    for r in rows {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{}\n",
            field(&r.registry),
            field(&r.package_name),
            field(r.version.as_deref().unwrap_or("")),
            r.first_pull.to_rfc3339(),
            r.last_pull.to_rfc3339(),
            r.count,
            field(r.client_user_agent.as_deref().unwrap_or("")),
            field(r.source_ip.as_deref().unwrap_or("")),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{PackageId, Role};

    fn pull_event(
        user: Option<&str>,
        ip: Option<&str>,
        registry: &str,
        name: &str,
        version: &str,
        ago_secs: i64,
    ) -> AccessEvent {
        let mut e = AccessEvent::allowed_download(
            PackageId::new(registry, name, version),
            user.map(str::to_owned),
            Role::User,
        );
        e.ip_address = ip.map(str::to_owned);
        e.timestamp = Utc::now() - chrono::Duration::seconds(ago_secs);
        e
    }

    /// The transpose: one identity, several coordinates, newest first. The
    /// grouping is by coordinate where `pullers` groups by identity, and that
    /// inversion is the whole reason this module exists.
    #[test]
    fn one_identity_groups_by_coordinate_newest_first() {
        let events = vec![
            pull_event(Some("ci-bot"), None, "npm", "left-pad", "1.3.1", 300),
            pull_event(Some("ci-bot"), None, "npm", "left-pad", "1.3.1", 10),
            pull_event(Some("ci-bot"), None, "npm", "lodash", "4.17.21", 200),
            pull_event(Some("someone-else"), None, "npm", "chalk", "5.0.0", 5),
        ];
        let rows = aggregate(&events, "ci-bot");

        assert_eq!(rows.len(), 2, "another identity's pull leaked in: {rows:?}");
        assert_eq!(rows[0].package_name, "left-pad", "newest last pull first");
        assert_eq!(rows[0].count, 2);
        assert!(rows[0].first_pull < rows[0].last_pull);
    }

    /// Two registries can serve the same package name. Keying on the whole
    /// coordinate is what stops them merging into one row.
    #[test]
    fn the_same_name_in_two_registries_stays_two_rows() {
        let events = vec![
            pull_event(Some("ci-bot"), None, "npm-pub", "left-pad", "1.3.1", 30),
            pull_event(Some("ci-bot"), None, "npm-priv", "left-pad", "1.3.1", 20),
        ];
        assert_eq!(aggregate(&events, "ci-bot").len(), 2);
    }

    /// Exposure is what was delivered. A refusal transferred no bytes and is a
    /// different report — the same rule `pullers` follows.
    #[test]
    fn a_refusal_is_not_a_pull() {
        let mut denied = pull_event(Some("ci-bot"), None, "npm", "left-pad", "1.3.1", 5);
        denied.result = AccessResult::Denied {
            reason: "held".into(),
        };
        assert!(aggregate(&[denied], "ci-bot").is_empty());
    }

    /// An anonymous caller is addressable as `ip:<addr>`, which is often the
    /// only handle an incident has on a runner that presents no credential.
    #[test]
    fn an_anonymous_identity_is_addressable_by_address() {
        let events = vec![pull_event(
            None,
            Some("10.0.0.7"),
            "npm",
            "left-pad",
            "1.3.1",
            5,
        )];
        let rows = aggregate(&events, "ip:10.0.0.7");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_ip.as_deref(), Some("10.0.0.7"));
    }

    /// `user_agent` and `source_ip` are last seen, and an event that recorded
    /// neither must not blank what an earlier one knew.
    #[test]
    fn the_agent_and_address_are_last_seen_and_never_blanked() {
        let mut old = pull_event(
            Some("ci-bot"),
            Some("10.0.0.1"),
            "npm",
            "left-pad",
            "1.3.1",
            300,
        );
        old.user_agent = Some("npm/9".into());
        let mut newer = pull_event(
            Some("ci-bot"),
            Some("10.0.0.2"),
            "npm",
            "left-pad",
            "1.3.1",
            100,
        );
        newer.user_agent = Some("npm/10".into());
        // Newest of all, but it recorded neither field.
        let blank = pull_event(Some("ci-bot"), None, "npm", "left-pad", "1.3.1", 10);

        let rows = aggregate(&[old, newer, blank], "ci-bot");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].client_user_agent.as_deref(), Some("npm/10"));
        assert_eq!(rows[0].source_ip.as_deref(), Some("10.0.0.2"));
        assert_eq!(rows[0].count, 3);
    }

    /// A real account filters in the database; an `ip:` identity cannot, so the
    /// filter must not invent a `user_id` the column would never match.
    #[test]
    fn only_a_real_account_becomes_a_user_id_filter() {
        let since = Utc::now();
        assert_eq!(
            pulls_filter("alice", since, None, None).user_id.as_deref(),
            Some("alice")
        );
        assert!(pulls_filter("ip:10.0.0.7", since, None, None)
            .user_id
            .is_none());
        assert!(pulls_filter("anonymous", since, None, None)
            .user_id
            .is_none());
        // And it only ever asks for downloads.
        assert_eq!(
            pulls_filter("alice", since, None, None).actions,
            vec![AccessAction::Download]
        );
    }

    #[test]
    fn the_csv_quotes_what_needs_quoting() {
        let rows = aggregate(
            &[{
                let mut e = pull_event(Some("ci-bot"), None, "npm", "a,b", "1.0.0", 5);
                e.user_agent = Some("agent\"quoted".into());
                e
            }],
            "ci-bot",
        );
        let csv = to_csv(&rows);
        assert!(csv.starts_with("registry,package_name,version,"), "{csv}");
        assert!(csv.contains("\"a,b\""), "a comma must be quoted: {csv}");
        assert!(
            csv.contains("\"agent\"\"quoted\""),
            "a quote must be doubled: {csv}"
        );
    }
}
