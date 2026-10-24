#![no_main]
//! Storage-key injectivity across every key family a local registry writes.
//!
//! A storage key is the only thing that separates one coordinate's bytes from
//! another's, so two coordinates a handler accepts must never share one — the
//! second write replaces the first, across owners — and the keys a *delete*
//! touches must never reach another coordinate's bytes. This target builds two
//! coordinates from any family, filters each through **that family's own edge
//! validation** (what the handlers call before building the key), and holds the
//! key builders to three properties:
//!
//! 1. **Equal keys mean the same coordinate.** Within a family and across the
//!    families that share a registry kind (a Terraform registry holds module
//!    rows, provider rows and provider binaries under one prefix).
//! 2. **No key is another coordinate's signature sibling.** Deleting a version
//!    also deletes `{key}.sigzip` and `{key}.sigzip.upstream`
//!    (`drop_version_bytes`), so a version stored at another's sibling key was
//!    destroyed by that other version's delete — `a@1.0.sigzip` by `a@1.0`.
//! 3. **A delete prefix reaches only its own version's files.** `{key}/` is
//!    prefix-deleted for the kinds that store files under a version
//!    (`stores_files_under_version`); every key under it must belong to that
//!    same version — a Maven jar, a provider binary, a NAR — and never to
//!    another package whose name happens to continue the path.
//!
//! The proxy cache's `PackageId::cache_key` (the old `fuzz_package_id_cache_key`
//! target) is checked here too: deterministic, carrying its registry, in a
//! namespace (`artifact:`) no local key can enter, and injective between ids
//! whose names have the same number of segments. It is **not** injective in
//! general — `(a, b, Some(c))` and `(a/b, c, None)` share `r/a/b/c` — and that
//! is recorded rather than fixed: changing the format would orphan every cached
//! artifact, and exploiting it needs one upstream to resolve both shapes — a
//! GitLab project `g/p` with a release *and* a project `g/p/{tag}`, which
//! GitLab refuses (a project and a group cannot share a path), or a Go module
//! whose version is the literal `zip`.

use arbitrary::{Arbitrary, Result, Unstructured};
use libfuzzer_sys::fuzz_target;

use batlehub_core::entities::{PackageId, RegistryKind};
use batlehub_core::services::proxy::proxy_artifact_key;
use batlehub_core::services::{
    artifact_storage_key, maven_artifact_storage_key, nix_nar_storage_key, nix_narinfo_storage_key,
    stores_files_under_version, terraform_provider_binary_storage_key, validate_coordinate,
    validate_package_name, validate_path_safe, validate_path_segment, validate_publish_coordinate,
    validate_terraform_platform, VERSION_SIBLING_SUFFIXES,
};

/// Fragments that make collisions likely: separators, the sibling suffixes, the
/// fixed Terraform prefixes. A collision needs two strings to agree byte for
/// byte, which uniform random strings almost never do.
const TOKENS: &[&str] = &[
    "a",
    "b",
    "1.0",
    "/",
    "-",
    ".",
    "_",
    ":",
    "@",
    "sigzip",
    ".sigzip",
    ".upstream",
    ".sigzip.upstream",
    "providers",
    "modules",
    ".narinfo",
    "%2F",
    "x86",
    "linux",
];

fn piece(u: &mut Unstructured) -> Result<String> {
    let mut s = String::new();
    for _ in 0..u.int_in_range(1..=4)? {
        if u.ratio(1, 8)? {
            s.push_str(&String::arbitrary(u)?);
        } else {
            s.push_str(u.choose(TOKENS)?);
        }
    }
    Ok(s)
}

/// One coordinate a handler can write, by key family.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Coord {
    /// A version row through `publish()`/`enforce_publish_policy`, stored at
    /// `artifact_storage_key`. `kind` decides whether it has a delete prefix.
    Row {
        kind: RegistryKind,
        name: String,
        version: String,
    },
    /// A non-POM Maven file (`maven_put`): `{name}/{version}/{filename}`.
    MavenFile {
        name: String,
        version: String,
        filename: String,
    },
    /// A Terraform provider platform binary.
    TfBinary {
        ns: String,
        ptype: String,
        version: String,
        os: String,
        arch: String,
    },
    /// A Nix NAR and the narinfo beside it.
    NixNar {
        name: String,
        version: String,
        hash: String,
        file: String,
    },
    NixNarinfo {
        name: String,
        version: String,
        hash: String,
    },
}

/// The registry kinds modelled, each with the name grammar its publish edge
/// actually enforces.
const KINDS: &[RegistryKind] = &[
    RegistryKind::Npm,
    RegistryKind::Openvsx,
    RegistryKind::Maven,
    RegistryKind::Terraform,
    RegistryKind::Nix,
];

/// Nix store names and hashes are restricted alphabets (`services::nix`), so
/// neither ever carries a `/`; `DrvName::split` cuts the version off the name.
fn nix_safe(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')
        })
}

impl Coord {
    fn arbitrary_valid(u: &mut Unstructured) -> Result<Option<Self>> {
        let kind = *u.choose(KINDS)?;
        match (kind, u.int_in_range(0..=2)?) {
            // npm/openvsx: a name from the request path (`%2F` decoded), a
            // version from the body; nothing beyond the publish coordinate.
            (RegistryKind::Npm | RegistryKind::Openvsx, _) => {
                let (name, version) = (piece(u)?, piece(u)?);
                if validate_publish_coordinate(&name, &version).is_err() {
                    return Ok(None);
                }
                Ok(Some(Coord::Row {
                    kind,
                    name,
                    version,
                }))
            }
            (RegistryKind::Maven, n) => Self::maven(u, n),
            (RegistryKind::Terraform, n) => Self::terraform(u, n),
            (_, n) => Self::nix(u, n),
        }
    }

    /// Maven: `parse_maven_path` splits on `/` and joins the group with `.`,
    /// so no part carries a `/`; each is `validate_path_safe`d.
    fn maven(u: &mut Unstructured, n: u8) -> Result<Option<Self>> {
        let (name, version) = (piece(u)?, piece(u)?);
        if name.contains('/')
            || validate_publish_coordinate(&name, &version).is_err()
            || validate_path_segment("version", &version).is_err()
        {
            return Ok(None);
        }
        if n == 0 {
            return Ok(Some(Coord::Row {
                kind: RegistryKind::Maven,
                name,
                version,
            }));
        }
        let filename = piece(u)?;
        if validate_path_segment("filename", &filename).is_err() {
            return Ok(None);
        }
        Ok(Some(Coord::MavenFile {
            name,
            version,
            filename,
        }))
    }

    /// Terraform: every part is `require_segments`-checked; a module is
    /// `modules/{ns}/{name}/{provider}`, a provider `providers/{ns}/{type}`.
    fn terraform(u: &mut Unstructured, n: u8) -> Result<Option<Self>> {
        let (ns, ptype, version) = (piece(u)?, piece(u)?, piece(u)?);
        for part in [&ns, &ptype, &version] {
            if validate_path_segment("part", part).is_err() {
                return Ok(None);
            }
        }
        let name = match n {
            0 => {
                let provider = piece(u)?;
                if validate_path_segment("provider", &provider).is_err() {
                    return Ok(None);
                }
                format!("modules/{ns}/{ptype}/{provider}")
            }
            _ => format!("providers/{ns}/{ptype}"),
        };
        if validate_publish_coordinate(&name, &version).is_err() {
            return Ok(None);
        }
        if n < 2 {
            return Ok(Some(Coord::Row {
                kind: RegistryKind::Terraform,
                name,
                version,
            }));
        }
        let (os, arch) = (piece(u)?, piece(u)?);
        if validate_terraform_platform(&os, &arch).is_err() {
            return Ok(None);
        }
        Ok(Some(Coord::TfBinary {
            ns,
            ptype,
            version,
            os,
            arch,
        }))
    }

    /// Nix: `publish_nix_narinfo` checks the name and version, the NAR file
    /// name is `validate_path_safe`d, and the row goes through the shared
    /// publish coordinate.
    fn nix(u: &mut Unstructured, n: u8) -> Result<Option<Self>> {
        let (name, version, hash) = (piece(u)?, piece(u)?, piece(u)?);
        if !nix_safe(&name)
            || !nix_safe(&version)
            || !nix_safe(&hash)
            || hash.contains('.')
            || validate_package_name(&name).is_err()
            || validate_publish_coordinate(&name, &version).is_err()
        {
            return Ok(None);
        }
        Ok(Some(match n {
            0 => Coord::Row {
                kind: RegistryKind::Nix,
                name,
                version,
            },
            1 => {
                let file = piece(u)?;
                if validate_path_safe("NAR file name", &file).is_err() {
                    return Ok(None);
                }
                Coord::NixNar {
                    name,
                    version,
                    hash,
                    file,
                }
            }
            _ => Coord::NixNarinfo {
                name,
                version,
                hash,
            },
        }))
    }

    fn kind(&self) -> RegistryKind {
        match self {
            Coord::Row { kind, .. } => *kind,
            Coord::MavenFile { .. } => RegistryKind::Maven,
            Coord::TfBinary { .. } => RegistryKind::Terraform,
            Coord::NixNar { .. } | Coord::NixNarinfo { .. } => RegistryKind::Nix,
        }
    }

    /// One registry per kind: a registry has exactly one type, so two kinds
    /// never share a key prefix.
    fn registry(&self) -> &'static str {
        self.kind().as_str()
    }

    fn key(&self) -> String {
        let reg = self.registry();
        match self {
            Coord::Row { name, version, .. } => artifact_storage_key(reg, name, version),
            Coord::MavenFile {
                name,
                version,
                filename,
            } => maven_artifact_storage_key(reg, name, version, filename),
            Coord::TfBinary {
                ns,
                ptype,
                version,
                os,
                arch,
            } => terraform_provider_binary_storage_key(reg, ns, ptype, version, os, arch),
            Coord::NixNar {
                name,
                version,
                hash,
                file,
            } => nix_nar_storage_key(reg, name, version, hash, file),
            Coord::NixNarinfo {
                name,
                version,
                hash,
            } => nix_narinfo_storage_key(reg, name, version, hash),
        }
    }

    /// The version row a file belongs to — what deleting that row should take.
    fn owning_row(&self) -> (RegistryKind, String, String) {
        match self {
            Coord::Row {
                kind,
                name,
                version,
            } => (*kind, name.clone(), version.clone()),
            Coord::MavenFile { name, version, .. } => {
                (RegistryKind::Maven, name.clone(), version.clone())
            }
            Coord::TfBinary {
                ns, ptype, version, ..
            } => (
                RegistryKind::Terraform,
                format!("providers/{ns}/{ptype}"),
                version.clone(),
            ),
            Coord::NixNar { name, version, .. } | Coord::NixNarinfo { name, version, .. } => {
                (RegistryKind::Nix, name.clone(), version.clone())
            }
        }
    }

    /// The keys deleting this row removes besides its own: the signature
    /// siblings, and (for a multi-file kind) everything under `{key}/`.
    fn delete_footprint(&self) -> Option<(Vec<String>, Option<String>)> {
        let Coord::Row { kind, .. } = self else {
            return None;
        };
        let key = self.key();
        let siblings = VERSION_SIBLING_SUFFIXES
            .iter()
            .map(|s| format!("{key}{s}"))
            .collect();
        let prefix = stores_files_under_version(*kind).then(|| format!("{key}/"));
        Some((siblings, prefix))
    }
}

/// A proxy id as `ProxyService::handle` accepts it: a registry name from the
/// config (one segment) and a `validate_coordinate`d coordinate.
fn proxy_id(u: &mut Unstructured) -> Result<Option<PackageId>> {
    let (registry, name, version) = (piece(u)?, piece(u)?, piece(u)?);
    let artifact = if u.arbitrary()? {
        Some(piece(u)?)
    } else {
        None
    };
    if validate_path_segment("registry", &registry).is_err()
        || validate_coordinate(&name, &version, artifact.as_deref()).is_err()
    {
        return Ok(None);
    }
    let id = PackageId::new(&registry, name, version);
    Ok(Some(match artifact {
        Some(a) => id.with_artifact(a),
        None => id,
    }))
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    if let (Ok(Some(a)), Ok(Some(b))) = (
        Coord::arbitrary_valid(&mut u),
        Coord::arbitrary_valid(&mut u),
    ) {
        let (ka, kb) = (a.key(), b.key());

        // 1. Equal keys, same coordinate.
        if ka == kb {
            assert_eq!(a, b, "two coordinates share the storage key {ka:?}");
        }

        for (x, y, ky) in [(&a, &b, &kb), (&b, &a, &ka)] {
            let Some((siblings, prefix)) = x.delete_footprint() else {
                continue;
            };
            // 2. No coordinate lives at another's signature sibling.
            assert!(
                !siblings.contains(ky),
                "{y:?} is stored at {ky:?}, a signature sibling deleted with {x:?}"
            );
            // 3. A delete prefix takes only its own version's files.
            if let Some(prefix) = prefix {
                if ky.starts_with(&prefix) {
                    assert_eq!(
                        y.owning_row(),
                        x.owning_row(),
                        "deleting {x:?} (prefix {prefix:?}) removes {y:?}'s {ky:?}"
                    );
                }
            }
        }
    }

    if let (Ok(Some(p)), Ok(Some(q))) = (proxy_id(&mut u), proxy_id(&mut u)) {
        let (kp, kq) = (p.cache_key(), q.cache_key());
        // Deterministic, and carrying the registry it belongs to.
        assert_eq!(p.cache_key(), kp);
        assert!(kp.starts_with(&format!("{}/", p.registry)));
        // The proxy namespace and the local one are disjoint by prefix.
        assert!(proxy_artifact_key(&p).starts_with("artifact:"));
        // Injective between names of equal arity: the registry is one segment,
        // the version one, and the artifact is the tail.
        if kp == kq && p.name.split('/').count() == q.name.split('/').count() {
            assert_eq!(
                (&p.registry, &p.name, &p.version, &p.artifact),
                (&q.registry, &q.name, &q.version, &q.artifact),
                "cache key {kp:?} is shared"
            );
        }
    }
});
