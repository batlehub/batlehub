//! What an artifact's file name says it is (RFC 0021 §11 q8).
//!
//! Two kinds of ecosystem, and the difference decides how an import reads a
//! coordinate. A VSIX names itself *inside* the archive, so the bytes have to
//! arrive first. A wheel, a gem, a `.nupkg` and an npm tarball name themselves
//! in the file name, by a convention each ecosystem's own tooling produces —
//! which is a coordinate an import can read **before** downloading anything, so
//! a release whose every asset is already held costs one API call.
//!
//! These rules were the CLI's: `batlehub publish some.nupkg` has always worked
//! out what it was publishing this way. Moving them here makes the same
//! sentence a server-side contract, and `cli/src/api/publish.rs` now delegates
//! rather than keeping a second copy — two implementations of "this filename is
//! that coordinate" would drift, and the one an import used would be the one
//! nobody was testing against a real package manager.
//!
//! Deliberately absent, and it is not an oversight: **Maven** needs a `groupId`
//! no filename carries, and a **Composer** zip shares its extension with
//! everything else — which is why the CLI makes `--type` mandatory for it. An
//! import of those kinds refuses rather than guesses (§4.2).

/// A coordinate read out of a file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilenameCoordinate {
    /// The `RegistryKind::as_str()` this file name belongs to.
    pub registry_type: String,
    pub name: String,
    /// Empty when the ecosystem's server reads the real version from the
    /// package's own metadata (`.deb`, `.rpm`, conda), in which case this is
    /// cosmetic and the caller must not publish under it.
    pub version: String,
}

impl FilenameCoordinate {
    fn new(registry_type: &str, name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            registry_type: registry_type.to_owned(),
            name: name.into(),
            version: version.into(),
        }
    }

    /// Whether this is a coordinate something may be published under.
    ///
    /// An empty version is the marker for "the server reads the real one from
    /// the archive": useful to the CLI, which is only choosing an endpoint, and
    /// useless to an import, which has to name the version it is creating.
    pub fn is_publishable(&self) -> bool {
        !self.name.is_empty() && !self.version.is_empty()
    }
}

/// The ecosystem, name and version a file name declares.
///
/// Two conventions do not fit an extension match and are asked first: a pacman
/// package and a conda one both carry a compound suffix an `extension()` call
/// only sees the tail of.
pub fn coordinate_from_filename(file_name: &str) -> Option<FilenameCoordinate> {
    if let Some(found) = pacman_coordinate(file_name) {
        return Some(found);
    }
    if let Some(found) = conda_coordinate(file_name) {
        return Some(found);
    }
    match file_name.rsplit_once('.')?.1 {
        "nupkg" => nuget_coordinate(file_name),
        "whl" => wheel_coordinate(file_name),
        "gem" => split_on_last_dash("rubygems", file_name, ".gem"),
        "tgz" => split_on_last_dash("npm", file_name, ".tgz"),
        "crate" => split_on_last_dash("cargo", file_name, ".crate"),
        // A VSIX *does* follow `<extension_id>-<version>.vsix` often enough for
        // the CLI to choose an endpoint by it — but an import publishes under
        // what it reads, and a name that is merely usually right is the worst
        // kind of guess. `VsixCoordinates` reads the manifest instead, and this
        // arm stays for the CLI's dispatch.
        "vsix" => split_on_last_dash("openvsx", file_name, ".vsix"),
        // The server reads name and version from the package's own control or
        // header data, so these are cosmetic and always succeed: the CLI needs
        // only the registry type to pick an endpoint.
        ext @ ("deb" | "rpm") => Some(FilenameCoordinate::new(ext, file_name, "")),
        _ => None,
    }
}

/// `<name>-<pkgver>-<pkgrel>-<arch>.pkg.tar.{zst,xz,gz}`.
fn pacman_coordinate(file_name: &str) -> Option<FilenameCoordinate> {
    if !(file_name.ends_with(".pkg.tar.zst")
        || file_name.ends_with(".pkg.tar.xz")
        || file_name.ends_with(".pkg.tar.gz"))
    {
        return None;
    }
    let stem = &file_name[..file_name.find(".pkg.tar")?];
    let parts: Vec<&str> = stem.rsplitn(4, '-').collect(); // [arch, pkgrel, pkgver, name]
    if parts.len() != 4 || parts[3].is_empty() {
        return None;
    }
    Some(FilenameCoordinate::new(
        "pacman",
        parts[3],
        format!("{}-{}", parts[2], parts[1]),
    ))
}

/// `<name>-<version>-<build>.tar.bz2` (legacy) or `.conda`.
///
/// The server derives the real subdir and platform from the archive's own
/// metadata, so what is read here is cosmetic — and an unsplittable name still
/// answers, because the CLI needs only the registry type.
fn conda_coordinate(file_name: &str) -> Option<FilenameCoordinate> {
    let stem = file_name
        .strip_suffix(".tar.bz2")
        .or_else(|| file_name.strip_suffix(".conda"))?;
    let parts: Vec<&str> = stem.rsplitn(3, '-').collect(); // [build, version, name]
    Some(if parts.len() == 3 {
        FilenameCoordinate::new("conda", parts[2], parts[1])
    } else {
        FilenameCoordinate::new("conda", stem, "")
    })
}

/// `<id>.<version>.nupkg`: the version starts at the first dot-delimited
/// segment beginning with a digit.
fn nuget_coordinate(file_name: &str) -> Option<FilenameCoordinate> {
    let stem = file_name.strip_suffix(".nupkg")?;
    let parts: Vec<&str> = stem.split('.').collect();
    let version_start = parts
        .iter()
        .position(|s| s.starts_with(|c: char| c.is_ascii_digit()))
        .filter(|at| *at > 0)?;
    Some(FilenameCoordinate::new(
        "nuget",
        parts[..version_start].join("."),
        parts[version_start..].join("."),
    ))
}

/// `<name>-<version>-<python tag>-<abi>-<platform>.whl` (PEP 427).
fn wheel_coordinate(file_name: &str) -> Option<FilenameCoordinate> {
    let parts: Vec<&str> = file_name.split('-').collect();
    (parts.len() >= 2)
        .then(|| FilenameCoordinate::new("pypi", parts[0].replace('_', "-"), parts[1]))
}

fn split_on_last_dash(
    registry_type: &str,
    file_name: &str,
    suffix: &str,
) -> Option<FilenameCoordinate> {
    let stem = file_name.strip_suffix(suffix)?;
    let dash = stem.rfind('-')?;
    Some(FilenameCoordinate::new(
        registry_type,
        &stem[..dash],
        &stem[dash + 1..],
    ))
}

/// Reads the coordinate an asset's **file name** declares, for a target of one
/// registry kind.
///
/// The kind is checked, not assumed: an `npm` registry importing a `.whl`
/// because a glob was too wide would publish a Python wheel as a tarball, and
/// the failure would surface at `npm install`. A file name belonging to another
/// ecosystem is no coordinate at all here.
pub struct FilenameCoordinates {
    /// `RegistryKind::as_str()` of the registry published into.
    pub registry_type: String,
}

impl super::CoordinateReader for FilenameCoordinates {
    fn read(&self, filename: &str, _bytes: &[u8]) -> Option<(String, String)> {
        self.read_from_name(filename)
    }

    fn read_from_name(&self, filename: &str) -> Option<(String, String)> {
        let found = coordinate_from_filename(filename)?;
        (found.registry_type == self.registry_type && found.is_publishable())
            .then_some((found.name, found.version))
    }
}
