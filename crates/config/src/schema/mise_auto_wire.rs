//! `[registries.auto_wire]` on a `mise-versions` registry (RFC 0045 §4.1).
//!
//! The wire (one `url_replacements` rule for every mirrored host) and the
//! overlay (the estate's own tools, reachable by short name through mise's
//! floating registry). Both default to the closed side.

use std::collections::HashSet;

use anyhow::{bail, Result};
use serde::Deserialize;

/// Where mise downloads its registry archive from, unless overridden.
pub const DEFAULT_REGISTRY_UPSTREAM: &str = "https://mise.jdx.dev";

/// mise refuses an archive past 4 096 tar entries; upstream holds ~1 000
/// files, so the overlay is bounded well clear of the sum (§4.3).
pub const MAX_OVERLAY_TOOLS: usize = 1000;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AutoWireConfig {
    /// The wire: rewrite `…/_wire/{host}/{rest}`. Default off.
    #[serde(default)]
    pub enabled: bool,
    /// Serve a merged registry archive with `tools`. Default off.
    #[serde(default)]
    pub overlay: bool,
    /// Where the server fetches `registry/latest.tar.zst`.
    #[serde(default)]
    pub registry_upstream: Option<String>,
    /// Overlay names no longer served, still answered locally (§4.2).
    #[serde(default)]
    pub retired: Vec<String>,
    #[serde(default)]
    pub tools: Vec<MiseToolConfig>,
}

impl AutoWireConfig {
    pub fn registry_upstream(&self) -> &str {
        self.registry_upstream
            .as_deref()
            .unwrap_or(DEFAULT_REGISTRY_UPSTREAM)
            .trim_end_matches('/')
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MiseToolConfig {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub source: Option<MiseToolSource>,
    #[serde(default)]
    pub backends: Vec<MiseBackendConfig>,
    #[serde(default)]
    pub bins: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_version_order")]
    pub version_order: String,
    #[serde(default)]
    pub on_collision: OnCollision,
}

fn default_version_order() -> String {
    "semver".to_owned()
}

#[derive(Debug, Clone, Deserialize)]
pub struct MiseToolSource {
    pub registry: String,
    pub repo: String,
}

/// A backend as mise's registry files write it: a string, or a table with
/// `full` and backend options.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MiseBackendConfig {
    Plain(String),
    Full {
        full: String,
        #[serde(default)]
        options: toml::Table,
    },
}

impl MiseBackendConfig {
    pub fn full(&self) -> &str {
        match self {
            Self::Plain(s) => s,
            Self::Full { full, .. } => full,
        }
    }
}

/// What the overlay does when a name is in the upstream archive too (§4.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnCollision {
    /// Both entries removed: the name installs nowhere until an operator acts.
    #[default]
    Drop,
    /// The overlay entry replaces the upstream one.
    Overlay,
    /// The overlay entry is removed.
    Upstream,
}

impl OnCollision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Drop => "drop",
            Self::Overlay => "overlay",
            Self::Upstream => "upstream",
        }
    }
}

/// The backend prefixes mise 2026.11.3 knows (`src/backend/backend_type.rs`);
/// any other entry makes mise reject the whole archive.
const KNOWN_BACKENDS: &[&str] = &[
    "aqua", "asdf", "cargo", "conda", "core", "dotnet", "forgejo", "gem", "github", "gitlab", "go",
    "http", "npm", "packslip", "pipx", "s3", "spm", "ubi", "vfox",
];

/// `^[a-z0-9][a-z0-9-]{0,63}$`: a file name in the archive and a path segment.
pub fn valid_tool_name(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=64).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `owner/name`, no `.`/`..` segment.
fn valid_repo(repo: &str) -> bool {
    let ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    repo.split_once('/').is_some_and(|(o, n)| ok(o) && ok(n))
}

/// A registry's type and first upstream, by name: what a `source` may point at.
pub type SourceLookup<'a> = &'a dyn Fn(&str) -> Option<(String, Option<String>)>;

/// The checks of §4.3 that need only this table, the registry's own type and
/// the names of the registries a `source` may point at. Wiring checks (is the
/// source's prefix wired) run where the wire table is built.
pub fn validate(
    path: &str,
    registry_type: &str,
    aw: &AutoWireConfig,
    source_kind: SourceLookup<'_>,
) -> Result<()> {
    if registry_type != "mise-versions" {
        bail!("{path}: auto_wire is only valid on a mise-versions registry, not {registry_type}");
    }
    if !aw.tools.is_empty() && !aw.overlay {
        bail!(
            "{path}.tools: {} tools declared while overlay = false\n  hint: set overlay = true, or move the names to auto_wire.retired",
            aw.tools.len()
        );
    }
    if aw.tools.len() > MAX_OVERLAY_TOOLS {
        bail!(
            "{path}.tools: {} tools, at most {MAX_OVERLAY_TOOLS}",
            aw.tools.len()
        );
    }
    if let Some(up) = &aw.registry_upstream {
        let ok = url::Url::parse(up).is_ok_and(|u| {
            matches!(u.scheme(), "http" | "https")
                && u.username().is_empty()
                && u.password().is_none()
                && u.query().is_none()
                && u.fragment().is_none()
        });
        if !ok {
            bail!("{path}.registry_upstream: {up:?} is not a plain http(s) URL");
        }
    }
    let claim = |name: &str, at: String| -> Result<()> {
        if !valid_tool_name(name) {
            bail!("{at}: {name:?} must match ^[a-z0-9][a-z0-9-]{{0,63}}$");
        }
        Ok(())
    };
    for (i, t) in aw.tools.iter().enumerate() {
        let at = format!("{path}.tools[{i}]");
        claim(&t.name, format!("{at}.name"))?;
        for (j, a) in t.aliases.iter().enumerate() {
            claim(a, format!("{at}.aliases[{j}]"))?;
        }
        match (&t.source, t.backends.is_empty()) {
            (Some(_), false) => bail!("{at}: set source or backends, not both"),
            (None, true) => bail!("{at}: set source or backends"),
            _ => {}
        }
        for (j, b) in t.backends.iter().enumerate() {
            let full = b.full();
            let known = full
                .split_once(':')
                .is_some_and(|(p, rest)| KNOWN_BACKENDS.contains(&p) && !rest.is_empty());
            if !known {
                bail!("{at}.backends[{j}]: {full:?} has no backend prefix mise knows");
            }
        }
        if let Some(src) = &t.source {
            match source_kind(&src.registry) {
                None => bail!("{at}.source.registry: no registry named {:?}", src.registry),
                Some((kind, upstream)) => {
                    if kind != "github" && kind != "forgejo" {
                        bail!("{at}.source.registry: {:?} is a {kind} registry; only github and forgejo can be a source", src.registry);
                    }
                    if let Some(up) = upstream {
                        let clean = url::Url::parse(&up).is_ok_and(|u| {
                            u.username().is_empty()
                                && u.password().is_none()
                                && u.query().is_none()
                                && u.fragment().is_none()
                        });
                        if !clean {
                            bail!("{at}.source.registry: {:?}'s first upstream carries credentials, a query or a fragment, which would be published in the archive", src.registry);
                        }
                    }
                }
            }
            if !valid_repo(&src.repo) {
                bail!("{at}.source.repo: {:?} must be owner/name", src.repo);
            }
        }
        if t.version_order != "semver" && t.version_order != "source" {
            bail!(
                "{at}.version_order: version_order must be \"source\" or \"semver\", not {:?}",
                t.version_order
            );
        }
    }
    for (i, r) in aw.retired.iter().enumerate() {
        claim(r, format!("{path}.retired[{i}]"))?;
    }
    // Duplicates across names, aliases and `retired`: the archive would hold one.
    let mut seen: HashSet<&str> = HashSet::new();
    for name in aw
        .tools
        .iter()
        .flat_map(|t| std::iter::once(&t.name).chain(t.aliases.iter()))
        .chain(aw.retired.iter())
    {
        if !seen.insert(name.as_str()) {
            bail!(
                "{path}: {name:?} is declared twice among the overlay's names, aliases and retired"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> AutoWireConfig {
        toml::from_str(toml).expect("parses")
    }

    fn none(_: &str) -> Option<(String, Option<String>)> {
        None
    }

    fn forge(name: &str) -> Option<(String, Option<String>)> {
        match name {
            "forge" => Some(("forgejo".into(), Some("https://forge.acme.internal".into()))),
            "leaky" => Some((
                "forgejo".into(),
                Some("https://u:p@forge.acme.internal".into()),
            )),
            "npm" => Some(("npm".into(), None)),
            _ => None,
        }
    }

    #[test]
    fn absent_and_empty_are_closed() {
        let aw = parse("");
        assert!(!aw.enabled && !aw.overlay);
        assert_eq!(aw.registry_upstream(), DEFAULT_REGISTRY_UPSTREAM);
        validate("r", "mise-versions", &aw, &none).expect("empty is valid");
    }

    #[test]
    fn only_on_mise_versions() {
        let err = validate("r", "npm", &parse("enabled = true"), &none).unwrap_err();
        assert!(
            err.to_string().contains("only valid on a mise-versions"),
            "{err}"
        );
    }

    #[test]
    fn tools_need_the_overlay() {
        let aw = parse(
            r#"[[tools]]
name = "acme"
backends = ["github:acme/x"]"#,
        );
        let err = validate("r", "mise-versions", &aw, &none).unwrap_err();
        assert!(err.to_string().contains("overlay = false"), "{err}");
    }

    #[test]
    fn rejects_each_section_4_3_condition() {
        let cases = [
            (
                r#"overlay = true
[[tools]]
name = "Bad_Name"
backends = ["github:a/b"]"#,
                "must match",
            ),
            (
                r#"overlay = true
[[tools]]
name = "a"
backends = ["github:a/b"]
source = { registry = "forge", repo = "a/b" }"#,
                "not both",
            ),
            (
                r#"overlay = true
[[tools]]
name = "a""#,
                "set source or backends",
            ),
            (
                r#"overlay = true
[[tools]]
name = "a"
backends = ["nope:a/b"]"#,
                "no backend prefix",
            ),
            (
                r#"overlay = true
[[tools]]
name = "a"
source = { registry = "npm", repo = "a/b" }"#,
                "only github and forgejo",
            ),
            (
                r#"overlay = true
[[tools]]
name = "a"
source = { registry = "leaky", repo = "a/b" }"#,
                "credentials",
            ),
            (
                r#"overlay = true
[[tools]]
name = "a"
source = { registry = "forge", repo = "a/../b" }"#,
                "owner/name",
            ),
            (
                r#"overlay = true
[[tools]]
name = "a"
backends = ["github:a/b"]
version_order = "date""#,
                "version_order",
            ),
            (
                r#"overlay = true
retired = ["a"]
[[tools]]
name = "a"
backends = ["github:a/b"]"#,
                "declared twice",
            ),
            (
                r#"overlay = true
[[tools]]
name = "a"
aliases = ["b"]
backends = ["github:a/b"]
[[tools]]
name = "b"
backends = ["github:a/c"]"#,
                "declared twice",
            ),
            ("registry_upstream = \"ftp://x\"", "plain http"),
        ];
        for (toml, want) in cases {
            let err = validate("r", "mise-versions", &parse(toml), &forge).unwrap_err();
            assert!(err.to_string().contains(want), "{toml}\n→ {err}");
        }
    }

    #[test]
    fn a_full_table_loads() {
        let aw = parse(
            r#"enabled = true
overlay = true
retired = ["acme-old"]
[[tools]]
name = "acme-deploy"
aliases = ["deploy"]
source = { registry = "forge", repo = "acme/deploy" }
[[tools]]
name = "acme-lint"
backends = [{ full = "http:acme-lint", options = { url = "https://x/{{version}}" } }]
on_collision = "overlay""#,
        );
        validate("r", "mise-versions", &aw, &forge).expect("valid");
        assert_eq!(aw.tools[0].version_order, "semver");
        assert_eq!(aw.tools[1].on_collision, OnCollision::Overlay);
    }
}
