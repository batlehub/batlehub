//! `registry/name[@version]` — the one way a command names a package
//! (RFC 0040 §4.1).
//!
//! The registry is everything before the first `/`, the version everything
//! after the last `@` that is not the name's first character. The legacy
//! `registry:name@version` spelling is read for good, with a warning
//! (decision 42), and never printed.

use std::fmt;

use crate::api::BatleHubClient;
use crate::exit::{CliError, ExitCode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coordinate {
    pub registry: String,
    pub name: String,
    pub version: Option<String>,
}

impl fmt::Display for Coordinate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.registry, self.name)?;
        if let Some(v) = &self.version {
            write!(f, "@{v}")?;
        }
        Ok(())
    }
}

/// How the string named its registry, before anyone checked it exists.
#[derive(Debug, PartialEq, Eq)]
enum Prefix<'a> {
    /// `reg/rest`
    Slash(&'a str, &'a str),
    /// `reg:rest`, the deprecated spelling
    Colon(&'a str, &'a str),
    /// no registry part
    Bare,
}

fn split_prefix(raw: &str) -> Prefix<'_> {
    let slash = raw.find('/');
    let colon = raw.find(':');
    match (slash, colon) {
        // `npm:@scope/x@1`: the `:` comes first, so it is the separator.
        (Some(s), Some(c)) if c < s && c > 0 && !raw[..c].contains('@') => {
            Prefix::Colon(&raw[..c], &raw[c + 1..])
        }
        (Some(s), _) if s > 0 && !raw.starts_with('@') => Prefix::Slash(&raw[..s], &raw[s + 1..]),
        (None, Some(c)) if c > 0 && !raw[..c].contains('@') => {
            Prefix::Colon(&raw[..c], &raw[c + 1..])
        }
        _ => Prefix::Bare,
    }
}

fn split_version(raw: &str, rest: &str) -> Result<(String, Option<String>), CliError> {
    let bad = || {
        CliError::new(
            ExitCode::Usage,
            format!("expected registry/name[@version], got `{raw}`"),
        )
    };
    let (name, version) = match rest.rfind('@') {
        // A leading `@` is an npm scope, not a version separator.
        Some(at) if at > 0 => {
            let version = &rest[at + 1..];
            // A trailing `@` names no version; sent on, it would ask the server
            // for the empty version rather than the newest one.
            if version.is_empty() {
                return Err(bad());
            }
            (&rest[..at], Some(version.to_owned()))
        }
        _ => (rest, None),
    };
    if name.is_empty() || name.starts_with('/') {
        return Err(bad());
    }
    Ok((name.to_owned(), version))
}

fn no_registry(raw: &str) -> CliError {
    CliError::new(ExitCode::Usage, format!("`{raw}` names no registry")).hint(format!(
        "write <registry>/{raw}, or pass -r, or set `registry` in your profile"
    ))
}

impl Coordinate {
    /// Parse without asking the server which registries exist: the prefix is
    /// taken as written. `default` is the `-r` / `BATLEHUB_REGISTRY` /
    /// profile registry, used for a bare name.
    pub fn parse(raw: &str, default: Option<&str>) -> Result<Self, CliError> {
        Self::parse_against(raw, default, None)
    }

    /// Parse, reading a prefix that is not a `visible` registry as part of
    /// the name when there is a default registry to put it in
    /// (`-r go github.com/x/y`).
    pub fn parse_against(
        raw: &str,
        default: Option<&str>,
        visible: Option<&[String]>,
    ) -> Result<Self, CliError> {
        let raw = raw.trim();
        let (registry, rest, legacy) = match split_prefix(raw) {
            Prefix::Slash(reg, rest) => (Some(reg), rest, false),
            Prefix::Colon(reg, rest) => (Some(reg), rest, true),
            Prefix::Bare => (None, raw, false),
        };
        let registry = match (registry, visible) {
            (Some(reg), Some(visible)) if !visible.iter().any(|v| v == reg) => {
                let suggestion = visible.iter().find(|v| one_edit_apart(v, reg));
                match default {
                    Some(d) => {
                        if let Some(s) = suggestion {
                            eprintln!("hint: did you mean {s}/{rest}?");
                        }
                        let (name, version) = split_version(raw, raw)?;
                        return Ok(Self {
                            registry: d.to_owned(),
                            name,
                            version,
                        });
                    }
                    None => {
                        let mut err = CliError::new(
                            ExitCode::NotFound,
                            format!(
                                "no registry named `{reg}`; the registries you can see are: {}",
                                if visible.is_empty() {
                                    "none".to_owned()
                                } else {
                                    visible.join(", ")
                                }
                            ),
                        );
                        if let Some(s) = suggestion {
                            err = err.hint(format!("did you mean {s}/{rest}?"));
                        }
                        return Err(err);
                    }
                }
            }
            (Some(reg), _) => reg.to_owned(),
            (None, _) => default.ok_or_else(|| no_registry(raw))?.to_owned(),
        };
        let (name, version) = split_version(raw, rest)?;
        let parsed = Self {
            registry,
            name,
            version,
        };
        if legacy {
            crate::render::warn(&format!(
                "`{raw}` uses the deprecated `registry:name` form; write `{parsed}`"
            ));
        }
        Ok(parsed)
    }

    /// Parse against the registries the caller can see. One `GET` when the
    /// string has a registry prefix; none for a bare name. A failed listing
    /// (an old server, no credential) falls back to taking the prefix as
    /// written, and the command's own call reports the problem.
    pub async fn resolve(
        client: &BatleHubClient,
        raw: &str,
        default: Option<&str>,
    ) -> Result<Self, CliError> {
        if split_prefix(raw.trim()) == Prefix::Bare {
            return Self::parse(raw, default);
        }
        let visible = client
            .list_registries()
            .await
            .ok()
            .map(|rs| rs.into_iter().map(|r| r.name).collect::<Vec<_>>());
        Self::parse_against(raw, default, visible.as_deref())
    }

    /// [`Self::resolve`] for several strings, with one registry listing.
    pub async fn resolve_all(
        client: &BatleHubClient,
        raws: &[String],
        default: Option<&str>,
    ) -> Result<Vec<Self>, CliError> {
        let needs_list = raws.iter().any(|r| split_prefix(r.trim()) != Prefix::Bare);
        let visible = if needs_list {
            client
                .list_registries()
                .await
                .ok()
                .map(|rs| rs.into_iter().map(|r| r.name).collect::<Vec<_>>())
        } else {
            None
        };
        raws.iter()
            .map(|r| Self::parse_against(r, default, visible.as_deref()))
            .collect()
    }

    /// The version, or a usage error naming the command's expectation.
    pub fn require_version(self) -> Result<(String, String, String), CliError> {
        match self.version {
            Some(v) => Ok((self.registry, self.name, v)),
            None => Err(CliError::new(
                ExitCode::Usage,
                format!("`{}/{}` names no version", self.registry, self.name),
            )
            .hint(format!("write {}/{}@<version>", self.registry, self.name))),
        }
    }
}

/// Levenshtein distance ≤ 1, for the typo hint.
fn one_edit_apart(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    match long.len() - short.len() {
        0 => short.iter().zip(&long).filter(|(x, y)| x != y).count() <= 1,
        1 => {
            let i = short.iter().zip(&long).take_while(|(x, y)| x == y).count();
            short[i..] == long[i + 1..]
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(reg: &str, name: &str, v: Option<&str>) -> Coordinate {
        Coordinate {
            registry: reg.into(),
            name: name.into(),
            version: v.map(Into::into),
        }
    }

    #[test]
    fn every_ecosystem_spelling() {
        let cases = [
            ("npm/lodash", c("npm", "lodash", None)),
            ("npm/lodash@4.17.21", c("npm", "lodash", Some("4.17.21"))),
            ("npm/@scope/pkg", c("npm", "@scope/pkg", None)),
            (
                "npm/@scope/pkg@1.2.3",
                c("npm", "@scope/pkg", Some("1.2.3")),
            ),
            (
                "go/github.com/x/y@v1.2.0",
                c("go", "github.com/x/y", Some("v1.2.0")),
            ),
            (
                "maven/org.slf4j:slf4j-api@2.0.9",
                c("maven", "org.slf4j:slf4j-api", Some("2.0.9")),
            ),
            (
                "pypi/zope.interface@6.0",
                c("pypi", "zope.interface", Some("6.0")),
            ),
            (
                "pypi/typing-extensions",
                c("pypi", "typing-extensions", None),
            ),
            (
                "tf/modules/ns/name/aws",
                c("tf", "modules/ns/name/aws", None),
            ),
        ];
        for (raw, want) in cases {
            assert_eq!(Coordinate::parse(raw, None).unwrap(), want, "{raw}");
            assert_eq!(Coordinate::parse(&want.to_string(), None).unwrap(), want);
        }
    }

    #[test]
    fn the_legacy_colon_form_is_still_read() {
        assert_eq!(
            Coordinate::parse("npm:left-pad@1.3.1", None).unwrap(),
            c("npm", "left-pad", Some("1.3.1"))
        );
        assert_eq!(
            Coordinate::parse("npm:@scope/name@1.0.0", None).unwrap(),
            c("npm", "@scope/name", Some("1.0.0"))
        );
        assert_eq!(
            Coordinate::parse("maven:com.acme:lib@1.0", None).unwrap(),
            c("maven", "com.acme:lib", Some("1.0"))
        );
    }

    #[test]
    fn a_bare_name_takes_the_default_registry() {
        assert_eq!(
            Coordinate::parse("lodash@1", Some("npm-local")).unwrap(),
            c("npm-local", "lodash", Some("1"))
        );
        assert_eq!(
            Coordinate::parse("@scope/pkg@1", Some("npm-local")).unwrap(),
            c("npm-local", "@scope/pkg", Some("1"))
        );
        let err = Coordinate::parse("lodash", None).unwrap_err();
        assert_eq!(err.exit, ExitCode::Usage);
        assert!(err.message.contains("names no registry"), "{}", err.message);
    }

    #[test]
    fn an_invisible_prefix_becomes_part_of_the_name_under_a_default() {
        let visible = vec!["go".to_owned(), "npm-local".to_owned()];
        assert_eq!(
            Coordinate::parse_against("github.com/x/y@v1", Some("go"), Some(&visible)).unwrap(),
            c("go", "github.com/x/y", Some("v1"))
        );
        let err = Coordinate::parse_against("npm-locl/x", None, Some(&visible)).unwrap_err();
        assert_eq!(err.exit, ExitCode::NotFound);
        assert!(err.message.contains("go, npm-local"), "{}", err.message);
        assert_eq!(err.hint.as_deref(), Some("did you mean npm-local/x?"));
    }

    #[test]
    fn malformed_coordinates_are_usage_errors() {
        for bad in ["npm/", "npm/x@", "", "/express", "npm/@scope/pkg@"] {
            let err = Coordinate::parse(bad, Some("d")).unwrap_err();
            assert_eq!(err.exit, ExitCode::Usage, "{bad}");
        }
        let err = Coordinate::parse("npm/x", None)
            .unwrap()
            .require_version()
            .unwrap_err();
        assert_eq!(err.exit, ExitCode::Usage);
    }

    #[test]
    fn one_edit() {
        assert!(one_edit_apart("npm-local", "npm-locl"));
        assert!(one_edit_apart("npm-local", "npm-locax"));
        assert!(one_edit_apart("npm", "npmx"));
        assert!(!one_edit_apart("npm", "pypi"));
    }
}
