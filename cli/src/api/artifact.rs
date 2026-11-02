//! Where a version's one artifact lives, by coordinate (RFC 0040 §4.7
//! *Download*), and the checksum its own metadata advertises.
//!
//! Only the kinds `RegistryKind::fetchable_by_version` says are fetchable by
//! version have a path here; the rest need a file name the coordinate does not
//! carry, and say why.

use batlehub_core::entities::{FetchSupport, RegistryKind};

use super::auth::percent_encode;

/// The native download path of `name@version`, and the file name the CLI
/// writes it to. The name is derived from the coordinate, never taken from
/// the server (T12).
pub fn artifact_path(
    kind: RegistryKind,
    registry: &str,
    name: &str,
    version: &str,
) -> Result<(String, String), String> {
    if let FetchSupport::None(why) = kind.fetchable_by_version() {
        return Err(format!("{kind}: {why}"));
    }
    let base = name.rsplit('/').next().unwrap_or(name);
    let r = registry;
    Ok(match kind {
        RegistryKind::Npm => (
            format!("/proxy/{r}/{}/{version}/tarball", percent_encode(name)),
            format!("{base}-{version}.tgz"),
        ),
        RegistryKind::Cargo => (
            format!("/proxy/{r}/{name}/{version}/download"),
            format!("{name}-{version}.crate"),
        ),
        RegistryKind::Composer => (
            format!("/proxy/{r}/dist/{name}/{version}"),
            format!("{}-{version}.zip", name.replace('/', "-")),
        ),
        RegistryKind::Rubygems => (
            format!("/proxy/{r}/gems/{name}-{version}.gem"),
            format!("{name}-{version}.gem"),
        ),
        RegistryKind::Goproxy => (
            format!(
                "/proxy/{r}/{}/@v/{version}.zip",
                kind.canonical_package_name(name)
            ),
            format!("{base}-{version}.zip"),
        ),
        RegistryKind::Nuget => {
            let (id, v) = (name.to_lowercase(), version.to_lowercase());
            (
                format!("/proxy/{r}/nuget/v3/flat/{id}/{v}/{id}.{v}.nupkg"),
                format!("{id}.{v}.nupkg"),
            )
        }
        RegistryKind::Openvsx | RegistryKind::VscodeMarketplace => (
            format!("/proxy/{r}/{name}/{version}/vsix"),
            format!("{name}-{version}.vsix"),
        ),
        RegistryKind::JetbrainsMarketplace => (
            format!(
                "/proxy/{r}/plugin/download?pluginId={}&version={}",
                percent_encode(name),
                percent_encode(version)
            ),
            format!("{name}-{version}.zip"),
        ),
        RegistryKind::Galaxy => {
            let file = format!("{}-{version}.tar.gz", name.replace('.', "-"));
            (
                format!("/proxy/{r}/galaxy/api/v3/artifacts/collections/{file}"),
                file,
            )
        }
        other => {
            return Err(format!(
                "{other}: no download by coordinate yet; pass the artifact's /proxy/… path"
            ))
        }
    })
}

/// Where the advertised checksum of `name@version` is read, and how to pick
/// it out of that document. `None`: this kind's metadata carries none the CLI
/// reads, so the download is written unverified, with a warning (T7).
pub fn checksum_source(kind: RegistryKind, registry: &str, name: &str) -> Option<String> {
    match kind {
        RegistryKind::Npm => Some(format!("/proxy/{registry}/{}", percent_encode(name))),
        RegistryKind::Cargo => Some(format!(
            "/proxy/{registry}/registry/{}",
            cargo_index_path(&name.to_lowercase())
        )),
        _ => None,
    }
}

/// The advertised checksum in the document [`checksum_source`] names.
pub fn checksum_in(kind: RegistryKind, doc: &str, version: &str) -> Option<String> {
    match kind {
        RegistryKind::Npm => {
            let v: serde_json::Value = serde_json::from_str(doc).ok()?;
            let dist = &v["versions"][version]["dist"];
            dist["integrity"]
                .as_str()
                .or_else(|| dist["shasum"].as_str())
                .map(str::to_owned)
        }
        // One JSON line per version.
        RegistryKind::Cargo => doc.lines().find_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            (v["vers"] == version).then(|| v["cksum"].as_str().map(str::to_owned))?
        }),
        _ => None,
    }
}

/// Cargo's sparse-index layout: `1/a`, `2/ab`, `3/a/abc`, `ab/cd/abcd…`.
fn cargo_index_path(name: &str) -> String {
    match name.len() {
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[..1]),
        _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_each_kinds_route() {
        let p = |k, n, v| artifact_path(k, "r", n, v).unwrap();
        assert_eq!(
            p(RegistryKind::Npm, "@scope/pkg", "1.0.0"),
            (
                "/proxy/r/%40scope%2Fpkg/1.0.0/tarball".into(),
                "pkg-1.0.0.tgz".into()
            )
        );
        assert_eq!(
            p(RegistryKind::Goproxy, "github.com/Foo/bar", "v1.2.0").0,
            "/proxy/r/github.com/!foo/bar/@v/v1.2.0.zip"
        );
        assert_eq!(
            p(RegistryKind::Nuget, "Serilog", "3.1.1-RC"),
            (
                "/proxy/r/nuget/v3/flat/serilog/3.1.1-rc/serilog.3.1.1-rc.nupkg".into(),
                "serilog.3.1.1-rc.nupkg".into()
            )
        );
        assert_eq!(
            p(RegistryKind::Galaxy, "acme.tools", "1.0.0").1,
            "acme-tools-1.0.0.tar.gz"
        );
        assert!(artifact_path(RegistryKind::Maven, "r", "g:a", "1").is_err());
        assert!(artifact_path(RegistryKind::Pypi, "r", "x", "1").is_err());
    }

    #[test]
    fn checksums_are_read_from_each_kinds_metadata() {
        let packument =
            r#"{"versions":{"1.0.0":{"dist":{"integrity":"sha512-abc","shasum":"ff"}}}}"#;
        assert_eq!(
            checksum_in(RegistryKind::Npm, packument, "1.0.0").as_deref(),
            Some("sha512-abc")
        );
        assert_eq!(checksum_in(RegistryKind::Npm, packument, "2.0.0"), None);
        let index =
            "{\"vers\":\"0.1.0\",\"cksum\":\"aa\"}\n{\"vers\":\"0.2.0\",\"cksum\":\"bb\"}\n";
        assert_eq!(
            checksum_in(RegistryKind::Cargo, index, "0.2.0").as_deref(),
            Some("bb")
        );
        assert_eq!(cargo_index_path("serde"), "se/rd/serde");
        assert_eq!(cargo_index_path("abc"), "3/a/abc");
    }
}
