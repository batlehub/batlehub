//! SDKMAN's coordinates, read once for the four places that need them.
//!
//! RFC 0010 §4.3, §5.2. SDKMAN addresses everything by
//! `{candidate}/{version}/{platform}`: the candidate is the package, the
//! platform is a sub-coordinate carried in `PackageId::artifact`, and the
//! version stands alone (`21.0.5-tem`, `3.9.9`). The listing documents are the
//! exception — `versions/all` and the rendered `versions/list` are per
//! *candidate and platform* with no version — and
//! `RegistryClient::fetch_version_document` has nowhere but `package` to put
//! the platform. So it travels inside the package string, `java/linuxx64`,
//! the way Terraform's `modules/`/`providers/` prefix already does; and because
//! a *block* is a statement about the candidate rather than the platform,
//! [`candidate_of`] is what the blocked-set lookup uses.
//!
//! The platform axis is a closed set of eight values produced by
//! `infer_platform` in SDKMAN's installer. Anything else is refused at the
//! edge rather than forwarded upstream as a path segment.

/// The platforms SDKMAN's installer can produce, as it spells them.
pub const PLATFORMS: &[&str] = &[
    "linuxx64",
    "linuxx32",
    "linuxarm32hf",
    "linuxarm64",
    "darwinx64",
    "darwinarm64",
    "windowsx64",
    "exotic",
];

/// The platform a listing is read for when the caller has none to give.
///
/// The console's discovery read and `list_versions` ask about a *candidate*;
/// SDKMAN answers only for a candidate on a platform. Every JDK vendor ships a
/// Linux x64 build, so it is the platform whose list is most nearly the
/// union of the others.
pub const DEFAULT_PLATFORM: &str = "linuxx64";

/// `Some(platform)` when `value` is one of SDKMAN's eight platforms.
pub fn parse_platform(value: &str) -> Option<&'static str> {
    PLATFORMS.iter().copied().find(|p| *p == value)
}

/// The candidate a listing package string names.
///
/// `java/linuxx64` → `java`; `java/linuxx64?current=…` → `java`; `java` →
/// `java`. Used for the blocked-version lookup, so a block on a JDK covers all
/// eight platforms rather than the one whose listing was being read (RFC 0010
/// decision 8).
pub fn candidate_of(package: &str) -> &str {
    let end = package.find(['/', '?']).unwrap_or(package.len());
    &package[..end]
}

/// The `package` string a candidate's listing on a platform is addressed by.
pub fn listing_package(candidate: &str, platform: &str) -> String {
    format!("{candidate}/{platform}")
}

/// The pieces of a listing package string: candidate, platform (defaulting
/// to [`DEFAULT_PLATFORM`]) and the query string, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListingCoordinate<'a> {
    pub candidate: &'a str,
    pub platform: &'a str,
    /// The rendered list's `current=…&installed=…`, without the `?`.
    pub query: Option<&'a str>,
}

/// Split `java/linuxx64?current=21.0.5-tem&installed=` into its parts.
pub fn split_listing_package(package: &str) -> ListingCoordinate<'_> {
    let (path, query) = match package.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (package, None),
    };
    let (candidate, platform) = match path.split_once('/') {
        Some((c, p)) if !p.is_empty() => (c, p),
        _ => (path, DEFAULT_PLATFORM),
    };
    ListingCoordinate {
        candidate,
        platform,
        query,
    }
}

/// The versions a `versions/all` body names, in document order.
///
/// Comma-separated, no whitespace, no trailing newline as upstream sends it —
/// but a body with either is read the same way, because the client splits on
/// `,` and trims nothing.
pub fn versions_in_csv(body: &str) -> Vec<String> {
    body.trim()
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_eight_platforms_parse_and_nothing_else_does() {
        for p in PLATFORMS {
            assert_eq!(parse_platform(p), Some(*p));
        }
        assert_eq!(parse_platform("linux"), None);
        assert_eq!(parse_platform("LinuxX64"), None);
        assert_eq!(parse_platform("../x"), None);
    }

    #[test]
    fn the_candidate_is_what_comes_before_the_platform_or_the_query() {
        assert_eq!(candidate_of("java/linuxx64"), "java");
        assert_eq!(candidate_of("java/linuxx64?current=&installed="), "java");
        assert_eq!(candidate_of("java"), "java");
        assert_eq!(candidate_of("java?x"), "java");
    }

    #[test]
    fn a_listing_package_splits_back_into_its_parts() {
        assert_eq!(
            split_listing_package("java/darwinarm64?current=21.0.5-tem&installed=21.0.5-tem"),
            ListingCoordinate {
                candidate: "java",
                platform: "darwinarm64",
                query: Some("current=21.0.5-tem&installed=21.0.5-tem"),
            }
        );
        assert_eq!(
            split_listing_package("maven"),
            ListingCoordinate {
                candidate: "maven",
                platform: DEFAULT_PLATFORM,
                query: None,
            }
        );
        assert_eq!(
            split_listing_package(&listing_package("java", "linuxx64")).platform,
            "linuxx64"
        );
    }

    #[test]
    fn a_csv_is_read_as_the_client_reads_it() {
        assert_eq!(
            versions_in_csv("3.9.8,3.9.9,4.0.0-rc-6"),
            ["3.9.8", "3.9.9", "4.0.0-rc-6"]
        );
        assert_eq!(versions_in_csv("3.9.9\n"), ["3.9.9"]);
        assert!(versions_in_csv("").is_empty());
    }
}
