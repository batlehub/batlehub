#![no_main]
//! Air-gap bundles (RFC 0008-bis): a tar an operator carries across a gap and
//! an import trusts after one signature check.
//!
//! `services/bundle.rs` records a signature bypass that shipped: tar allows
//! duplicate names, `manifest_bytes_of` verified the *first* `manifest.json`
//! and `read_bundle` imported the *last*. The regression test pins that one
//! archive. This holds the property the fix restored over archives nobody
//! built on purpose — a written bundle with up to three bytes flipped, and raw
//! fuzz bytes — plus the round trip a real bundle must survive:
//!
//! 1. **What is verified is what is imported.** Whenever `read_bundle` accepts
//!    an archive, `manifest_bytes_of` accepts the same bytes, and the manifest
//!    it returns parses to the very manifest `read_bundle` returned.
//! 2. **A kept blob is named by its content.** Every entry of `blobs` is a
//!    64-hex key equal to the SHA-256 of its bytes, and the returned manifest
//!    passes its own `validate`.
//! 3. **No panic on any input.** A tar reader over hostile bytes returns an
//!    error, not a stack trace — and never a giant allocation, because a
//!    header claiming a terabyte reads only what is there.
//! 4. **Round trip.** `write_bundle` then `read_bundle` returns the same
//!    manifest and signature, exactly the blobs that were supplied, and
//!    rejects nothing.

use chrono::{DateTime, Utc};
use libfuzzer_sys::fuzz_target;

use batlehub_core::services::bundle::{
    is_sha256_hex, manifest_bytes_of, read_bundle, write_bundle, BundleEntry, BundleManifest,
    BundleRef, BUNDLE_VERSION,
};
use batlehub_core::services::integrity::sha256_hex;

/// A `DateTime` inside years 1–9999, where RFC 3339 serialisation round-trips
/// without a sign-prefixed year.
fn when(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<DateTime<Utc>> {
    let secs = u.int_in_range(-62_000_000_000i64..=253_000_000_000)?;
    let nanos = u.int_in_range(0..=999_999_999u32)?;
    Ok(DateTime::from_timestamp(secs, nanos).expect("inside chrono's range"))
}

/// A storage key `BundleManifest::validate` accepts: non-empty, no `..`, not
/// absolute. Everything else about it is the fuzzer's choice.
fn key(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<String> {
    let mut cleaned: String = u.arbitrary()?;
    while cleaned.contains("..") {
        cleaned = cleaned.replace("..", ".");
    }
    let cleaned = cleaned.trim_start_matches('/').to_owned();
    Ok(if cleaned.is_empty() {
        "k".to_owned()
    } else {
        cleaned
    })
}

fn non_empty(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<String> {
    let raw: String = u.arbitrary()?;
    Ok(if raw.is_empty() { "r".to_owned() } else { raw })
}

/// The invariants any accepted archive must satisfy, whoever built it.
fn check_accepted(bytes: &[u8]) {
    let Ok(read) = read_bundle(bytes) else {
        return;
    };
    for (name, blob) in &read.blobs {
        assert!(
            is_sha256_hex(name),
            "kept a blob under a non-digest name {name:?}"
        );
        assert_eq!(
            &sha256_hex(blob),
            name,
            "kept a blob whose bytes do not hash to its name"
        );
    }
    read.manifest
        .validate()
        .expect("read_bundle returned a manifest its own validate refuses");
    let signed = manifest_bytes_of(bytes)
        .expect("read_bundle accepted an archive manifest_bytes_of refuses");
    let verified: BundleManifest =
        serde_json::from_slice(&signed).expect("the bytes the signature covers are not a manifest");
    assert_eq!(
        verified, read.manifest,
        "the manifest the signature covers is not the one that would be imported"
    );
}

fuzz_target!(|data: &[u8]| {
    // 3. Raw bytes: no panic, and if accepted, honest.
    check_accepted(data);

    let mut u = arbitrary::Unstructured::new(data);

    // 4. Build a manifest the writer will accept, with real blobs.
    let Ok(bundle_version) = u.int_in_range(0..=BUNDLE_VERSION) else {
        return;
    };
    let Ok(bundle_id): arbitrary::Result<String> = u.arbitrary() else {
        return;
    };
    let Ok(created_at) = when(&mut u) else { return };
    let Ok(source_plan): arbitrary::Result<Option<String>> = u.arbitrary() else {
        return;
    };
    let Ok(entry_count) = u.int_in_range(0..=4u8) else {
        return;
    };

    let mut entries = Vec::new();
    let mut blobs: Vec<(String, Option<Vec<u8>>)> = Vec::new();
    for _ in 0..entry_count {
        let Ok(len) = u.int_in_range(0..=64usize) else {
            return;
        };
        let Ok(blob) = u.bytes(len).map(<[u8]>::to_vec) else {
            return;
        };
        let digest = sha256_hex(&blob);
        // Some digests are named but never supplied: the writer leaves the
        // entry in and the reader must simply not have the blob.
        let Ok(supplied) = u.arbitrary::<bool>() else {
            return;
        };
        let Ok(registry) = non_empty(&mut u) else {
            return;
        };
        let Ok(key) = key(&mut u) else { return };
        let Ok(package_name): arbitrary::Result<Option<String>> = u.arbitrary() else {
            return;
        };
        let Ok(version): arbitrary::Result<Option<String>> = u.arbitrary() else {
            return;
        };
        let Ok(verdict): arbitrary::Result<Option<String>> = u.arbitrary() else {
            return;
        };
        let Ok(reason_codes): arbitrary::Result<Vec<String>> = u.arbitrary() else {
            return;
        };
        let verified_at = match u.arbitrary::<bool>() {
            Ok(true) => match when(&mut u) {
                Ok(t) => Some(t),
                Err(_) => return,
            },
            _ => None,
        };
        let git_ref = match u.arbitrary::<bool>() {
            Ok(true) => {
                let Ok(parts): arbitrary::Result<(String, String, String, String)> = u.arbitrary()
                else {
                    return;
                };
                Some(BundleRef {
                    owner_repo: parts.0,
                    git_ref: parts.1,
                    kind: parts.2,
                    sha: parts.3,
                })
            }
            _ => None,
        };
        let facts = match u.arbitrary::<bool>() {
            Ok(true) => {
                let Ok(k): arbitrary::Result<String> = u.arbitrary() else {
                    return;
                };
                let Ok(v): arbitrary::Result<String> = u.arbitrary() else {
                    return;
                };
                Some(serde_json::json!({ k: v }))
            }
            _ => None,
        };
        entries.push(BundleEntry {
            registry,
            key,
            digest: digest.clone(),
            size: blob.len() as u64,
            package_name,
            version,
            verdict,
            reason_codes,
            verified_at,
            git_ref,
            facts,
        });
        blobs.push((digest, supplied.then_some(blob)));
    }
    let manifest = BundleManifest {
        bundle_version,
        bundle_id,
        created_at,
        source_plan,
        entries,
    };
    let Ok(sig_len) = u.int_in_range(0..=64usize) else {
        return;
    };
    let Ok(signature) = u.bytes(sig_len).map(<[u8]>::to_vec) else {
        return;
    };

    let mut archive = Vec::new();
    let written = write_bundle(&mut archive, &manifest, &signature, |digest| {
        // The first supplied copy wins; a digest named twice is one blob.
        blobs
            .iter()
            .find(|(d, b)| d == digest && b.is_some())
            .and_then(|(_, b)| b.clone())
    })
    .expect("writing a manifest that validates must succeed");

    let read = read_bundle(&archive[..]).expect("reading back what was just written");
    assert_eq!(read.manifest, manifest);
    assert_eq!(read.signature, signature);
    assert!(
        read.rejected.is_empty(),
        "rejected {:?} out of a bundle it wrote",
        read.rejected
    );
    let mut expected: std::collections::BTreeMap<String, Vec<u8>> = Default::default();
    for (digest, blob) in &blobs {
        if let Some(bytes) = blob {
            expected
                .entry(digest.clone())
                .or_insert_with(|| bytes.clone());
        }
    }
    assert_eq!(read.blobs, expected);
    assert_eq!(written, expected.len());
    assert_eq!(
        manifest_bytes_of(&archive[..]).expect("manifest bytes of a written bundle"),
        manifest
            .to_signed_bytes()
            .expect("serialising the manifest")
    );

    // 1–3. The written archive, damaged: still no panic, still honest.
    let Ok(edits) = u.int_in_range(0..=3u8) else {
        return;
    };
    for _ in 0..edits {
        let Ok(at) = u.int_in_range(0..=archive.len() - 1) else {
            return;
        };
        let Ok(b): arbitrary::Result<u8> = u.arbitrary() else {
            return;
        };
        archive[at] = b;
    }
    if let Ok(cut) = u.int_in_range(0..=archive.len()) {
        if u.arbitrary::<bool>().unwrap_or(false) {
            archive.truncate(cut);
        }
    }
    check_accepted(&archive);
});
