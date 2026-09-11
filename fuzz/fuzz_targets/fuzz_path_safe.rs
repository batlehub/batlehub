#![no_main]

use libfuzzer_sys::fuzz_target;

use batlehub_core::services::{
    artifact_storage_key, has_traversal_after_decoding, maven_artifact_storage_key,
    validate_coordinate, validate_package_name, validate_path_safe,
};

/// One round of the decoding the guard performs, written independently: every
/// `%XX` with two hex digits becomes the byte, anything else is copied. What the
/// guard does with its own decoder is the thing under test, so this one shares
/// no code with it.
fn decode_once(value: &[u8]) -> Option<Vec<u8>> {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(value.len());
    let mut decoded_any = false;
    let mut i = 0;
    while i < value.len() {
        if value[i] == b'%' && i + 2 < value.len() {
            if let (Some(hi), Some(lo)) = (hex(value[i + 1]), hex(value[i + 2])) {
                out.push(hi * 16 + lo);
                decoded_any = true;
                i += 3;
                continue;
            }
        }
        out.push(value[i]);
        i += 1;
    }
    decoded_any.then_some(out)
}

/// Does any decoding depth of `value` reveal a traversal segment, a NUL, or a
/// backslash? Unbounded on purpose — the guard gives up after eight rounds and
/// rejects, so anything this finds deeper than that the guard rejects for the
/// other reason.
fn reveals_traversal(value: &str) -> bool {
    let mut current = value.as_bytes().to_vec();
    for _ in 0..64 {
        let text = String::from_utf8_lossy(&current);
        if text.contains('\0')
            || text.contains('\\')
            || text.split('/').any(|segment| segment == "..")
        {
            return true;
        }
        match decode_once(&current) {
            Some(next) => current = next,
            None => return false,
        }
    }
    false
}

fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);
    let Ok(name): arbitrary::Result<String> = u.arbitrary() else {
        return;
    };
    let Ok(version): arbitrary::Result<String> = u.arbitrary() else {
        return;
    };
    let Ok(artifact): arbitrary::Result<Option<String>> = u.arbitrary() else {
        return;
    };

    let verdict = validate_coordinate(&name, &version, artifact.as_deref());

    // Soundness — the property every storage backend relies on. Whatever the
    // guard accepts, the key built from it reaches nothing outside its prefix:
    // no `..` segment at any decoding depth, no NUL, no backslash, no absolute
    // path, and every component non-empty so `local:reg//x` cannot alias
    // `local:reg/x`. `has_traversal_after_decoding` is what `ensure_safe_key`
    // in the adapters calls last, so an accepted coordinate must pass it too.
    if verdict.is_ok() {
        let plain = artifact_storage_key("reg", &name, &version);
        let maven = artifact
            .as_deref()
            .map(|a| maven_artifact_storage_key("reg", &name, &version, a));
        for key in std::iter::once(&plain).chain(maven.iter()) {
            assert!(
                !reveals_traversal(key),
                "accepted coordinate builds {key:?}"
            );
            assert!(
                !has_traversal_after_decoding(key),
                "backend guard rejects {key:?}"
            );
            assert!(!key.contains("//"), "empty segment in {key:?}");
            assert!(!key.ends_with('/'), "trailing separator in {key:?}");
        }
        // A version is one path segment; a `/` in it would let `1.0/../2.0`
        // style coordinates address a sibling even without a `..`.
        assert!(
            !version.contains('/'),
            "accepted version {version:?} has a separator"
        );
        assert!(!name.is_empty() && !version.is_empty());
        assert!(
            !name.starts_with('/') && !name.ends_with('/'),
            "name {name:?}"
        );
    }

    // Completeness — the independent decoder's view. Anything that reveals a
    // traversal at any depth must be rejected. (The converse is not asserted:
    // the guard also rejects on its own grounds, such as more than eight rounds
    // of encoding, and that is a policy this target does not second-guess.)
    for (kind, value) in [("name", name.as_str()), ("version", version.as_str())]
        .into_iter()
        .chain(artifact.as_deref().map(|a| ("artifact", a)))
    {
        if reveals_traversal(value) {
            assert!(
                verdict.is_err(),
                "{kind} {value:?} reveals a traversal and was accepted"
            );
        }
    }

    // The two entry points agree: a package name is a coordinate whose other
    // components are known-good, and the per-kind check is what both call.
    assert_eq!(
        validate_package_name(&name).is_ok(),
        validate_coordinate(&name, "1.0.0", None).is_ok(),
        "validate_package_name and validate_coordinate disagree on {name:?}"
    );
    assert_eq!(
        validate_path_safe("package name", &name).is_ok(),
        validate_package_name(&name).is_ok(),
    );
    // Deterministic — the same input gets the same answer twice.
    assert_eq!(
        verdict.is_ok(),
        validate_coordinate(&name, &version, artifact.as_deref()).is_ok()
    );
});
