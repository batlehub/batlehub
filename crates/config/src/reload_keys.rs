//! Which config keys a hot reload applies (RFC 0037 §4.3).
//!
//! [`classify`] diffs two documents as [`crate::merge_layers`] returns them
//! and sorts every changed path by the most specific row of [`RELOAD_KEYS`]
//! that matches it. A path no row matches needs a restart: a section added to
//! `AppConfig` tomorrow is restart-only until someone traces a reader, which is
//! the safe way to be wrong.

use anyhow::{Context, Result};
use toml::Value;

/// When a changed key takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Every reader sees the new value once the reload is applied.
    Hot,
    /// Every request sees it; a component built once at startup keeps the
    /// old value until a restart.
    Partial,
    /// Nothing reads it again before a restart.
    Restart,
}

/// The reloadable keys, traced from `make_hot_builder` and `build_hot_bundle`
/// (`server/src/hot_config.rs`), `build_pending` (`batlehub-web`'s reload
/// service) and the startup wiring in `server/src/main.rs` and `setup.rs`:
/// `(pattern, tier, who reads it)`. The third column is what
/// `docs/guide/hot-reload.md` prints, kept honest by a test below.
///
/// `*` matches one key, `[*]` one array entry. The most specific matching
/// row wins (more segments, then fewer wildcards), so the order here means
/// nothing; no match is [`Tier::Restart`].
pub const RELOAD_KEYS: &[(&str, Tier, &str)] = &[
    ("server.trusted_proxies", Tier::Hot, "the proxy-trust policy, swapped together with the host routing table"),
    ("ip_blocking.trusted_proxies", Tier::Hot, "the proxy-trust policy, as the deprecated fallback of `server.trusted_proxies`"),
    ("server.signed_urls", Tier::Hot, "the download URL signer"),
    ("grants", Tier::Hot, "the instance-tier grants"),
    ("limits.versions_per_page", Tier::Hot, "listing pagination"),
    ("limits.packages_per_page", Tier::Hot, "listing pagination"),
    ("subdomain_routing", Tier::Hot, "the host → registry routing table"),
    ("search.readmes", Tier::Hot, "the prose-search switch"),
    ("scanners.*.escalation", Tier::Hot, "each registry's security policy"),
    ("registries[*]", Tier::Partial, "the registry clients, policies and maps; warming and release imports keep the startup client"),
    ("registries[*].rbac", Tier::Hot, "the registry's grant hierarchy"),
    ("registries[*].grants", Tier::Hot, "the registry's grant hierarchy"),
    ("registries[*].rules", Tier::Hot, "the registry's rule chain"),
    ("registries[*].hosts", Tier::Hot, "the host → registry routing table"),
    ("registries[*].path_routing", Tier::Hot, "the host → registry routing table"),
    ("registries[*].cache.metadata_ttl_secs", Tier::Partial, "the registry's metadata policy; warming keeps the startup client"),
    ("registries[*].cache.serve_stale", Tier::Hot, "the registry's metadata policy"),
    ("registries[*].cache.artifact_ttl_secs", Tier::Partial, "the registry's download policy; eviction keeps the startup TTL"),
    ("registries[*].security.rescan", Tier::Restart, "the rescan scheduler, started at startup only when a registry already had one"),
    ("registries[*].rate_limit", Tier::Restart, "the rate limiter, built once"),
    ("registries[*].quota", Tier::Restart, "the quota service, built once"),
    ("registries[*].storage", Tier::Restart, "the storage router, built once"),
    ("registries[*].cache", Tier::Restart, "the warming and eviction services, built once"),
    ("proxy", Tier::Partial, "the registry clients and cargo index; the README fetcher and warming keep the startup proxy"),
    ("air_gap", Tier::Partial, "the registry clients' air-gap wrapper; warming and release imports keep the startup clients"),
    ("limits.max_artifact_size_bytes", Tier::Partial, "the request path; the scan worker keeps the startup limit"),
];

/// The changed paths between two documents, by tier. Each list is sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Changed paths a hot reload applies everywhere.
    pub hot: Vec<String>,
    /// Changed paths the request path applies and a startup-built
    /// background component does not.
    pub partial: Vec<String>,
    /// Changed paths that take effect only after a restart.
    pub restart: Vec<String>,
}

impl Verdict {
    pub fn is_empty(&self) -> bool {
        self.hot.is_empty() && self.partial.is_empty() && self.restart.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    Key(String),
    Entry(String),
}

/// Diff `before` and `after` — two documents as `merge_layers` returns them —
/// and sort every changed path into its tier.
///
/// Paths are dotted, an array-of-tables entry named by its identity key:
/// `registries[npm].cache.idle_days`. An entry present on one side only is one
/// path, `registries[npm]`, plus each restart-only key an added entry
/// carries — a component built at startup never sees the new entry's
/// `rate_limit`. Reordering a keyed array is a change at the array's path,
/// except `registries`, whose order nothing reads. Placeholders compare as
/// text, and a key written out at its default value is a change: both err
/// towards a restart.
pub fn classify(before: &str, after: &str) -> Result<Verdict> {
    // `merge_layers` writes a placeholder as `${NAME}` and a literal `$` as
    // `\u0024`, and TOML decodes both to the same text. Its output has no
    // other `${`, so reading each one back as the marker it was keeps
    // `"${X}"` and `"$${X}"` two different values.
    let parse = |doc: &str| {
        toml::from_str::<Value>(&doc.replace("${", &crate::render::MARK_OPEN.to_string()))
    };
    let before = parse(before).context("parsing the document before")?;
    let after = parse(after).context("parsing the document after")?;
    let mut paths = Vec::new();
    diff(Some(&before), Some(&after), &mut Vec::new(), &mut paths);
    let mut verdict = Verdict::default();
    for path in paths {
        let list = match tier(&path) {
            Tier::Hot => &mut verdict.hot,
            Tier::Partial => &mut verdict.partial,
            Tier::Restart => &mut verdict.restart,
        };
        list.push(render_path(&path));
    }
    verdict.hot.sort();
    verdict.partial.sort();
    verdict.restart.sort();
    Ok(verdict)
}

/// The tier of a dotted path as [`classify`] writes it.
pub fn tier_of(path: &str) -> Tier {
    tier(&parse_path(path))
}

fn tier(path: &[Seg]) -> Tier {
    RELOAD_KEYS
        .iter()
        .filter_map(|(pattern, tier, _)| {
            let pattern = parse_path(pattern);
            let matches = pattern.len() <= path.len()
                && pattern.iter().zip(path).all(|(p, s)| match (p, s) {
                    (Seg::Key(p), Seg::Key(s)) | (Seg::Entry(p), Seg::Entry(s)) => {
                        p == "*" || p == s
                    }
                    _ => false,
                });
            let wildcards = pattern
                .iter()
                .filter(|s| matches!(s, Seg::Key(k) | Seg::Entry(k) if k == "*"))
                .count();
            // Ranked by (length, fewer wildcards): `Reverse` so `max` prefers fewer.
            matches.then_some(((pattern.len(), std::cmp::Reverse(wildcards)), *tier))
        })
        .max_by_key(|(rank, _)| *rank)
        .map_or(Tier::Restart, |(_, tier)| tier)
}

/// The inverse of [`render_path`]. An entry name may hold a `.` — a registry
/// called `npm.internal` — so brackets are read before dots.
fn parse_path(path: &str) -> Vec<Seg> {
    let mut segs = Vec::new();
    let mut rest = path;
    while !rest.is_empty() {
        if let Some(inner) = rest.strip_prefix('[') {
            let close = inner.find(']').unwrap_or(inner.len());
            segs.push(Seg::Entry(inner[..close].to_owned()));
            rest = inner.get(close + 1..).unwrap_or("");
        } else {
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            segs.push(Seg::Key(rest[..end].to_owned()));
            rest = &rest[end..];
        }
        rest = rest.strip_prefix('.').unwrap_or(rest);
    }
    segs
}

fn render_path(path: &[Seg]) -> String {
    let mut out = String::new();
    for seg in path {
        match seg {
            Seg::Key(k) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(k);
            }
            Seg::Entry(id) => {
                out.push('[');
                out.push_str(id);
                out.push(']');
            }
        }
    }
    out
}

fn tables(value: Option<&Value>) -> Option<Vec<Value>> {
    match value {
        None => Some(Vec::new()),
        Some(Value::Array(a)) if a.iter().all(Value::is_table) => Some(a.clone()),
        _ => None,
    }
}

fn diff(a: Option<&Value>, b: Option<&Value>, path: &mut Vec<Seg>, out: &mut Vec<Vec<Seg>>) {
    if a == b {
        return;
    }
    let empty = toml::Table::new();
    match (a, b) {
        // A table that appears or disappears empty is still a change: an empty
        // `[otel]` turns an `Option` section on.
        (Some(Value::Table(t)), None) | (None, Some(Value::Table(t))) if t.is_empty() => {
            out.push(path.clone());
        }
        (Some(Value::Table(_)) | None, Some(Value::Table(_)) | None) => {
            let ta = a.and_then(Value::as_table).unwrap_or(&empty);
            let tb = b.and_then(Value::as_table).unwrap_or(&empty);
            let mut keys: Vec<&String> = ta.keys().chain(tb.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                path.push(Seg::Key(k.clone()));
                diff(ta.get(k), tb.get(k), path, out);
                path.pop();
            }
        }
        _ => {
            let keyed = tables(a)
                .zip(tables(b))
                // Not the merge's `array_identity_key`, which gives up on an
                // empty side — the merge's "`x = []` clears" — and would make
                // the first registry ever added a whole-array change.
                .and_then(|(ea, eb)| crate::identity_key(&ea, &eb).map(|key| (key, ea, eb)));
            let Some((key, ea, eb)) = keyed else {
                out.push(path.clone());
                return;
            };
            if ea.is_empty() && eb.is_empty() {
                // `x = []` against no `x` at all: no entry to name, but an
                // `Option` field reads the two differently.
                out.push(path.clone());
                return;
            }
            let id = |e: &Value| e.get(key).and_then(Value::as_str).map(str::to_owned);
            // The order of `[[auth]]` decides which provider answers first;
            // the order of `[[registries]]` decides nothing.
            if *path != [Seg::Key("registries".to_owned())] {
                let shared = |side: &[Value], other: &[Value]| -> Vec<String> {
                    let others: Vec<String> = other.iter().filter_map(id).collect();
                    side.iter()
                        .filter_map(id)
                        .filter(|i| others.contains(i))
                        .collect()
                };
                if shared(&ea, &eb) != shared(&eb, &ea) {
                    out.push(path.clone());
                }
            }
            let mut ids: Vec<String> = ea.iter().chain(&eb).filter_map(id).collect();
            ids.sort();
            ids.dedup();
            for name in ids {
                let find = |side: &[Value]| {
                    side.iter()
                        .find(|e| id(e).as_deref() == Some(&name))
                        .cloned()
                };
                let (x, y) = (find(&ea), find(&eb));
                path.push(Seg::Entry(name));
                if x.is_some() && y.is_some() {
                    diff(x.as_ref(), y.as_ref(), path, out);
                } else {
                    out.push(path.clone());
                    if x.is_none() {
                        let mut inner = Vec::new();
                        diff(None, y.as_ref(), path, &mut inner);
                        out.extend(inner.into_iter().filter(|p| tier(p) == Tier::Restart));
                    }
                }
                path.pop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(before: &str, after: &str) -> Verdict {
        classify(before, after).unwrap()
    }

    fn v(hot: &[&str], partial: &[&str], restart: &[&str]) -> Verdict {
        let own = |l: &[&str]| l.iter().map(|s| s.to_string()).collect();
        Verdict {
            hot: own(hot),
            partial: own(partial),
            restart: own(restart),
        }
    }

    #[test]
    fn every_row_sorts_its_own_path() {
        for (pattern, expected, _) in RELOAD_KEYS {
            let path = pattern.replace("[*]", "[x]").replace(".*.", ".x.");
            assert_eq!(tier_of(&path), *expected, "{pattern}");
            assert_eq!(
                tier_of(&format!("{path}.leaf")),
                *expected,
                "{pattern}.leaf"
            );
        }
    }

    #[test]
    fn the_most_specific_row_wins() {
        assert_eq!(
            tier_of("registries[x].cache.metadata_ttl_secs"),
            Tier::Partial
        );
        assert_eq!(tier_of("registries[x].cache.idle_days"), Tier::Restart);
        assert_eq!(tier_of("registries[x].upstreams"), Tier::Partial);
        assert_eq!(tier_of("registries[x]"), Tier::Partial);
        assert_eq!(tier_of("limits.max_versions"), Tier::Restart);
        assert_eq!(tier_of("scanners.osv.command"), Tier::Restart);
    }

    #[test]
    fn an_unknown_section_is_restart() {
        assert_eq!(
            verdict("", "[brand_new]\nk = 1\n"),
            v(&[], &[], &["brand_new.k"])
        );
        assert_eq!(
            verdict("[database]\nurl = \"a\"\n", "[database]\nurl = \"b\"\n"),
            v(&[], &[], &["database.url"])
        );
    }

    #[test]
    fn reordering_registries_is_no_change() {
        let a = "[[registries]]\nname = \"a\"\n[[registries]]\nname = \"b\"\n";
        let b = "[[registries]]\nname = \"b\"\n[[registries]]\nname = \"a\"\n";
        assert!(verdict(a, b).is_empty());
        assert!(verdict(a, a).is_empty());
    }

    /// Added, the entry also names the restart-only keys it carries: the
    /// rate limiter built at startup never learns the new registry's limit.
    #[test]
    fn a_registry_added_or_removed_is_one_partial_path() {
        let one =
            "[[registries]]\nname = \"a\"\ntype = \"npm\"\n[registries.rate_limit]\nrps = 1\n";
        let added = v(&[], &["registries[a]"], &["registries[a].rate_limit.rps"]);
        assert_eq!(verdict("", one), added);
        assert_eq!(verdict("registries = []\n", one), added);
        assert_eq!(verdict(one, ""), v(&[], &["registries[a]"], &[]));
    }

    #[test]
    fn a_placeholder_and_its_escaped_literal_differ() {
        // As `merge_layers` writes them: `"${X}"` and `"$${X}"`.
        assert_eq!(
            verdict(
                "[database]\nurl = \"\\u0024{X}\"\n",
                "[database]\nurl = \"${X}\"\n"
            ),
            v(&[], &[], &["database.url"])
        );
    }

    #[test]
    fn reordering_auth_providers_is_a_change() {
        let a = "[[auth]]\ntype = \"oidc\"\n[[auth]]\ntype = \"kubernetes\"\n";
        let b = "[[auth]]\ntype = \"kubernetes\"\n[[auth]]\ntype = \"oidc\"\n";
        assert_eq!(verdict(a, b), v(&[], &[], &["auth"]));
    }

    #[test]
    fn a_registry_name_may_hold_a_dot() {
        assert_eq!(
            tier_of("registries[npm.internal].cache.idle_days"),
            Tier::Restart
        );
        assert_eq!(tier_of("registries[npm.internal].rules"), Tier::Hot);
    }

    #[test]
    fn a_changed_registry_is_classified_leaf_by_leaf() {
        let before = "[[registries]]\nname = \"a\"\nupstreams = [\"u1\"]\n[registries.cache]\nidle_days = 1\nmetadata_ttl_secs = 1\n";
        let after = "[[registries]]\nname = \"a\"\nupstreams = [\"u2\"]\n[registries.cache]\nidle_days = 2\nmetadata_ttl_secs = 2\n";
        assert_eq!(
            verdict(before, after),
            v(
                &[],
                &[
                    "registries[a].cache.metadata_ttl_secs",
                    "registries[a].upstreams"
                ],
                &["registries[a].cache.idle_days"]
            )
        );
    }

    #[test]
    fn entries_without_a_usable_key_are_one_leaf() {
        let a = "[[auth]]\nissuer = \"x\"\n";
        let b = "[[auth]]\nissuer = \"y\"\n";
        assert_eq!(verdict(a, b), v(&[], &[], &["auth"]));
        // Keyed on `type` when `name` is missing.
        let a = "[[auth]]\ntype = \"oidc\"\nissuer = \"x\"\n";
        let b = "[[auth]]\ntype = \"oidc\"\nissuer = \"y\"\n";
        assert_eq!(verdict(a, b), v(&[], &[], &["auth[oidc].issuer"]));
    }

    #[test]
    fn a_changed_placeholder_name_is_a_change() {
        assert_eq!(
            verdict(
                "[database]\nurl = \"${A}\"\n",
                "[database]\nurl = \"${B}\"\n"
            ),
            v(&[], &[], &["database.url"])
        );
    }

    #[test]
    fn an_empty_table_appearing_is_a_change() {
        assert_eq!(verdict("", "[otel]\n"), v(&[], &[], &["otel"]));
        assert_eq!(verdict("", "[proxy]\n"), v(&[], &["proxy"], &[]));
        assert_eq!(
            verdict("", "[server]\ntrusted_proxies = []\n"),
            v(&["server.trusted_proxies"], &[], &[])
        );
    }

    #[test]
    fn tiers_split_one_document() {
        let before = "[search]\nreadmes = false\n[limits]\nversions_per_page = 1\nmax_artifact_size_bytes = 1\n[database]\nurl = \"a\"\n";
        let after = "[search]\nreadmes = true\n[limits]\nversions_per_page = 2\nmax_artifact_size_bytes = 2\n[database]\nurl = \"b\"\n";
        assert_eq!(
            verdict(before, after),
            v(
                &["limits.versions_per_page", "search.readmes"],
                &["limits.max_artifact_size_bytes"],
                &["database.url"]
            )
        );
    }

    /// The table `docs/guide/hot-reload.md` prints between its `reload-keys`
    /// markers, from [`RELOAD_KEYS`]. `BLESS=1 cargo test -p batlehub-config
    /// docs_table` rewrites the page.
    fn docs_table() -> String {
        let mut rows: Vec<_> = RELOAD_KEYS.iter().collect();
        let rank = |t: &Tier| match t {
            Tier::Hot => 0,
            Tier::Partial => 1,
            Tier::Restart => 2,
        };
        rows.sort_by(|a, b| (rank(&a.1), a.0).cmp(&(rank(&b.1), b.0)));
        let mut out = String::from("| Key | Tier | Read by |\n| --- | --- | --- |\n");
        for (pattern, tier, reader) in rows {
            let tier = format!("{tier:?}").to_lowercase();
            out.push_str(&format!("| `{pattern}` | {tier} | {reader} |\n"));
        }
        out.push_str("| everything else | restart | read once at startup |\n");
        out
    }

    #[test]
    fn docs_table_matches_the_hot_reload_page() {
        const OPEN: &str = "<!-- reload-keys -->\n";
        const CLOSE: &str = "<!-- /reload-keys -->";
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/guide/hot-reload.md");
        let page = std::fs::read_to_string(&path).expect("reading the hot-reload page");
        let start = page.find(OPEN).expect("the opening reload-keys marker") + OPEN.len();
        let end = page.find(CLOSE).expect("the closing reload-keys marker");
        let want = docs_table();
        if std::env::var_os("BLESS").is_some() {
            let blessed = format!("{}{want}{}", &page[..start], &page[end..]);
            std::fs::write(&path, blessed).expect("writing the hot-reload page");
            return;
        }
        assert_eq!(
            &page[start..end],
            want,
            "docs/guide/hot-reload.md is out of date: run `BLESS=1 cargo test -p batlehub-config docs_table`"
        );
    }
}
