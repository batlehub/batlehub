#![no_main]

use libfuzzer_sys::fuzz_target;

use batlehub_core::entities::normalize_url;

// `normalize_url` turns the spellings package manifests use for a repository
// (`git+https://…`, `git@host:o/r.git`, `github:o/r`) into a page the UI links
// to as-is. The invariant is the one its unit test `nothing_that_is_not_http_
// becomes_a_link` states for a handful of literals, over every input: what
// comes out is `http(s)://` with a host, or nothing. A `javascript:` or `data:`
// URL that survived any of the rewrites would be an href in the package page.
fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    let Some(url) = normalize_url(raw) else {
        return;
    };

    let lower = url.to_ascii_lowercase();
    assert!(
        lower.starts_with("http://") || lower.starts_with("https://"),
        "{raw:?} became {url:?}, which is not an http(s) URL"
    );
    let host = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or_default();
    assert!(
        !host.is_empty() && !host.starts_with('/'),
        "{raw:?} became {url:?}, which has no host"
    );
    // Re-normalising the output never turns it into a non-link. It may turn
    // it into nothing — `ssh://.git.git` becomes `https://.git`, whose host is
    // the suffix the next pass strips — but never into a different scheme.
    if let Some(again) = normalize_url(&url) {
        let again_lower = again.to_ascii_lowercase();
        assert!(
            again_lower.starts_with("http://") || again_lower.starts_with("https://"),
            "{url:?} re-normalised to {again:?}"
        );
    }
});
