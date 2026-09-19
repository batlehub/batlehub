//! Ansible Galaxy: the collection versions list, the collection document and
//! the v1 role versions list (RFC 0031 §6.2).
//!
//! The chokepoint is the **versions list**. `ansible-galaxy`'s dependency
//! resolver asks `get_collection_versions` for every candidate — including a
//! requirement pinned to one exact version — and picks from what comes back, so
//! a version absent from the list cannot be selected: the client prints
//! *"Failed to resolve the requested dependencies map"* and stops before any
//! metadata or artifact request. Both halves of "a blocked version is
//! unreachable" already exist in the client.
//!
//! The **collection document** is repaired by the handler, not here, for the
//! reason Go's `@latest` and a RubyGems gem document are: it names one version
//! (`highest_version`) and carries no list to pick a replacement from, so the
//! repair needs the versions list as well and the dispatch cannot fetch it.
//! [`repair_collection`] is that repair, called from the handler that holds both
//! documents.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::{best_latest, BlockedVersions};

/// Remove every blocked version from a v3 versions listing, in place, and
/// rewrite `meta.count` to describe what is served.
///
/// `data` is the key this instance emits, and `results` is accepted because a
/// standalone `pulp_ansible` upstream spells it that way — the same two keys
/// the client itself accepts.
///
/// `links` is not touched here: the assembled document has already been
/// collapsed to one page by `services::galaxy::collapse_to_one_page`, which
/// runs whether or not anything is blocked. A `links.next` is an absolute path,
/// and `urljoin`'d against a path-prefixed api_server it resolves to the root of
/// the host — so no continuation this instance emits could be followed back to
/// it, blocked versions or not.
pub fn strip_versions(doc: &mut Value, blocked: &BlockedVersions) -> Vec<String> {
    if blocked.is_empty() {
        return Vec::new();
    }
    let Some(obj) = doc.as_object_mut() else {
        return Vec::new();
    };
    let key = if obj.contains_key("data") {
        "data"
    } else {
        "results"
    };
    let Some(entries) = obj.get_mut(key).and_then(Value::as_array_mut) else {
        return Vec::new();
    };

    let mut removed = Vec::new();
    entries.retain(|entry| {
        let Some(version) = entry.get("version").and_then(Value::as_str) else {
            // An entry with no version names nothing to hide, and dropping it
            // would remove a row the operator never blocked.
            return true;
        };
        if blocked.contains(version) {
            removed.push(version.to_owned());
            false
        } else {
            true
        }
    });
    if removed.is_empty() {
        return removed;
    }

    let count = entries.len();
    if let Some(meta) = obj.get_mut("meta").and_then(Value::as_object_mut) {
        meta.insert("count".to_owned(), json!(count));
    }
    removed
}

/// Remove every blocked version from a v1 role versions listing, in place.
///
/// The v1 surface has only ever used `results`, and its pagination field is
/// `next_link` rather than `links.next`. Both are nulled: `fetch_role_related`
/// joins `next_link` against a deliberately stripped `scheme://netloc/` — the
/// fix for ansible issue 64355 — so a relayed link cannot survive a path prefix
/// at all, and a *fully qualified* one would not survive either.
pub fn strip_role_versions(doc: &mut Value, blocked: &BlockedVersions) -> Vec<String> {
    if blocked.is_empty() {
        return Vec::new();
    }
    let Some(obj) = doc.as_object_mut() else {
        return Vec::new();
    };
    let Some(entries) = obj.get_mut("results").and_then(Value::as_array_mut) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    entries.retain(|entry| {
        // A role version is `name` on this document, not `version`: the v1 API
        // predates the v3 spelling and never adopted it.
        let version = entry
            .get("version")
            .and_then(Value::as_str)
            .or_else(|| entry.get("name").and_then(Value::as_str));
        let Some(version) = version else {
            return true;
        };
        if blocked.contains(version) {
            removed.push(version.to_owned());
            false
        } else {
            true
        }
    });
    if removed.is_empty() {
        return removed;
    }
    let count = entries.len();
    obj.insert("count".to_owned(), json!(count));
    removed
}

/// Null every pagination field of a v1 listing, whether or not anything was
/// blocked.
///
/// The counterpart of `services::galaxy::collapse_to_one_page` for the v1
/// documents, and separate from the filter for the same reason: a relayed
/// `next_link` is broken by the client's own URL joining regardless of policy.
pub fn collapse_role_page(doc: &mut Value) {
    let Some(obj) = doc.as_object_mut() else {
        return;
    };
    let count = obj
        .get("results")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    obj.insert("count".to_owned(), json!(count));
    for key in ["next", "next_link", "previous", "previous_link"] {
        if obj.contains_key(key) {
            obj.insert(key.to_owned(), Value::Null);
        }
    }
}

/// What [`repair_collection`] did, so the handler can log it and the tests can
/// assert on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CollectionRepair {
    /// `highest_version` named a blocked version and was moved.
    pub highest_moved: bool,
    /// `updated_at` was bumped past the upstream's own value.
    pub updated_at_bumped: bool,
}

/// Repair a collection document against the versions that survived filtering.
///
/// Two edits, and the second one is the reason the first is worth making:
///
/// - **`highest_version`** is repaired to the newest surviving version when the
///   named one was removed, the way npm's `dist-tags.latest` is. A document
///   naming a version its own listing no longer carries is a document the
///   resolver will chase to a `404`.
/// - **`updated_at`** is served as `max(upstream, newest blocked_at)`.
///   `get_collection_versions` re-reads this document on
///   every call — uncached, unlike the versions list itself — and drops its
///   cached copy of the list when `updated_at` differs from the value it
///   recorded beside it. Without the bump, a newly blocked version stays in a
///   warm client's list for up to a day, the resolver picks it, and the install
///   fails on a `404` at the version document instead of quietly resolving to
///   an allowed version. *Enforcement holds either way* — the version document
///   and the tarball are both refused — so this is an error-quality mechanism,
///   not a security one.
///
/// `changed_at` is the newest `blocked_at` among this collection's block rows.
/// `None` means the store could not say, and then only the `highest_version`
/// repair runs: an invented timestamp would be a claim about when the document
/// changed that nothing supports.
pub fn repair_collection(
    doc: &mut Value,
    blocked: &BlockedVersions,
    surviving: &[String],
    changed_at: Option<DateTime<Utc>>,
) -> CollectionRepair {
    let mut repair = CollectionRepair::default();
    if blocked.is_empty() {
        return repair;
    }
    let Some(obj) = doc.as_object_mut() else {
        return repair;
    };

    let named = obj
        .get("highest_version")
        .and_then(|h| h.get("version"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let highest_is_blocked = named.as_deref().is_some_and(|v| blocked.contains(v));

    if highest_is_blocked {
        match best_latest(surviving) {
            Some(best) => {
                let href = obj
                    .get("highest_version")
                    .and_then(|h| h.get("href"))
                    .and_then(Value::as_str)
                    .zip(named.as_deref())
                    // The href is the version document's, so the version in it
                    // moves with the version it names.
                    .map(|(href, old)| href.replace(old, &best));
                let mut entry = serde_json::Map::new();
                if let Some(href) = href {
                    entry.insert("href".to_owned(), json!(href));
                }
                entry.insert("version".to_owned(), json!(best));
                obj.insert("highest_version".to_owned(), Value::Object(entry));
            }
            None => {
                // Every version is blocked. A `highest_version` naming
                // something the listing does not carry is worse than none, and
                // the listing is empty anyway.
                obj.insert("highest_version".to_owned(), Value::Null);
            }
        }
        repair.highest_moved = true;
    }

    // The bump is `max(upstream updated_at, newest blocked_at)`, applied
    // whenever this collection carries a block row at all — not only when
    // filtering removed something.
    //
    // Deliberately unconditional, and stable because of it: `blocked_at` is a
    // fixed instant, so the served value moves *once* when a block is written
    // and then stays put. Conditioning on "something was removed" would need
    // the unfiltered listing as well as the filtered one, and would buy
    // nothing: a block on a version this collection never had bumps the
    // document to a timestamp that never moves again, which costs one extra
    // listing read, once.
    if let Some(changed_at) = changed_at {
        let current = obj
            .get("updated_at")
            .and_then(Value::as_str)
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|dt| dt.with_timezone(&Utc));
        if current.is_none_or(|c| c < changed_at) {
            let stamp = changed_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            obj.insert("updated_at".to_owned(), json!(stamp));
            repair.updated_at_bumped = true;
        }
    }
    repair
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::RegistryKind;

    fn blocks(versions: &[&str]) -> BlockedVersions {
        BlockedVersions::new(
            RegistryKind::Galaxy,
            versions.iter().map(|v| (*v).to_owned()).collect(),
        )
    }

    fn listing(versions: &[&str]) -> Value {
        json!({
            "meta": { "count": versions.len() },
            "links": { "first": null, "previous": null, "next": null, "last": null },
            "data": versions.iter().map(|v| json!({
                "version": v,
                "href": format!("https://h/v3/collections/c/g/versions/{v}/"),
                "created_at": "2026-01-01T00:00:00Z",
            })).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn a_blocked_version_is_absent_and_the_count_follows() {
        let mut doc = listing(&["1.0.0", "1.1.0", "2.0.0"]);
        let removed = strip_versions(&mut doc, &blocks(&["1.1.0"]));
        assert_eq!(removed, vec!["1.1.0".to_owned()]);
        assert_eq!(doc["meta"]["count"], 2);
        let served: Vec<&str> = doc["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["version"].as_str().unwrap())
            .collect();
        assert_eq!(served, ["1.0.0", "2.0.0"]);
    }

    #[test]
    fn a_results_keyed_upstream_is_filtered_the_same_way() {
        let mut doc = json!({
            "meta": { "count": 2 },
            "results": [ {"version": "1.0.0"}, {"version": "1.1.0"} ],
        });
        let removed = strip_versions(&mut doc, &blocks(&["1.0.0"]));
        assert_eq!(removed, vec!["1.0.0".to_owned()]);
        assert_eq!(doc["results"].as_array().unwrap().len(), 1);
        assert_eq!(doc["meta"]["count"], 1);
    }

    #[test]
    fn nothing_blocked_removes_nothing() {
        let mut doc = listing(&["1.0.0", "1.1.0"]);
        let before = doc.clone();
        assert!(strip_versions(&mut doc, &blocks(&["9.9.9"])).is_empty());
        assert_eq!(doc, before);
    }

    #[test]
    fn an_empty_block_set_is_a_no_op() {
        let mut doc = listing(&["1.0.0"]);
        let before = doc.clone();
        assert!(strip_versions(&mut doc, &blocks(&[])).is_empty());
        assert_eq!(doc, before);
    }

    #[test]
    fn the_highest_version_moves_to_the_newest_survivor() {
        let mut doc = json!({
            "namespace": "community",
            "name": "general",
            "updated_at": "2026-01-01T00:00:00Z",
            "highest_version": {
                "href": "https://h/v3/collections/community/general/versions/13.4.0/",
                "version": "13.4.0",
            },
        });
        let changed = "2026-02-01T09:30:00Z".parse::<DateTime<Utc>>().unwrap();
        let repair = repair_collection(
            &mut doc,
            &blocks(&["13.4.0"]),
            &["13.2.0".to_owned(), "13.3.0".to_owned()],
            Some(changed),
        );
        assert!(repair.highest_moved);
        assert!(repair.updated_at_bumped);
        assert_eq!(doc["highest_version"]["version"], "13.3.0");
        assert_eq!(
            doc["highest_version"]["href"],
            "https://h/v3/collections/community/general/versions/13.3.0/"
        );
        assert_eq!(doc["updated_at"], "2026-02-01T09:30:00Z");
    }

    #[test]
    fn a_block_on_a_version_this_collection_never_had_leaves_highest_version_alone() {
        let mut doc = json!({
            "updated_at": "2026-01-01T00:00:00Z",
            "highest_version": { "version": "13.4.0" },
        });
        let repair = repair_collection(
            &mut doc,
            &blocks(&["0.0.1"]),
            &["13.4.0".to_owned()],
            Some("2026-02-01T00:00:00Z".parse().unwrap()),
        );
        assert!(!repair.highest_moved);
        assert_eq!(doc["highest_version"]["version"], "13.4.0");
        // The bump still happens, and lands on a fixed instant: the served
        // value moves once and then stays put.
        assert!(repair.updated_at_bumped);
        assert_eq!(doc["updated_at"], "2026-02-01T00:00:00Z");
    }

    #[test]
    fn the_bump_is_idempotent() {
        let mut doc = json!({ "updated_at": "2026-01-01T00:00:00Z", "highest_version": null });
        let at: DateTime<Utc> = "2026-02-01T00:00:00Z".parse().unwrap();
        assert!(repair_collection(&mut doc, &blocks(&["1.0.0"]), &[], Some(at)).updated_at_bumped);
        let once = doc.clone();
        assert!(!repair_collection(&mut doc, &blocks(&["1.0.0"]), &[], Some(at)).updated_at_bumped);
        assert_eq!(doc, once);
    }

    #[test]
    fn no_timestamp_from_the_store_means_no_invented_one() {
        let mut doc = json!({ "updated_at": "2026-01-01T00:00:00Z", "highest_version": { "version": "2.0.0" } });
        let repair = repair_collection(&mut doc, &blocks(&["2.0.0"]), &["1.0.0".to_owned()], None);
        assert!(repair.highest_moved);
        assert!(!repair.updated_at_bumped);
        assert_eq!(doc["updated_at"], "2026-01-01T00:00:00Z");
    }

    #[test]
    fn an_upstream_newer_than_the_block_keeps_its_own_timestamp() {
        let mut doc = json!({
            "updated_at": "2026-03-01T00:00:00Z",
            "highest_version": { "version": "2.0.0" },
        });
        let repair = repair_collection(
            &mut doc,
            &blocks(&["2.0.0"]),
            &["1.0.0".to_owned()],
            Some("2026-02-01T00:00:00Z".parse().unwrap()),
        );
        assert!(repair.highest_moved);
        assert!(!repair.updated_at_bumped);
        assert_eq!(doc["updated_at"], "2026-03-01T00:00:00Z");
    }

    #[test]
    fn every_version_blocked_leaves_no_highest_version() {
        let mut doc = json!({ "highest_version": { "version": "1.0.0" } });
        repair_collection(&mut doc, &blocks(&["1.0.0"]), &[], None);
        assert!(doc["highest_version"].is_null());
    }

    #[test]
    fn a_role_versions_listing_is_filtered_and_collapsed() {
        let mut doc = json!({
            "count": 3,
            "next": "/api/v1/roles/1/versions/?page=2",
            "next_link": "/api/v1/roles/1/versions/?page=2",
            "results": [
                {"name": "1.0.0", "download_url": "https://github.com/x/y/archive/1.0.0.tar.gz"},
                {"name": "2.0.0", "download_url": "https://github.com/x/y/archive/2.0.0.tar.gz"},
                {"name": "3.0.0", "download_url": "https://github.com/x/y/archive/3.0.0.tar.gz"},
            ],
        });
        let removed = strip_role_versions(&mut doc, &blocks(&["2.0.0"]));
        assert_eq!(removed, vec!["2.0.0".to_owned()]);
        collapse_role_page(&mut doc);
        assert_eq!(doc["count"], 2);
        assert!(doc["next"].is_null());
        assert!(doc["next_link"].is_null());
    }
}
