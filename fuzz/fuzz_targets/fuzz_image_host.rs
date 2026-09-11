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

use libfuzzer_sys::fuzz_target;

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

fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);

    let Ok(host) = label(&mut u) else { return };
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
    let expected = if allowed.is_empty() {
        true
    } else if !is_http {
        false
    } else {
        let host = host.to_ascii_lowercase();
        allowed.iter().any(|e| {
            let e = e.trim().trim_start_matches('.').to_ascii_lowercase();
            !e.is_empty() && (host == e || host.ends_with(&format!(".{e}")))
        })
    };

    assert_eq!(
        image_host_allowed(&url, &allowed),
        expected,
        "url {url:?} against {allowed:?} (host {host:?})"
    );
});
