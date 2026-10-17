#![no_main]

//! The invariant the README pipeline exists to hold, asserted directly.
//!
//! For **arbitrary** input, in any of the four formats and under either image
//! policy, the rendered output contains no `<script`, no `on*=` handler and no
//! URL scheme outside the allow-list. That is a stronger statement than the unit
//! tests in `sanitize.rs` make: those check the standard vectors, this checks
//! the ones nobody thought of.
//!
//! It matters because the input is attacker-authored by construction — anyone
//! who can publish to a proxied upstream can write it — and the output is
//! rendered on the console's own origin to a session that is frequently an
//! administrator's (RFC 0007 §7.1).
//!
//!   task fuzz TARGET=fuzz_readme_render MAX_TIME=60

use libfuzzer_sys::fuzz_target;

use batlehub_core::{
    entities::ReadmeFormat,
    services::{
        readme::render::{render, RenderOptions},
        RemoteImagePolicy,
    },
};

/// Every scheme that must never appear in output, in the forms a parser might
/// still resolve. Checked case-insensitively.
const FORBIDDEN_SCHEMES: &[&str] = &["javascript:", "vbscript:", "data:", "file:", "blob:"];

/// Every attribute value in `html`, as written between its quotes.
///
/// A tag is walked quote-aware rather than cut at the next `>`: the serialiser
/// escapes `"` and `&` in a value but not `>`, so `title="a>b"` is one tag.
fn attribute_values(html: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = html.as_bytes();
    let mut i = 0;
    let mut in_tag = false;
    while i < bytes.len() {
        match bytes[i] {
            b'<' if !in_tag => in_tag = true,
            b'>' if in_tag => in_tag = false,
            q @ (b'"' | b'\'') if in_tag && i > 0 && bytes[i - 1] == b'=' => {
                let start = i + 1;
                let len = html[start..].find(q as char).unwrap_or(html.len() - start);
                out.push(&html[start..start + len]);
                i = start + len;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// An attribute value as a browser's URL parser receives it: character
/// references decoded (numeric with or without the `;`, and the named ones
/// that spell a scheme's punctuation or whitespace), then ASCII tab, LF and
/// CR removed *anywhere* and C0 controls and spaces trimmed from the ends —
/// WHATWG URL's own preprocessing. Every other C0 control is removed too,
/// which is stricter than a browser and therefore a sound over-approximation.
fn as_browser_reads(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(amp) = rest.find('&') {
        decoded.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let named = [
            ("&amp;", '&'),
            ("&quot;", '"'),
            ("&apos;", '\''),
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&colon;", ':'),
            ("&tab;", '\t'),
            ("&newline;", '\n'),
            ("&nbsp;", '\u{a0}'),
        ];
        let lower = rest.get(..10).unwrap_or(rest).to_ascii_lowercase();
        if let Some((ent, c)) = named.iter().find(|(e, _)| lower.starts_with(e)) {
            decoded.push(*c);
            rest = &rest[ent.len()..];
            continue;
        }
        let (radix, digits_at) = match rest.as_bytes().get(1..3) {
            Some([b'#', b'x' | b'X']) => (16, 3),
            Some([b'#', _]) => (10, 2),
            _ => (0, 0),
        };
        let digits = rest
            .get(digits_at..)
            .map(|d| {
                d.len()
                    - d.trim_start_matches(|c: char| c.is_digit(radix.max(2)))
                        .len()
            })
            .unwrap_or(0);
        if radix == 0 || digits == 0 {
            decoded.push('&');
            rest = &rest[1..];
            continue;
        }
        let num = u32::from_str_radix(
            // Browsers accept any run of leading zeros: `&#x000000006a;` is `j`.
            &rest[digits_at..digits_at + digits]
                .trim_start_matches('0')
                .chars()
                .take(8)
                .collect::<String>(),
            radix,
        )
        .ok()
        .and_then(char::from_u32)
        .unwrap_or('\u{fffd}');
        decoded.push(num);
        rest = &rest[digits_at + digits..];
        rest = rest.strip_prefix(';').unwrap_or(rest);
    }
    decoded.push_str(rest);
    decoded
        .chars()
        .filter(|c| !c.is_ascii_control())
        .collect::<String>()
        .trim_matches(|c: char| c == ' ')
        .to_ascii_lowercase()
}

fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);

    let Ok(source): arbitrary::Result<String> = u.arbitrary() else {
        return;
    };
    let Ok(format_idx): arbitrary::Result<u8> = u.arbitrary() else {
        return;
    };
    let Ok(proxy_images): arbitrary::Result<bool> = u.arbitrary() else {
        return;
    };
    // Taken from the fuzz input rather than fixed, so libFuzzer can find a host
    // string that matches a URL it also generated. `image_hosts` decides, per
    // image, between rewriting the `src` and emitting a chip — two different
    // output paths, and a hard-coded list would only ever exercise one of them.
    // `None` is the empty list, which is what `proxy` meant before the list
    // existed (RFC 0013 §4.2) and still the default.
    let Ok(image_host): arbitrary::Result<Option<String>> = u.arbitrary() else {
        return;
    };

    let format = match format_idx % 4 {
        0 => ReadmeFormat::Markdown,
        1 => ReadmeFormat::Html,
        2 => ReadmeFormat::Rst,
        _ => ReadmeFormat::Plain,
    };
    // Exhaustive literals in both arms, never `..Default::default()`: a new
    // field on `RenderOptions` must stop this target compiling rather than
    // silently default itself out of the fuzzed surface. That is exactly how
    // `image_hosts` went unfuzzed — the field landed, this file kept building
    // nowhere, and nothing said so until a build was attempted by hand.
    let opts = if proxy_images {
        RenderOptions {
            remote_images: RemoteImagePolicy::Proxy,
            image_proxy_prefix: Some("https://hub.invalid/api/v1/readme-image/".to_owned()),
            image_hosts: image_host.into_iter().collect(),
        }
    } else {
        RenderOptions {
            remote_images: RemoteImagePolicy::Strip,
            image_proxy_prefix: None,
            image_hosts: image_host.into_iter().collect(),
        }
    };

    let out = render(&source, format, &opts);
    let lower = out.to_ascii_lowercase();

    assert!(
        !lower.contains("<script"),
        "script element survived: {out:?} (from {source:?})"
    );

    // Everything below distinguishes *markup* from *text*. A README may
    // legitimately discuss `javascript:` or write `onerror=` in prose, and the
    // escaped-source formats show exactly that — so an occurrence only matters
    // when it is inside a tag. The sanitiser escapes `<` in text, so "the last
    // `<` is more recent than the last `>`" is an exact test rather than a
    // heuristic.
    //
    // Two earlier versions of this check were wrong before the sanitiser ever
    // was: `"data:` as text, then `="data:` as text, both found within seconds.
    // That is the fuzz target earning its place on the *test* rather than on
    // the code.
    let inside_tag = |idx: usize| {
        let before = &lower[..idx];
        before.rfind('<') > before.rfind('>')
    };

    for (idx, _) in lower.match_indices("on") {
        let rest = &lower[idx..];
        let Some(eq) = rest.find('=') else { continue };
        let name = &rest[..eq];
        if !(name.len() <= 24 && name.chars().all(|c| c.is_ascii_alphabetic())) {
            continue;
        }
        // An attribute is preceded by whitespace inside a tag; text content
        // that happens to read "once=" is not one.
        let preceded_by_space = lower[..idx]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
        assert!(
            !(inside_tag(idx) && preceded_by_space),
            "event handler attribute {name:?} survived: {out:?} (from {source:?})"
        );
    }

    for scheme in FORBIDDEN_SCHEMES {
        for (idx, _) in lower.match_indices(scheme) {
            assert!(
                !inside_tag(idx),
                "{scheme} survived inside a tag: {out:?} (from {source:?})"
            );
        }
    }

    // The same check, on attribute values *as the browser reads them*. The
    // loop above reads raw text, so `href="jav&#x61;script:"` or
    // `java&#9;script:` — which a browser decodes, strips, and runs — would pass
    // it. Decoding first closes that gap whatever the sanitiser's serialiser
    // happens to escape today.
    for value in attribute_values(&out) {
        let read = as_browser_reads(value);
        for scheme in FORBIDDEN_SCHEMES {
            assert!(
                !read.contains(scheme),
                "{scheme} survives in attribute {value:?} once decoded ({read:?}): {out:?} (from {source:?})"
            );
        }
    }

    // Rendering is deterministic: the render cache is keyed by content digest
    // plus renderer version, so the same source rendering differently on a
    // second call would serve one reader a different document from another.
    assert_eq!(out, render(&source, format, &opts), "render is not stable");
});
