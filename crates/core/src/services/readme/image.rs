//! What may come back when this server fetches a README's image.
//!
//! The allow-list and the DTO live in `core` rather than beside the HTTP client
//! that fills them because three layers need them and none of them is the HTTP
//! client: the endpoint echoes the type, the SVG sanitiser is chosen by it, and
//! the tests fake the whole thing.
//!
//! *What an image is* is this file's decision and stays here — it is about
//! README images, and nothing else fetches one. What to do with the awkward one
//! is [`crate::services::svg`]'s, and moved there when the marketplace's icons
//! became its second caller (RFC 0007-bis §11 q1).

use crate::services::svg::SVG_CONTENT_TYPE;

/// An image a README pointed at, fetched by this server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedImage {
    /// The `Content-Type` to echo, taken from [`IMAGE_TYPES`] rather than from
    /// the response — an upstream that answered `image/png; charset=utf-8` gets
    /// the canonical spelling, and one that answered something not on the list
    /// never got this far.
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

impl FetchedImage {
    pub fn is_svg(&self) -> bool {
        self.content_type == SVG_CONTENT_TYPE
    }
}

/// Every `Content-Type` an image response may carry.
///
/// `image/svg+xml` is on it, and RFC 0007-bis was drafted saying it would not
/// be. Two-thirds of README images are SVG (§13.2), so excluding them makes
/// `remote_images = "proxy"` refuse the case that motivated it: a badge row that
/// renders as three broken images and a PNG. It is served **sanitised**
/// ([`crate::services::svg`]) and under a `sandbox` CSP, which are §7.2's two independent
/// controls, either sufficient on its own.
///
/// No `image/x-icon`, no `image/bmp`, no `image/tiff`: none appears in a README,
/// and every entry here is a decoder an operator's browser is asked to run on
/// bytes a package author chose.
pub const IMAGE_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/avif",
    SVG_CONTENT_TYPE,
];

/// The canonical spelling of `raw`, if it names an allow-listed image type.
///
/// Compared on the media type alone: a parameter (`; charset=utf-8`) is not part
/// of the decision, and matching the whole header would refuse a perfectly good
/// PNG on a technicality. Case-insensitive, because the header is.
pub fn image_content_type(raw: &str) -> Option<&'static str> {
    let media = raw.split(';').next()?.trim().to_ascii_lowercase();
    IMAGE_TYPES.iter().copied().find(|t| *t == media)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_media_type_is_matched_without_its_parameters() {
        assert_eq!(image_content_type("image/png"), Some("image/png"));
        assert_eq!(
            image_content_type("image/svg+xml; charset=utf-8"),
            Some("image/svg+xml")
        );
        assert_eq!(image_content_type("  IMAGE/PNG  "), Some("image/png"));
    }

    #[test]
    fn anything_that_is_not_an_allow_listed_image_is_refused() {
        for raw in [
            "text/html",
            "application/octet-stream",
            "image/x-icon",
            "image/bmp",
            "text/html; charset=utf-8",
            "",
            "image/png/../../etc",
        ] {
            assert_eq!(image_content_type(raw), None, "{raw} should be refused");
        }
    }

    /// The echoed type is a `&'static str` from the list, so a handler cannot
    /// accidentally reflect the upstream's own header into a response.
    #[test]
    fn the_echoed_type_is_the_lists_own_string() {
        let matched = image_content_type("Image/JPEG; q=1").unwrap();
        assert!(IMAGE_TYPES.contains(&matched));
        assert_eq!(matched, "image/jpeg");
    }
}

/// Whether an image URL's host is one this registry will fetch from.
///
/// **Empty list means every host.** That is the reading that keeps
/// `remote_images = "proxy"` doing what it did before the list existed; an
/// operator narrows it by naming hosts, and naming none is not the same as
/// naming nothing (RFC 0013 §4.2).
///
/// An entry matches the host itself or any subdomain of it, so `shields.io`
/// covers `img.shields.io` and does **not** cover `notshields.io` — the dot is
/// required, which is the difference between a suffix rule and a substring one.
/// Comparison is ASCII-case-insensitive because hosts are.
///
/// A URL with no host — a relative `src`, a `data:` URI — is not allowed by this
/// function. Those are dropped earlier for their own reasons, and answering
/// "yes" for something with no host to check would make this the wrong kind of
/// default.
pub fn image_host_allowed(url: &str, allowed: &[String]) -> bool {
    if allowed.is_empty() {
        return true;
    }
    // The renderer's parser, so the host this matches on is the same string the
    // chip would have named — one reading of a URL, not two. The port is not
    // part of the host for this comparison: `shields.io:8443` is `shields.io`.
    let Some(host) = super::render::url_host(url) else {
        return false;
    };
    let host = host
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    allowed.iter().any(|entry| {
        let entry = entry.trim().trim_start_matches('.').to_ascii_lowercase();
        !entry.is_empty() && (host == entry || host.ends_with(&format!(".{entry}")))
    })
}

#[cfg(test)]
mod host_tests {
    use super::*;

    fn list(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn an_empty_list_allows_every_host() {
        assert!(image_host_allowed("https://anything.example/x.png", &[]));
    }

    #[test]
    fn an_entry_matches_its_own_host_and_its_subdomains() {
        let allowed = list(&["shields.io", "codecov.io"]);
        assert!(image_host_allowed("https://shields.io/a.svg", &allowed));
        assert!(image_host_allowed("https://img.shields.io/a.svg", &allowed));
        assert!(image_host_allowed("http://CODECOV.IO/badge.svg", &allowed));
    }

    /// The dot is the whole rule: a suffix match without it would let
    /// `notshields.io` — a host an attacker can register — through a list that
    /// names `shields.io`.
    #[test]
    fn a_host_that_merely_ends_with_the_entry_is_refused() {
        let allowed = list(&["shields.io"]);
        assert!(!image_host_allowed("https://notshields.io/a.svg", &allowed));
        assert!(!image_host_allowed(
            "https://shields.io.evil.test/a.svg",
            &allowed
        ));
    }

    /// Userinfo and ports are not the host, and neither is a path that happens
    /// to contain an allowed name.
    #[test]
    fn the_host_is_read_from_the_authority_only() {
        let allowed = list(&["shields.io"]);
        assert!(image_host_allowed(
            "https://shields.io:8443/a.svg",
            &allowed
        ));
        assert!(!image_host_allowed(
            "https://evil.test/shields.io/a.svg",
            &allowed
        ));
        assert!(!image_host_allowed(
            "https://shields.io@evil.test/a.svg",
            &allowed
        ));
    }

    #[test]
    fn something_with_no_host_is_not_allowed() {
        let allowed = list(&["shields.io"]);
        assert!(!image_host_allowed("/relative.png", &allowed));
        assert!(!image_host_allowed("data:image/png;base64,AAAA", &allowed));
    }
}
