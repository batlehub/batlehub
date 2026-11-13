#![no_main]
//! `services::godist`: Go's release document and the file names the `godist`
//! route turns into storage keys (RFC 0048 §7.3).
//!
//! 1. **The document round-trips** — what `parse_releases` accepts serialises
//!    and parses back to the same releases: the served `index.json` is the
//!    parsed struct re-serialised, so a value that changed on the way would be
//!    a document the client sees and the cache did not check.
//! 2. **A file name is safe before it is a key** — whatever `parse_file_name`
//!    accepts has a version of `[0-9a-z.]` with no `..`, no separator anywhere,
//!    and an archive name that is the input minus an optional `.sha256` and
//!    starts `go{version}.`.

use libfuzzer_sys::fuzz_target;

use batlehub_core::services::godist::{parse_file_name, parse_releases, Release};

fuzz_target!(|data: &[u8]| {
    if let Some(releases) = parse_releases(data) {
        let wire = serde_json::to_vec(&releases).expect("a parsed document serialises");
        let back: Vec<Release> = parse_releases(&wire).expect("a serialised document parses");
        assert_eq!(back, releases);
        for file in releases.iter().flat_map(|r| &r.files) {
            check_name(&file.filename);
        }
    }
    if let Ok(name) = std::str::from_utf8(data) {
        check_name(name);
    }
});

fn check_name(name: &str) {
    let Some((version, archive, checksum)) = parse_file_name(name) else {
        return;
    };
    assert!(!name.contains('/') && !name.contains('\\'), "{name:?}");
    assert!(!version.contains(".."), "{name:?}");
    assert!(version
        .chars()
        .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() || c == '.'));
    assert_eq!(checksum, name.ends_with(".sha256"));
    let expected = if checksum {
        &name[..name.len() - ".sha256".len()]
    } else {
        name
    };
    assert_eq!(archive, expected);
    assert!(archive.starts_with(&format!("go{version}.")), "{name:?}");
}
