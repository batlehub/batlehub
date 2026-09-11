#![no_main]
//! `coordinate_from_filename` (RFC 0021 §11 q8): the coordinate a release
//! import publishes under, read from nothing but an asset's file name.
//!
//! The module's tests check one file name per ecosystem. This builds file names
//! the other way round — a name and a version drawn from each convention's
//! legal alphabet, formatted the way that ecosystem's own tooling would — and
//! asserts the parse gives back exactly what went in. A wrong split here is a
//! package published under a neighbour's name, which the import then serves.
//!
//! - **Constructive:** for every convention, `parse(format(name, version))`
//!   is `(kind, name, version)`, with the wheel's `_`→`-` fold applied.
//! - **Arbitrary:** any string parses without panicking, and whatever comes
//!   out names a `RegistryKind` this build has.

use libfuzzer_sys::fuzz_target;

use batlehub_core::entities::RegistryKind;
use batlehub_core::services::release_import::coordinate_from_filename;

/// A string over `alphabet`, 1..=12 characters.
fn word(u: &mut arbitrary::Unstructured<'_>, alphabet: &[u8]) -> arbitrary::Result<String> {
    let len = u.int_in_range(1..=12usize)?;
    let mut out = String::with_capacity(len);
    for _ in 0..len {
        out.push(alphabet[u.int_in_range(0..=alphabet.len() - 1)?] as char);
    }
    Ok(out)
}

const ALNUM: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const ALNUM_DOT_UNDERSCORE: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789._";
const ALNUM_DASH: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789-";
const DIGITS_DOT: &[u8] = b"0123456789.";
/// pacman's `pkgver`: no dash (it is the separator) and no letters, so it can
/// never spell the `.pkg.tar` the parser locates the stem by.
const PKGVER: &[u8] = b"0123456789._";
const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// A version that begins with a digit: `1.2.3`, `0.9rc1`, `2.0.0.15`.
fn version(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<String> {
    let mut v = String::from((b'0' + u.int_in_range(0..=9u8)?) as char);
    v.push_str(&word(u, DIGITS_DOT)?);
    if u.arbitrary::<bool>()? {
        v.push_str(["rc1", "b2", ".post1", "SNAPSHOT", "beta"][u.int_in_range(0..=4usize)?]);
    }
    Ok(v)
}

fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);

    // Arbitrary: no panic, and a kind this build knows.
    let Ok(raw): arbitrary::Result<String> = u.arbitrary() else {
        return;
    };
    if let Some(found) = coordinate_from_filename(&raw) {
        assert!(
            found.registry_type.parse::<RegistryKind>().is_ok(),
            "{raw:?} parsed to unknown registry type {:?}",
            found.registry_type
        );
    }

    // Constructive: format per convention, parse, compare.
    let Ok(convention) = u.int_in_range(0..=9u8) else {
        return;
    };
    let (file_name, kind, name, version) = match convention {
        0 => {
            // `<id>.<version>.nupkg` — id segments start with a letter.
            let Ok(segments) = u.int_in_range(1..=3u8) else {
                return;
            };
            let mut id = Vec::new();
            for _ in 0..segments {
                let Ok(first) = word(&mut u, LETTERS) else {
                    return;
                };
                let Ok(rest) = word(&mut u, ALNUM) else {
                    return;
                };
                id.push(format!("{first}{rest}"));
            }
            let id = id.join(".");
            let Ok(ver) = version(&mut u) else { return };
            (format!("{id}.{ver}.nupkg"), "nuget", id, ver)
        }
        1 => {
            // `<name>-<version>-<py>-<abi>-<platform>.whl` — name has `_`, not `-`.
            let Ok(name) = word(&mut u, ALNUM_DOT_UNDERSCORE) else {
                return;
            };
            let Ok(ver) = word(&mut u, DIGITS_DOT) else {
                return;
            };
            (
                format!("{name}-{ver}-py3-none-any.whl"),
                "pypi",
                name.replace('_', "-"),
                ver,
            )
        }
        2..=5 => {
            // `<name>-<version>.<ext>` — the name may carry dashes, the version may not.
            let (ext, kind) = [
                (".gem", "rubygems"),
                (".tgz", "npm"),
                (".crate", "cargo"),
                (".vsix", "openvsx"),
            ][(convention - 2) as usize];
            let Ok(name) = word(&mut u, ALNUM_DASH) else {
                return;
            };
            let Ok(ver) = word(&mut u, ALNUM_DOT_UNDERSCORE) else {
                return;
            };
            (format!("{name}-{ver}{ext}"), kind, name, ver)
        }
        6 => {
            // `<name>-<pkgver>-<pkgrel>-<arch>.pkg.tar.zst`
            let Ok(name) = word(&mut u, ALNUM_DASH) else {
                return;
            };
            let Ok(pkgver) = word(&mut u, PKGVER) else {
                return;
            };
            let Ok(pkgrel) = word(&mut u, DIGITS_DOT) else {
                return;
            };
            let Ok(arch) = word(&mut u, ALNUM) else {
                return;
            };
            let ext = [".pkg.tar.zst", ".pkg.tar.xz", ".pkg.tar.gz"][pkgrel.len() % 3];
            (
                format!("{name}-{pkgver}-{pkgrel}-{arch}{ext}"),
                "pacman",
                name,
                format!("{pkgver}-{pkgrel}"),
            )
        }
        7 => {
            // `<name>-<version>-<build>.conda` / `.tar.bz2`
            let Ok(name) = word(&mut u, ALNUM_DASH) else {
                return;
            };
            let Ok(ver) = word(&mut u, ALNUM_DOT_UNDERSCORE) else {
                return;
            };
            let Ok(build) = word(&mut u, ALNUM_DOT_UNDERSCORE) else {
                return;
            };
            let ext = if build.len() % 2 == 0 {
                ".conda"
            } else {
                ".tar.bz2"
            };
            (format!("{name}-{ver}-{build}{ext}"), "conda", name, ver)
        }
        _ => {
            // `.deb` / `.rpm`: the server reads the real coordinate from the
            // archive, so the name is the file name and the version is empty.
            let Ok(stem) = word(&mut u, ALNUM_DASH) else {
                return;
            };
            let ext = if convention == 8 { "deb" } else { "rpm" };
            let file_name = format!("{stem}.{ext}");
            (file_name.clone(), ext, file_name, String::new())
        }
    };

    let found = coordinate_from_filename(&file_name)
        .unwrap_or_else(|| panic!("{file_name:?} ({kind}) did not parse"));
    assert_eq!(
        found.registry_type, kind,
        "{file_name:?} read as the wrong ecosystem"
    );
    assert_eq!(found.name, name, "{file_name:?}: wrong name");
    assert_eq!(found.version, version, "{file_name:?}: wrong version");
    assert_eq!(found.is_publishable(), !version.is_empty());
});
