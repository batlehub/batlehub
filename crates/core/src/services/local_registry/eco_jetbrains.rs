use chrono::{DateTime, Utc};

use super::{CoreError, Identity, LocalRegistryService, PublishedPackage};

/// One published JetBrains Marketplace plugin version, decoded from the
/// per-version `index_metadata` stored at publish time. The web layer renders
/// `updatePlugins.xml`, the plugin JSON shapes, and both `meta.json` shapes
/// from this struct.
#[derive(Debug, Clone)]
pub struct JetbrainsPluginVersion {
    /// The plugin `<id>` from `META-INF/plugin.xml` (a.k.a. xmlId).
    pub xml_id: String,
    pub version: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub vendor: Option<String>,
    pub since_build: Option<String>,
    pub until_build: Option<String>,
    /// Release channel; empty string is the Stable channel.
    pub channel: String,
    pub file_name: Option<String>,
    pub size: u64,
    pub depends: Vec<String>,
    pub change_notes: Option<String>,
    /// SHA-256 hex of the artifact bytes.
    pub checksum: String,
    pub published_at: DateTime<Utc>,
}

impl JetbrainsPluginVersion {
    fn from_published(pkg: &PublishedPackage) -> Self {
        let m = &pkg.index_metadata;
        let s = |key: &str| {
            m.get(key)
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        Self {
            xml_id: pkg.name.clone(),
            version: pkg.version.clone(),
            name: s("name"),
            description: s("description"),
            vendor: s("vendor"),
            since_build: s("sinceBuild"),
            until_build: s("untilBuild"),
            channel: s("channel").unwrap_or_default(),
            file_name: s("fileName"),
            size: m.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
            depends: m
                .get("depends")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            change_notes: s("changeNotes"),
            checksum: pkg.checksum.clone(),
            published_at: pkg.published_at,
        }
    }

    /// Whether this version is installable on IDE `build` (both bounds inclusive;
    /// absent bounds are open-ended).
    pub fn compatible_with(&self, build: &str) -> bool {
        build_in_range(
            self.since_build.as_deref(),
            self.until_build.as_deref(),
            build,
        )
    }
}

impl LocalRegistryService {
    /// All visible, non-yanked versions of one plugin, oldest first.
    ///
    /// `unlisted` (the storage of the marketplace `isHidden` flag) is filtered
    /// by `load_visible_versions`; `yanked` has no representation in the
    /// marketplace protocol, so yanked versions are dropped here like the
    /// Composer eco does.
    pub async fn get_jetbrains_versions(
        &self,
        registry: &str,
        xml_id: &str,
        identity: &Identity,
    ) -> Result<Vec<JetbrainsPluginVersion>, CoreError> {
        let versions = self
            .load_visible_versions_or_not_found(registry, xml_id, identity, "JetBrains plugin")
            .await?;
        let versions: Vec<_> = versions
            .iter()
            .filter(|p| !p.yanked)
            .map(JetbrainsPluginVersion::from_published)
            .collect();
        if versions.is_empty() {
            return Err(CoreError::NotFound(format!(
                "JetBrains plugin '{xml_id}' not found in local registry '{registry}'"
            )));
        }
        Ok(versions)
    }

    /// The xmlId of every plugin in `registry` this caller may see — the
    /// `files/pluginsXMLIds.json` document.
    ///
    /// The same two gates as [`Self::get_jetbrains_plugins`], since both
    /// documents name every plugin (RFC 0015 §4.4): the read grant, then a
    /// plugin with no version left after visibility and grants is not listed.
    pub async fn get_jetbrains_plugin_ids(
        &self,
        registry: &str,
        identity: &Identity,
    ) -> Result<Vec<String>, CoreError> {
        let names = self.backend.list_package_names(registry).await?;
        let (readable, grant_rows) = self.readable_packages(registry, identity).await?;
        let mut ids = Vec::new();
        for name in names {
            if !readable.contains(&name) {
                continue;
            }
            match self
                .load_visible_versions_in(registry, &name, identity, Some(&grant_rows))
                .await
            {
                Ok(v) if !v.is_empty() => ids.push(name),
                Ok(_) | Err(CoreError::AccessDenied(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(ids)
    }

    /// Newest build-compatible version of every visible plugin in `registry`,
    /// on `channel` (empty string = Stable). `build = None` skips the
    /// compatibility filter (plain listings without a `build` parameter).
    ///
    /// Packages whose visibility check denies `identity` are skipped, not
    /// surfaced as errors — a listing should show what the caller may see.
    pub async fn get_jetbrains_plugins(
        &self,
        registry: &str,
        build: Option<&str>,
        channel: &str,
        identity: &Identity,
    ) -> Result<Vec<JetbrainsPluginVersion>, CoreError> {
        let names = self.backend.list_package_names(registry).await?;
        let (readable, grant_rows) = self.readable_packages(registry, identity).await?;
        let mut plugins = Vec::new();
        for name in names {
            // RFC 0015 §4.4 — this document names every plugin in the registry,
            // so a caller without the read grant must not find one here. One
            // resolution for the whole loop, not one per plugin (§13.2).
            if !readable.contains(&name) {
                continue;
            }
            let versions = match self
                .load_visible_versions_in(registry, &name, identity, Some(&grant_rows))
                .await
            {
                Ok(v) => v,
                Err(CoreError::AccessDenied(_)) => continue,
                Err(e) => return Err(e),
            };
            if let Some(best) = Self::newest_jetbrains_match(&versions, build, channel) {
                plugins.push(best);
            }
        }
        plugins.sort_by(|a, b| a.xml_id.cmp(&b.xml_id));
        Ok(plugins)
    }

    /// Newest build-compatible version for each requested xmlId (missing or
    /// incompatible plugins are simply absent from the result).
    pub async fn get_jetbrains_compatible_updates(
        &self,
        registry: &str,
        xml_ids: &[String],
        build: &str,
        channel: &str,
        identity: &Identity,
    ) -> Result<Vec<JetbrainsPluginVersion>, CoreError> {
        let mut updates = Vec::new();
        for xml_id in xml_ids {
            let versions = match self.load_visible_versions(registry, xml_id, identity).await {
                Ok(v) => v,
                Err(CoreError::AccessDenied(_)) | Err(CoreError::NotFound(_)) => continue,
                Err(e) => return Err(e),
            };
            if let Some(best) = Self::newest_jetbrains_match(&versions, Some(build), channel) {
                updates.push(best);
            }
        }
        Ok(updates)
    }

    /// Pick the newest matching version by publish date — plugin version strings
    /// are arbitrary (no semver guarantee), so lexicographic ordering is wrong.
    fn newest_jetbrains_match(
        versions: &[PublishedPackage],
        build: Option<&str>,
        channel: &str,
    ) -> Option<JetbrainsPluginVersion> {
        versions
            .iter()
            .filter(|p| !p.yanked)
            .map(JetbrainsPluginVersion::from_published)
            .filter(|v| v.channel == channel)
            .filter(|v| build.is_none_or(|b| v.compatible_with(b)))
            .max_by_key(|v| v.published_at)
    }
}

/// Whether IDE `build` falls in the inclusive `[since, until]` range, using
/// IntelliJ `BuildNumber` semantics:
///
/// - a product prefix (`IU-241.14494` → `241.14494`) is stripped from `build`
///   and from the bounds;
/// - components compare numerically, dot by dot; a shorter build is padded
///   with zeros (`241` == `241.0.0`);
/// - `*` in a bound matches any value from that component on (`241.*` accepts
///   every `241.x`);
/// - an absent or empty bound is open-ended;
/// - a malformed bound is ignored (open) rather than rejecting the build —
///   best-effort, matching the marketplace's lenient handling.
pub fn build_in_range(since: Option<&str>, until: Option<&str>, build: &str) -> bool {
    let build = parse_build_components(build);
    let lower_ok = match since.map(str::trim).filter(|s| !s.is_empty()) {
        // `*` in a lower bound means "any" — treat as no constraint (i64::MIN).
        Some(bound) => cmp_build_to_bound(&build, bound, i64::MIN) != std::cmp::Ordering::Less,
        None => true,
    };
    let upper_ok = match until.map(str::trim).filter(|s| !s.is_empty()) {
        // `*` in an upper bound means "any" — treat as +infinity (i64::MAX).
        Some(bound) => cmp_build_to_bound(&build, bound, i64::MAX) != std::cmp::Ordering::Greater,
        None => true,
    };
    lower_ok && upper_ok
}

/// Strip an alphabetic product prefix (`IU-`, `PY-`, …) and split into numeric
/// components. Unparseable components degrade to 0 rather than failing.
fn parse_build_components(build: &str) -> Vec<i64> {
    strip_product_prefix(build.trim())
        .split('.')
        .map(|c| c.trim().parse::<i64>().unwrap_or(0))
        .collect()
}

fn strip_product_prefix(s: &str) -> &str {
    match s.split_once('-') {
        Some((prefix, rest))
            if !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_alphabetic()) =>
        {
            rest
        }
        _ => s,
    }
}

/// Compare `build` against `bound`, substituting `star` for `*` components
/// (and for any component that is not a number — lenient parsing). Missing
/// trailing components on either side compare as 0.
fn cmp_build_to_bound(build: &[i64], bound: &str, star: i64) -> std::cmp::Ordering {
    let bound: Vec<i64> = strip_product_prefix(bound)
        .split('.')
        .map(|c| {
            let c = c.trim();
            if c == "*" {
                star
            } else {
                c.parse::<i64>().unwrap_or(star)
            }
        })
        .collect();
    let len = build.len().max(bound.len());
    for i in 0..len {
        let b = build.get(i).copied().unwrap_or(0);
        let r = bound.get(i).copied().unwrap_or(0);
        // Once a `*` is reached in the bound, every later build component matches.
        if r == star && bound.get(i).copied() == Some(star) && star != 0 {
            return std::cmp::Ordering::Equal;
        }
        match b.cmp(&r) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::build_in_range;

    #[test]
    fn open_range_accepts_everything() {
        assert!(build_in_range(None, None, "241.14494"));
        assert!(build_in_range(Some(""), Some(""), "241.14494"));
        assert!(build_in_range(Some("  "), None, "IU-999.1"));
    }

    #[test]
    fn simple_inclusive_bounds() {
        assert!(build_in_range(Some("233.0"), Some("241.100"), "240.5"));
        assert!(build_in_range(Some("233.0"), Some("241.100"), "233.0"));
        assert!(build_in_range(Some("233.0"), Some("241.100"), "241.100"));
        assert!(!build_in_range(Some("233.0"), Some("241.100"), "232.999"));
        assert!(!build_in_range(Some("233.0"), Some("241.100"), "241.101"));
        assert!(!build_in_range(Some("233.0"), Some("241.100"), "242.0"));
    }

    #[test]
    fn star_in_until_matches_whole_branch() {
        assert!(build_in_range(
            Some("241.0"),
            Some("241.*"),
            "241.14494.240"
        ));
        assert!(build_in_range(None, Some("241.*"), "241.0"));
        assert!(!build_in_range(None, Some("241.*"), "242.0"));
        // Star only opens components from its own position on.
        assert!(build_in_range(None, Some("241.100.*"), "241.100.9999"));
        assert!(!build_in_range(None, Some("241.100.*"), "241.101.0"));
    }

    #[test]
    fn star_in_since_is_open() {
        assert!(build_in_range(Some("*"), None, "1.0"));
        assert!(build_in_range(Some("241.*"), None, "241.0"));
    }

    #[test]
    fn product_prefix_is_stripped() {
        assert!(build_in_range(Some("233.0"), Some("241.*"), "IU-241.14494"));
        assert!(build_in_range(Some("IC-233.0"), Some("IU-241.*"), "241.1"));
        assert!(!build_in_range(Some("233.0"), Some("241.*"), "PY-242.1"));
    }

    #[test]
    fn shorter_build_pads_with_zeros() {
        // "241" == 241.0.0…, so it satisfies since=241.0 but a bare
        // until=241 excludes 241.1 (strict BuildNumber comparison).
        assert!(build_in_range(Some("241.0"), None, "241"));
        assert!(build_in_range(None, Some("241"), "241"));
        assert!(!build_in_range(None, Some("241"), "241.1"));
        assert!(build_in_range(Some("241"), None, "241.1"));
    }

    #[test]
    fn open_ended_until() {
        assert!(build_in_range(Some("233.0"), None, "999.999"));
        assert!(!build_in_range(Some("233.0"), None, "232.0"));
    }

    #[test]
    fn malformed_bound_is_ignored() {
        // A completely non-numeric bound degrades to open rather than
        // rejecting every build.
        assert!(build_in_range(Some("not-a-build"), None, "241.0"));
        assert!(build_in_range(None, Some("nonsense"), "241.0"));
    }

    #[test]
    fn snapshot_style_components() {
        assert!(build_in_range(Some("243.0"), Some("243.*"), "243.12345.67"));
        assert!(!build_in_range(Some("243.21565"), Some("244.*"), "243.100"));
    }
}
