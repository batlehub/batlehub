//! What SDKMAN's API says, and which of its paths this proxy relays.

/// The candidates API `sdkman-init.sh` defaults `SDKMAN_CANDIDATES_API` to.
/// The `/2` is part of the URL rather than synthesised: the client's own
/// variable carries it, and an operator pointing at a beta or a mirror must
/// be able to say so (RFC 0010 §4.1).
pub const DEFAULT_API_BASE: &str = "https://api.sdkman.io/2";

/// The download broker `sdkman-init.sh` defaults `SDKMAN_BROKER_API` to.
pub const DEFAULT_BROKER_BASE: &str = "https://broker.sdkman.io";

/// What `candidates/validate/{c}/{v}/{plat}` answered.
///
/// The API says `valid` or `invalid` and nothing else; anything else is an
/// upstream that is not SDKMAN (a captive portal, an HTML error page), which
/// the caller reports rather than guesses at.
pub fn parse_validate_answer(body: &str) -> Option<bool> {
    match body.trim() {
        "valid" => Some(true),
        "invalid" => Some(false),
        _ => None,
    }
}

/// The API paths served byte-exact under [`DocumentKind::RELAYED`].
///
/// The handlers build these from validated segments, so this is defence in
/// depth rather than the edge: a `RELAYED` request can only ever reach one
/// of the endpoints the client is known to call (RFC 0010 §6.5), never an
/// arbitrary path on the API host.
///
/// [`DocumentKind::RELAYED`]: batlehub_core::ports::DocumentKind::RELAYED
const RELAYED_PREFIXES: &[&str] = &[
    "candidates/all",
    "candidates/list",
    "candidates/validate/",
    "hooks/pre/",
    "hooks/post/",
    "healthcheck",
    "broker/version/sdkman/",
    "selfupdate/",
];

/// Whether `path` is one this proxy relays from the candidates API.
pub fn relayed_path_is_allowed(path: &str) -> bool {
    if path.contains("..") || path.contains('?') || path.contains('#') || path.starts_with('/') {
        return false;
    }
    RELAYED_PREFIXES.iter().any(|prefix| {
        if let Some(rest) = prefix.strip_suffix('/') {
            path.strip_prefix(rest)
                .is_some_and(|tail| tail.starts_with('/') && tail.len() > 1)
        } else {
            path == *prefix
        }
    })
}

/// `{broker}/download/{candidate}/{version}/{platform}`.
pub fn broker_download_url(
    broker_base: &str,
    candidate: &str,
    version: &str,
    platform: &str,
) -> String {
    format!("{broker_base}/download/{candidate}/{version}/{platform}")
}
