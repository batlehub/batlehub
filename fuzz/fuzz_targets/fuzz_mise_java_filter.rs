#![no_main]
//! `services::mise_java`: the metadata document and the `(host, path)` keys
//! the download route looks rows up by (RFC 0048 §7.3).
//!
//! 1. **The document round-trips** — what `parse_document` accepts serialises
//!    and parses back to the same rows: the served document is the parsed
//!    rows re-serialised.
//! 2. **A kept row is fetchable only where it was allowed** — every row
//!    `filter_document` keeps has an `https://` URL whose key is under a
//!    vendor host, with no query, fragment, `..` or `.` segment, and an empty
//!    allowlist keeps nothing.
//! 3. **A key is safe before it is a storage key** — whatever `host_path`
//!    accepts, decoded by `index_key`, passes `validate_path_safe`, unless the
//!    decoding itself produced the separator or the dot segment it hides.

use libfuzzer_sys::fuzz_target;

use batlehub_core::services::mise_java::{
    filter_document, host_path, index_key, parse_document, vendor_allows, Row, DEFAULT_VENDOR_HOSTS,
};

fuzz_target!(|data: &[u8]| {
    if let Some(rows) = parse_document(data) {
        let wire = serde_json::to_vec(&rows).expect("parsed rows serialise");
        let back: Vec<Row> = parse_document(&wire).expect("serialised rows parse");
        assert_eq!(back, rows);
        let hosts: Vec<String> = DEFAULT_VENDOR_HOSTS.iter().map(|s| s.to_string()).collect();
        let (kept, dropped) = filter_document(rows.clone(), &hosts);
        assert_eq!(kept.len() + dropped.host + dropped.invalid, rows.len());
        for row in &kept {
            let hp = host_path(&row.url).expect("a kept row is addressable");
            assert!(vendor_allows(&index_key(hp), &hosts), "{}", row.url);
        }
        assert!(filter_document(rows, &[]).0.is_empty());
    }
    if let Ok(s) = std::str::from_utf8(data) {
        if let Some(hp) = host_path(s) {
            assert!(s.starts_with("https://"));
            assert!(!hp.contains(['?', '#']), "{s:?}");
            assert!(!hp.split('/').any(|seg| seg == ".." || seg == "."), "{s:?}");
            let key = index_key(hp);
            if !hp.contains('%') {
                assert!(
                    batlehub_core::services::validate_path_safe("artifact", &key).is_ok(),
                    "{s:?}"
                );
            }
        }
    }
});
