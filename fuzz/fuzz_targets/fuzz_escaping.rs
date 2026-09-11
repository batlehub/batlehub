#![no_main]
//! The four small encoders that stand between a publisher's string and a
//! consumer that interprets it — an HTML attribute, a URL path, a spreadsheet
//! cell, an HTTP header.
//!
//! Each has a handful of unit tests naming the characters the author thought
//! of. Each property here is stated with an **independent decoder** written in
//! this file, so the check is "the consumer reads back exactly what was meant"
//! rather than "the output contains the substrings the tests expect":
//!
//! - `escape_html`: no `<`, `>`, `"` or `'` survives, every `&` opens one of
//!   the five entities, and decoding the entities gives the input back.
//! - `percent_encode_path_segment`: the output is only unreserved bytes and
//!   `%XX` with upper-case hex, contains none of `/ ? # % +`, and decoding
//!   gives the input bytes back.
//! - `csv::field`: an RFC 4180 reader recovers the input, or the input with a
//!   leading apostrophe when — and only when — it led with a formula
//!   character; a quoted cell is closed; an unquoted cell carries no separator.
//! - `parse_cache_control`: a directive is reported only if the header names
//!   it, and `max-age` never exceeds the number that was written.

use libfuzzer_sys::fuzz_target;

use batlehub_core::services::cache_control::parse_cache_control;
use batlehub_core::services::csv;
use batlehub_core::services::escaping::{escape_html, percent_encode_path_segment};

fn unescape_html(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let (ch, len) = if rest.starts_with("&amp;") {
            ('&', 5)
        } else if rest.starts_with("&lt;") {
            ('<', 4)
        } else if rest.starts_with("&gt;") {
            ('>', 4)
        } else if rest.starts_with("&quot;") {
            ('"', 6)
        } else if rest.starts_with("&#39;") {
            ('\'', 5)
        } else {
            return None;
        };
        out.push(ch);
        rest = &rest[len..];
    }
    out.push_str(rest);
    Some(out)
}

fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hi = *b.get(i + 1)? as char;
            let lo = *b.get(i + 2)? as char;
            if !(hi.is_ascii_uppercase() || hi.is_ascii_digit())
                || !(lo.is_ascii_uppercase() || lo.is_ascii_digit())
            {
                return None;
            }
            out.push((hi.to_digit(16)? * 16 + lo.to_digit(16)?) as u8);
            i += 3;
        } else {
            match b[i] {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    out.push(b[i])
                }
                _ => return None,
            }
            i += 1;
        }
    }
    Some(out)
}

/// One RFC 4180 cell, read back. `None` when the cell is malformed.
fn csv_read(cell: &str) -> Option<String> {
    if let Some(inner) = cell.strip_prefix('"') {
        let inner = inner.strip_suffix('"')?;
        let mut out = String::new();
        let mut chars = inner.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '"' {
                // A quote inside a quoted cell must be doubled.
                if chars.next()? != '"' {
                    return None;
                }
            }
            out.push(c);
        }
        Some(out)
    } else if cell.contains([',', '"', '\n', '\r']) {
        None
    } else {
        Some(cell.to_owned())
    }
}

fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);
    let Ok(s): arbitrary::Result<String> = u.arbitrary() else {
        return;
    };

    // escape_html
    let html = escape_html(&s);
    assert!(
        !html.contains(['<', '>', '"', '\'']),
        "escape_html left a markup character in {html:?}"
    );
    assert_eq!(
        unescape_html(&html).as_deref(),
        Some(s.as_str()),
        "escape_html output {html:?} does not decode to its input"
    );

    // percent_encode_path_segment
    let seg = percent_encode_path_segment(&s);
    assert!(
        !seg.contains(['/', '?', '#', '+', ' ']),
        "a path segment can leave its segment: {seg:?}"
    );
    assert_eq!(
        percent_decode(&seg).as_deref(),
        Some(s.as_bytes()),
        "percent-encoded {seg:?} does not decode to its input bytes"
    );

    // csv::field
    let cell = csv::field(&s);
    let read =
        csv_read(&cell).unwrap_or_else(|| panic!("csv::field produced a malformed cell {cell:?}"));
    let formula_lead = s.starts_with(['=', '+', '-', '@', '\t', '\r']);
    if formula_lead {
        assert_eq!(
            read,
            format!("'{s}"),
            "a formula lead must be apostrophe-guarded: {cell:?}"
        );
    } else {
        assert_eq!(read, s, "csv::field changed a plain value: {cell:?}");
    }
    assert!(
        !read.starts_with(['=', '+', '-', '@', '\t', '\r']),
        "the cell a spreadsheet reads still leads a formula: {read:?}"
    );

    // parse_cache_control
    let d = parse_cache_control(&s);
    let lower = s.to_ascii_lowercase();
    if d.no_store {
        assert!(lower.contains("no-store"), "no_store reported for {s:?}");
    }
    if d.no_cache {
        assert!(lower.contains("no-cache"), "no_cache reported for {s:?}");
    }
    if let Some(age) = d.max_age {
        assert!(lower.contains("max-age"), "max_age reported for {s:?}");
        // The number it reports is one the header spelled out, whole.
        assert!(
            lower
                .split(',')
                .filter_map(|t| t.trim().strip_prefix("max-age"))
                .filter_map(|t| t.trim().strip_prefix('='))
                .any(|t| t.trim().parse::<u64>().ok() == Some(age)),
            "max_age {age} is not a value {s:?} carries"
        );
    }
});
