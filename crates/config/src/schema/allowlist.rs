//! Validation and warnings for `package_allowlist` (RFC 0051 §4.3).

use std::collections::HashMap;

use anyhow::{bail, Result};
use batlehub_core::entities::{namespace_separator, RegistryKind};
use batlehub_core::rules::version_match::{is_malformed_range, version_matches};

use super::warnings::{self, ConfigWarning};
use super::{AppConfig, RegistryConfig, RuleConfig};
use crate::schema::rules::PackageAllowlistConfig;

/// Kinds whose versions are mostly not semver: a range there refuses most
/// of what the upstream publishes (§4.3 warning).
const NON_SEMVER_KINDS: &[RegistryKind] = &[
    RegistryKind::Pypi,
    RegistryKind::Maven,
    RegistryKind::Nuget,
    RegistryKind::Rubygems,
];

impl AppConfig {
    /// The hard errors of RFC 0051 §4.3, for every registry and namespace.
    pub(super) fn validate_package_allowlists(&self) -> Result<()> {
        for reg in &self.registries {
            let Ok(kind) = reg.registry_type.parse::<RegistryKind>() else {
                continue; // `validate_registries` reports an unknown type.
            };
            check_rule_list(kind, &reg.rules, &format!("registry '{}'", reg.name))?;
            for ns in &reg.namespaces {
                if let Some(rules) = &ns.rules {
                    let at = format!("registry '{}' namespace '{}'", reg.name, ns.match_prefix);
                    check_rule_list(kind, rules, &at)?;
                }
            }
        }
        Ok(())
    }

    /// The warnings of RFC 0051 §4.3.
    pub(super) fn package_allowlist_warnings(&self, out: &mut Vec<ConfigWarning>) {
        for (index, reg) in self.registries.iter().enumerate() {
            let Ok(kind) = reg.registry_type.parse::<RegistryKind>() else {
                continue;
            };
            for (rule_index, rule) in reg.rules.iter().enumerate() {
                let RuleConfig::PackageAllowlist(cfg) = rule else {
                    continue;
                };
                let path = format!("registries[{index}].rules[{rule_index}]");
                allowlist_warnings(kind, reg, cfg, &path, out);
            }
        }
    }
}

fn check_rule_list(kind: RegistryKind, rules: &[RuleConfig], at: &str) -> Result<()> {
    let lists: Vec<&PackageAllowlistConfig> = rules
        .iter()
        .filter_map(|r| match r {
            RuleConfig::PackageAllowlist(c) => Some(c),
            _ => None,
        })
        .collect();
    if lists.len() > 1 {
        bail!(
            "{at}: package_allowlist is declared {} times; keep one block (both would run, so a \
             name would have to be on every list)",
            lists.len()
        );
    }
    let Some(cfg) = lists.first() else {
        return Ok(());
    };
    if !kind.has_package_names() {
        let instead = if kind == RegistryKind::Generic {
            "; use path_allow"
        } else {
            ""
        };
        bail!(
            "{at}: package_allowlist does not apply to a '{}' registry, whose requests do not name \
             a package{instead}",
            kind.as_str()
        );
    }
    if cfg.packages.is_empty() {
        bail!("{at}: package_allowlist.packages is empty; an empty allowlist refuses everything");
    }
    let sep = namespace_separator(kind);
    let mut seen: HashMap<String, &str> = HashMap::new();
    for entry in &cfg.packages {
        let name = entry.name();
        check_name(name, sep)
            .map_err(|why| anyhow::anyhow!("{at}: package_allowlist: entry '{name}': {why}"))?;
        if let Some(versions) = entry.versions() {
            if versions.is_empty() {
                bail!(
                    "{at}: package_allowlist: entry '{name}': versions = [] is ambiguous; leave \
                     versions out to allow every version"
                );
            }
            if kind.is_forge() {
                bail!(
                    "{at}: package_allowlist: entry '{name}': versions do not apply on a forge, \
                     whose archives are keyed by commit, so a range would refuse every download"
                );
            }
            if let Some(bad) = versions.iter().find(|v| is_malformed_range(v)) {
                bail!(
                    "{at}: package_allowlist: entry '{name}': '{bad}' looks like a version range \
                     but does not parse as one"
                );
            }
        }
        if !name.ends_with('*') {
            let canonical = kind.canonical_package_name(name).into_owned();
            if let Some(first) = seen.insert(canonical.clone(), name) {
                bail!(
                    "{at}: package_allowlist: '{first}' and '{name}' are the same {} package (both \
                     read as '{canonical}'); keep one entry",
                    kind.as_str()
                );
            }
        }
    }
    Ok(())
}

/// A name the rule can match: no whitespace or control characters, no
/// version-spec characters, `@` only first (an npm scope), and `*` only as a
/// final `<separator>*`. `validate_package_name` is a path-safety check and
/// accepts all of these (§4.3).
fn check_name(name: &str, sep: char) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("the name is empty".to_owned());
    }
    if name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("a name has no whitespace or control characters".to_owned());
    }
    if let Some(c) = name
        .chars()
        .find(|c| matches!(c, '<' | '>' | '=' | '~' | '^' | '!'))
    {
        return Err(format!(
            "'{c}' belongs in versions, not in the name; write {{ name = \"…\", versions = [\"…\"] }}"
        ));
    }
    if name.char_indices().any(|(i, c)| c == '@' && i > 0) {
        return Err(
            "'@' is only a scope prefix; a version goes in versions, not after the name".to_owned(),
        );
    }
    if let Some(star) = name.find('*') {
        let is_final_prefix = star == name.len() - 1
            && name[..star].ends_with(sep)
            && name.len() > sep.len_utf8() + 1;
        if !is_final_prefix {
            return Err(format!(
                "'*' is only allowed as a final '{sep}*' (everything under a prefix)"
            ));
        }
    }
    Ok(())
}

fn allowlist_warnings(
    kind: RegistryKind,
    reg: &RegistryConfig,
    cfg: &PackageAllowlistConfig,
    path: &str,
    out: &mut Vec<ConfigWarning>,
) {
    let has_ranges = cfg.packages.iter().any(|e| e.versions().is_some());
    if has_ranges && NON_SEMVER_KINDS.contains(&kind) {
        out.push(ConfigWarning::new(
            warnings::ALLOWLIST_RANGE_ON_NON_SEMVER_KIND,
            path.to_owned(),
            format!(
                "Registry '{}' gives version ranges to its package allowlist, but most {} \
                 versions are not semver; versions that do not parse are refused. List exact \
                 versions, or leave versions out.",
                reg.name,
                kind.as_str()
            ),
        ));
    }
    for entry in &cfg.packages {
        if let Some(v) = entry
            .versions()
            .into_iter()
            .flatten()
            .find(|v| v.contains(['x', 'X']) && v.split('.').any(|p| p == "x" || p == "X"))
        {
            out.push(ConfigWarning::new(
                warnings::ALLOWLIST_LITERAL_WILDCARD,
                path.to_owned(),
                format!(
                    "Registry '{}': package allowlist entry '{}' lists '{v}', which is read as the \
                     exact version '{v}' and matches nothing. Write a range such as '^4' or \
                     '>=4.0.0, <5.0.0'.",
                    reg.name,
                    entry.name()
                ),
            ));
        }
    }
    let gate_has_allow = reg
        .rules
        .iter()
        .any(|r| matches!(r, RuleConfig::VersionGate(g) if !g.allow.is_empty()));
    if gate_has_allow {
        out.push(ConfigWarning::new(
            warnings::ALLOWLIST_OVERLAPS_VERSION_GATE,
            path.to_owned(),
            format!(
                "Registry '{}' has both a package allowlist and a version_gate allowlist; a \
                 version must pass both.",
                reg.name
            ),
        ));
    }
    for warm in &reg.cache.warm_packages {
        let (name, version) = split_warm_entry(warm);
        if !warm_entry_allowed(kind, cfg, name, version) {
            out.push(ConfigWarning::new(
                warnings::ALLOWLIST_WARMS_UNLISTED,
                format!("{path}.cache.warm_packages"),
                format!(
                    "Registry '{}' warms '{warm}', which its package allowlist does not serve. \
                     Warming fetches it from the upstream anyway; every read of it is refused. \
                     Remove it from warm_packages or add it to the allowlist.",
                    reg.name
                ),
            ));
        }
    }
}

/// `"lodash"` or `"lodash@4.17.21"`, with an npm scope's leading `@` kept.
fn split_warm_entry(entry: &str) -> (&str, Option<&str>) {
    match entry
        .char_indices()
        .skip(1)
        .filter(|(_, c)| *c == '@')
        .last()
    {
        Some((i, _)) => (&entry[..i], Some(&entry[i + 1..])),
        None => (entry, None),
    }
}

fn warm_entry_allowed(
    kind: RegistryKind,
    cfg: &PackageAllowlistConfig,
    name: &str,
    version: Option<&str>,
) -> bool {
    let sep = namespace_separator(kind);
    let canonical = kind.canonical_package_name(name);
    // Exact entries first, then the longest prefix — the rule's own order.
    let exact = cfg
        .packages
        .iter()
        .find(|e| !e.name().ends_with('*') && kind.canonical_package_name(e.name()) == canonical);
    let matched = exact.or_else(|| {
        cfg.packages
            .iter()
            .filter_map(|e| {
                let prefix = e.name().strip_suffix('*')?.strip_suffix(sep)?;
                let prefix = kind.canonical_package_name(prefix).into_owned();
                let under = canonical.as_ref() != prefix
                    && canonical.starts_with(prefix.as_str())
                    && canonical[prefix.len()..].starts_with(sep);
                under.then_some((prefix.len(), e))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, e)| e)
    });
    match (matched, version) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(entry), Some(v)) => entry
            .versions()
            .is_none_or(|vs| vs.iter().any(|r| version_matches(r, v))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
        [database]
        type = "postgresql"
        url = "postgresql://localhost/test"

        [storage]
        type = "filesystem"
        path = "/tmp/batlehub-test"

        [server]
        host = "127.0.0.1"
        port = 8080
    "#;

    fn parse(extra: &str) -> std::result::Result<AppConfig, String> {
        toml::from_str::<AppConfig>(&format!("{BASE}\n{extra}")).map_err(|e| e.to_string())
    }

    fn registry(kind: &str, rule: &str) -> String {
        format!(
            "[[registries]]\nname = \"reg\"\ntype = \"{kind}\"\n\n[[registries.rules]]\nkind = \"package_allowlist\"\n{rule}\n"
        )
    }

    fn validate_err(kind: &str, rule: &str) -> String {
        let cfg = parse(&registry(kind, rule)).expect("parses");
        cfg.validate().expect_err("should be refused").to_string()
    }

    fn warning_codes(kind: &str, extra: &str) -> Vec<String> {
        let cfg = parse(extra).expect("parses");
        cfg.validate().expect("valid");
        let _ = kind;
        cfg.warnings().into_iter().map(|w| w.code).collect()
    }

    #[test]
    fn a_plain_allowlist_is_valid_and_quiet() {
        let cfg = parse(&registry(
            "npm",
            r#"packages = ["react", { name = "lodash", versions = [">=4.17.21, <5"] }, "@acme/*"]"#,
        ))
        .expect("parses");
        cfg.validate().expect("valid");
        assert!(!cfg
            .warnings()
            .iter()
            .any(|w| w.code.starts_with("allowlist.")));
    }

    #[test]
    fn a_misspelled_key_is_named() {
        let err = parse(&registry(
            "npm",
            r#"packages = [{ name = "lodash", version = ["<5"] }]"#,
        ))
        .expect_err("unknown key refused");
        assert!(
            err.contains("unknown key 'version'") && err.contains("'lodash'"),
            "{err}"
        );
    }

    #[test]
    fn empty_packages_is_refused() {
        assert!(validate_err("npm", "packages = []").contains("is empty"));
    }

    #[test]
    fn empty_versions_is_refused() {
        let err = validate_err("npm", r#"packages = [{ name = "lodash", versions = [] }]"#);
        assert!(err.contains("ambiguous"), "{err}");
    }

    #[test]
    fn a_malformed_range_is_refused() {
        let err = validate_err(
            "npm",
            r#"packages = [{ name = "lodash", versions = ["=> 4.17"] }]"#,
        );
        assert!(err.contains("does not parse"), "{err}");
    }

    #[test]
    fn a_version_spec_in_a_name_is_refused() {
        let err = validate_err("npm", r#"packages = ["lodash@^4"]"#);
        assert!(err.contains("entry 'lodash@^4'"), "{err}");
    }

    #[test]
    fn a_canonical_collision_is_refused() {
        let err = validate_err("pypi", r#"packages = ["Requests", "requests"]"#);
        assert!(err.contains("'Requests' and 'requests'"), "{err}");
    }

    #[test]
    fn two_allowlist_blocks_are_refused() {
        let extra = format!(
            "{}\n[[registries.rules]]\nkind = \"package_allowlist\"\npackages = [\"vue\"]\n",
            registry("npm", r#"packages = ["react"]"#)
        );
        let err = parse(&extra)
            .expect("parses")
            .validate()
            .expect_err("refused")
            .to_string();
        assert!(err.contains("declared 2 times"), "{err}");
    }

    #[test]
    fn versions_on_a_forge_are_refused() {
        let err = validate_err(
            "github",
            r#"packages = [{ name = "acme/tool", versions = ["v1.0.0"] }]"#,
        );
        assert!(err.contains("forge"), "{err}");
    }

    #[test]
    fn kinds_without_package_names_are_refused() {
        let generic = "[[registries]]\nname = \"files\"\ntype = \"generic\"\nupstreams = [\"https://example.com\"]\npath_allow = [\"**\"]\n\n[[registries.rules]]\nkind = \"package_allowlist\"\npackages = [\"repo\"]\n";
        let err = parse(generic)
            .expect("parses")
            .validate()
            .expect_err("refused")
            .to_string();
        assert!(err.contains("use path_allow"), "{err}");
        let deb = "[[registries]]\nname = \"debian\"\ntype = \"deb\"\nupstreams = [\"https://deb.debian.org/debian\"]\n\n[[registries.rules]]\nkind = \"package_allowlist\"\npackages = [\"curl\"]\n";
        let err = parse(deb)
            .expect("parses")
            .validate()
            .expect_err("refused")
            .to_string();
        assert!(err.contains("does not apply to a 'deb' registry"), "{err}");
    }

    #[test]
    fn a_range_on_pypi_warns() {
        let codes = warning_codes(
            "pypi",
            &registry(
                "pypi",
                r#"packages = [{ name = "django", versions = [">=4.0.0"] }]"#,
            ),
        );
        assert!(
            codes.contains(&"allowlist.range-on-non-semver-kind".to_owned()),
            "{codes:?}"
        );
    }

    #[test]
    fn a_literal_wildcard_warns() {
        let codes = warning_codes(
            "npm",
            &registry(
                "npm",
                r#"packages = [{ name = "lodash", versions = ["4.x"] }]"#,
            ),
        );
        assert!(
            codes.contains(&"allowlist.literal-wildcard".to_owned()),
            "{codes:?}"
        );
    }

    #[test]
    fn an_overlapping_version_gate_warns() {
        let extra = format!(
            "{}\n[[registries.rules]]\nkind = \"version_gate\"\nallow = [\"^1\"]\n",
            registry("npm", r#"packages = ["react"]"#)
        );
        let codes = warning_codes("npm", &extra);
        assert!(
            codes.contains(&"allowlist.overlaps-version-gate".to_owned()),
            "{codes:?}"
        );
    }

    #[test]
    fn warming_an_unlisted_package_warns() {
        let extra = format!(
            "[[registries]]\nname = \"reg\"\ntype = \"npm\"\n[registries.cache]\nwarm_packages = [\"react\", \"left-pad@1.3.0\", \"lodash@4.17.20\"]\n\n[[registries.rules]]\nkind = \"package_allowlist\"\npackages = [\"react\", {{ name = \"lodash\", versions = [\">=4.17.21\"] }}]\n"
        );
        let cfg = parse(&extra).expect("parses");
        cfg.validate().expect("valid");
        let warned: Vec<String> = cfg
            .warnings()
            .into_iter()
            .filter(|w| w.code == "allowlist.warms-unlisted")
            .map(|w| w.message)
            .collect();
        assert_eq!(warned.len(), 2, "{warned:?}");
        assert!(warned.iter().any(|m| m.contains("'left-pad@1.3.0'")));
        assert!(warned.iter().any(|m| m.contains("'lodash@4.17.20'")));
    }

    #[test]
    fn warm_entries_split_on_the_last_at_after_a_scope() {
        assert_eq!(split_warm_entry("lodash"), ("lodash", None));
        assert_eq!(
            split_warm_entry("lodash@4.17.21"),
            ("lodash", Some("4.17.21"))
        );
        assert_eq!(split_warm_entry("@acme/x"), ("@acme/x", None));
        assert_eq!(
            split_warm_entry("@acme/x@1.0.0"),
            ("@acme/x", Some("1.0.0"))
        );
    }

    #[test]
    fn names_with_version_specs_or_stray_stars_are_refused() {
        for bad in [
            "lodash@^4",
            "requests>=2",
            "left pad",
            "a*b",
            "*",
            "@acme*",
            "x/*/y",
        ] {
            assert!(check_name(bad, '/').is_err(), "{bad}");
        }
        for good in ["lodash", "@acme/x", "@acme/*", "github.com/acme/tool"] {
            assert!(check_name(good, '/').is_ok(), "{good}");
        }
        assert!(check_name("Acme.*", '.').is_ok());
        assert!(check_name("com.acme:*", ':').is_ok());
    }
}
