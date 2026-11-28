//! The version matcher `version_gate` and `package_allowlist` share
//! (RFC 0051 §5.2): an exact version, or a semver range.

use semver::{Version, VersionReq};

/// The characters that make an entry a range rather than an exact version.
const RANGE_CHARS: &[char] = &['<', '>', '=', '^', '~', '*', ','];

/// A single allow/block entry: either an exact version string or a semver range.
///
/// An entry is treated as a semver [`VersionReq`] only when it contains a range
/// operator (`<`, `>`, `=`, `^`, `~`, `*`, `,`) and parses successfully;
/// otherwise it is matched by exact string equality. This keeps plain entries
/// like `"1.2.3"` exact (rather than the caret semantics `VersionReq` would
/// otherwise apply) while still allowing ranges such as `">=1.2.0, <2.0.0"`, and
/// lets non-semver version strings (git hashes, dates) be listed verbatim.
#[derive(Debug, Clone)]
pub(crate) enum VersionMatcher {
    Exact(String),
    Range(VersionReq),
}

impl VersionMatcher {
    pub(crate) fn parse(s: &str) -> Self {
        if s.contains(RANGE_CHARS) {
            if let Ok(req) = VersionReq::parse(s) {
                return VersionMatcher::Range(req);
            }
        }
        VersionMatcher::Exact(s.to_owned())
    }

    pub(crate) fn matches(&self, version: &str) -> bool {
        match self {
            VersionMatcher::Exact(e) => e == version,
            VersionMatcher::Range(req) => Version::parse(version)
                .map(|v| req.matches(&v))
                .unwrap_or(false),
        }
    }
}

/// Whether `entry` looks like a range — it carries a range character — but
/// does not parse as one. [`VersionMatcher::parse`] would fall back to an
/// exact match on the literal text, which no version ever equals, so config
/// validation refuses such an entry rather than letting it refuse every
/// version with no message (RFC 0051 §4.3).
pub fn is_malformed_range(entry: &str) -> bool {
    entry.contains(RANGE_CHARS) && VersionReq::parse(entry).is_err()
}

/// Whether `version` satisfies one allowlist entry (exact, or a range), the
/// way the rules match it. For config warnings, which cannot reach the
/// crate-private matcher.
pub fn version_matches(entry: &str, version: &str) -> bool {
    VersionMatcher::parse(entry).matches(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_that_parses_is_not_malformed() {
        assert!(!is_malformed_range(">=1.2.0, <2.0.0"));
        assert!(!is_malformed_range("^4"));
    }

    #[test]
    fn a_range_character_that_does_not_parse_is_malformed() {
        assert!(is_malformed_range("=> 4.17"));
        assert!(is_malformed_range(">=1.2.0,,"));
    }

    #[test]
    fn a_plain_version_is_never_malformed() {
        assert!(!is_malformed_range("1.2.3"));
        assert!(!is_malformed_range("4.x"));
        assert!(!is_malformed_range("2024-01-01"));
    }

    #[test]
    fn an_exact_entry_is_not_a_caret_range() {
        let m = VersionMatcher::parse("1.2.3");
        assert!(m.matches("1.2.3"));
        assert!(!m.matches("1.2.4"));
    }

    #[test]
    fn a_range_does_not_match_an_unparseable_version() {
        assert!(!VersionMatcher::parse(">=1.0.0").matches("2.10"));
    }
}
