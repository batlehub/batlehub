#![no_main]
//! `image_host_allowed` (RFC 0013 §4.2): the allow-list that decides which
//! hosts this server will fetch README images from on an operator's behalf —
//! an outbound request, so the wrong answer is a server-side request to a
//! host the operator never named.
//!
//! The rule is short enough to restate independently: an empty list allows
//! everything; otherwise the URL must be `http(s)` with a host, and that host
//! (case-folded, port dropped) must equal an entry or end in `.` + entry. The
//! oracle builds the URL *from* a host it chose — with user-info, a port, a
//! path, a query, a fragment, a case-shuffled scheme, tabs and newlines the
//! parser is documented to strip — and compares against that rule, so the
//! parser under test is never the thing that decides what the host was.
//!
//! A second, **differential** property covers the strings that oracle cannot
//! build: whenever `image_host_allowed` says yes, the host hyper dials — the
//! `url::Url` `fetch_image` parses, re-read as the `http::Uri` reqwest hands
//! the connector — must satisfy the same allow rule. The string is assembled
//! from an allow-listed host and the characters two URL readers are most likely
//! to disagree about: `\`, `@` in user-info or a fragment, `%`-escapes, a
//! trailing `.`, IDNA-mapped dots and letters, IPv4 spellings — or is simply
//! arbitrary. Two readings of one URL is how the `\@` bypass shipped.

use libfuzzer_sys::fuzz_target;
use url::Url;

use batlehub_core::services::readme::image::image_host_allowed;

const HOST_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-";

fn label(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<String> {
    let len = u.int_in_range(1..=16usize)?;
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        out.push(HOST_CHARS[u.int_in_range(0..=HOST_CHARS.len() - 1)?] as char);
    }
    Ok(out)
}

/// A scheme as a browser would read it, with the whitespace the parser strips.
fn scheme(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<(String, bool)> {
    let (s, http): (&str, bool) = [
        ("https", true),
        ("http", true),
        ("HTTPS", true),
        ("Http", true),
        ("ftp", false),
        ("data", false),
        ("javascript", false),
        ("", false),
    ][u.int_in_range(0..=7usize)?];
    let mut out = String::new();
    for c in s.chars() {
        out.push(c);
        if u.int_in_range(0..=7u8)? == 0 {
            out.push(['\t', '\n', '\r'][u.int_in_range(0..=2usize)?]);
        }
    }
    Ok((out, http))
}

/// The allow rule, applied to a host someone else extracted.
fn rule(host: &str, allowed: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    allowed.iter().any(|e| {
        let e = e.trim().trim_start_matches('.').to_ascii_lowercase();
        !e.is_empty() && (host == e || host.ends_with(&format!(".{e}")))
    })
}

/// WHATWG URL's "ends in a number" (§3.5): the last label, after dropping
/// one trailing empty one, is all digits or a `0x` hex number.
fn ends_in_a_number(host: &str) -> bool {
    let mut labels: Vec<&str> = host.split('.').collect();
    if labels.last() == Some(&"") && labels.len() > 1 {
        labels.pop();
    }
    let last = labels.last().copied().unwrap_or_default();
    (!last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()))
        || last
            .strip_prefix("0x")
            .or_else(|| last.strip_prefix("0X"))
            .is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Fragments two URL readers might read differently.
const TRICKY: &[&str] = &[
    "\\",
    "@",
    "%40",
    "%2e",
    "%2E",
    "%2f",
    "%5c",
    ".",
    "..",
    ":",
    ":443",
    "/",
    "?",
    "#",
    "[",
    "]",
    "\u{3002}",
    "\u{ff0e}",
    "\u{ff20}",
    "\u{ff0f}",
    "\u{ad}",
    "\u{17f}",
    "\t",
    " ",
    "\u{a0}",
    "0x7f",
    "1",
    "evil.example",
    "user:pw",
    "xn--",
    "\u{200b}",
];

/// Property 2, the differential: see the module docs.
fn differential(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<()> {
    let allowed = vec!["shields.io".to_owned(), label(u)?];
    let s: String = if u.arbitrary()? {
        u.arbitrary()?
    } else {
        let mut s = ["https://", "http://", "HTTPS://", " https://", "https:/\\"]
            [u.int_in_range(0..=4usize)?]
        .to_owned();
        for _ in 0..u.int_in_range(0..=4u8)? {
            s.push_str(u.choose(TRICKY)?);
        }
        s.push_str(
            ["shields.io", "img.shields.io", "SHIELDS.IO", &allowed[1]]
                [u.int_in_range(0..=3usize)?],
        );
        for _ in 0..u.int_in_range(0..=4u8)? {
            s.push_str(u.choose(TRICKY)?);
        }
        s
    };
    if !image_host_allowed(&s, &allowed) {
        return Ok(());
    }
    let Ok(parsed) = Url::parse(&s) else {
        return Ok(()); // `fetch_image` refuses what it cannot parse.
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return Ok(()); // and does not fetch a non-http scheme.
    }
    // What hyper dials: reqwest hands it the URL re-read as an `http::Uri`.
    // `image_host_allowed` already reads with the `url` crate, so comparing
    // against `parsed.host_str()` would be the function checked against itself.
    let Ok(uri) = parsed.as_str().parse::<http::Uri>() else {
        return Ok(()); // reqwest refuses to send what `http` cannot read.
    };
    let host = uri.host().unwrap_or_default();
    assert!(
        rule(host, &allowed),
        "{s:?} passed the allow-list {allowed:?}, but the fetch would dial {host:?}"
    );
    Ok(())
}

fuzz_target!(|data: &[u8]| {
    let _ = differential(&mut arbitrary::Unstructured::new(data));

    let mut u = arbitrary::Unstructured::new(data);

    let Ok(host) = label(&mut u) else { return };
    // A host that "ends in a number" is an IPv4 address to a WHATWG parser and
    // is dialled as its dotted-quad serialisation (`666` is `0.0.2.154`), so the
    // spelling built here is not the host — the differential above owns those.
    if ends_in_a_number(&host) {
        return;
    }
    let Ok((scheme, is_http)) = scheme(&mut u) else {
        return;
    };
    let Ok(with_userinfo) = u.arbitrary::<bool>() else {
        return;
    };
    let Ok(with_port) = u.arbitrary::<bool>() else {
        return;
    };
    let Ok(tail) = u.int_in_range(0..=4u8) else {
        return;
    };
    let Ok(pad) = u.arbitrary::<bool>() else {
        return;
    };

    let mut url = String::new();
    if pad {
        url.push_str("  ");
    }
    url.push_str(&scheme);
    if !scheme.is_empty() {
        url.push_str("://");
    }
    if with_userinfo {
        url.push_str("user:pa.ss@");
    }
    url.push_str(&host);
    if with_port {
        url.push_str(":8443");
    }
    match tail {
        0 => {}
        1 => url.push_str("/logo.png"),
        2 => url.push_str("?x=1"),
        3 => url.push_str("#frag"),
        _ => url.push_str("\\evil.example/logo.png"),
    }
    if pad {
        url.push('\n');
    }

    let Ok(entries) = u.int_in_range(0..=3u8) else {
        return;
    };
    let mut allowed = Vec::new();
    for _ in 0..entries {
        // Sometimes the host itself, sometimes one of its suffixes, sometimes
        // a stranger — and spelled the way an operator might: leading dot,
        // surrounding spaces, another case.
        let Ok(choice) = u.int_in_range(0..=3u8) else {
            return;
        };
        let mut entry = match choice {
            0 => host.clone(),
            1 => host
                .split_once('.')
                .map(|(_, rest)| rest.to_owned())
                .unwrap_or_else(|| host.clone()),
            2 => match label(&mut u) {
                Ok(l) => l,
                Err(_) => return,
            },
            _ => String::new(),
        };
        if u.arbitrary::<bool>().unwrap_or(false) {
            entry = entry.to_ascii_uppercase();
        }
        if u.arbitrary::<bool>().unwrap_or(false) {
            entry = format!(" .{entry} ");
        }
        allowed.push(entry);
    }

    // The rule, restated.
    let expected = allowed.is_empty() || (is_http && rule(&host, &allowed));

    assert_eq!(
        image_host_allowed(&url, &allowed),
        expected,
        "url {url:?} against {allowed:?} (host {host:?})"
    );
});
