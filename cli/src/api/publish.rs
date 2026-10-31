use std::path::Path;

use anyhow::Result;
use base64::Engine as _;
use sha2::{Digest, Sha256};

use super::BatleHubClient;

/// Metadata extracted from (or provided for) an artifact.
#[derive(Debug, Clone)]
pub struct ArtifactMeta {
    pub registry_type: String,
    pub name: String,
    pub version: String,
}

/// Attempt to detect registry type and extract name/version from an artifact file.
pub fn detect_meta(path: &Path) -> Option<ArtifactMeta> {
    if is_devfile_path(path) {
        let bytes = devfile_bundle(path).ok()?;
        let bundle = batlehub_core::services::devfile::read_bundle(&bytes).ok()?;
        return Some(ArtifactMeta {
            registry_type: "devfile".to_owned(),
            name: bundle.name,
            version: bundle.version,
        });
    }
    let file_name = path.file_name()?.to_str()?;
    let found = batlehub_core::services::coordinate_from_filename(file_name)?;
    Some(ArtifactMeta {
        registry_type: found.registry_type,
        name: found.name,
        version: found.version,
    })
}

/// A devfile stack directory, its `devfile.yaml`, or a prebuilt
/// `*.devfile.tar` (RFC 0039 §4.2).
pub fn is_devfile_path(path: &Path) -> bool {
    let name = path.file_name().and_then(std::ffi::OsStr::to_str);
    (path.is_dir() && path.join("devfile.yaml").is_file())
        || name == Some("devfile.yaml")
        || name.is_some_and(|n| n.ends_with(".devfile.tar"))
}

/// The bundle to upload for a devfile path: a prebuilt `*.devfile.tar` as it
/// is, or the stack directory packed by [`pack_stack`].
pub fn devfile_bundle(path: &Path) -> Result<Vec<u8>> {
    if path.is_file() && path.file_name().and_then(std::ffi::OsStr::to_str) != Some("devfile.yaml")
    {
        return Ok(std::fs::read(path)?);
    }
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or_else(|| Path::new("."))
    };
    pack_stack(dir)
}

/// Pack a stack directory, as `registry-support` lays one out, into the
/// publish bundle (RFC 0039 §4.3): `devfile.yaml` as is, each
/// `starter-projects/*.zip` as is, and every other file except dotfiles into
/// `archive.tar`.
///
/// Deterministic, so publishing the same directory twice — or on both sides of
/// an air gap — gives the same digests: paths sorted, `mtime` 0, owner 0 with
/// no names, mode `0644`/`0755` from the executable bit. `archive.tar` is
/// gzip-compressed despite its name, because `registry-library` reads it
/// through `gzip.NewReader`, and carries a directory entry ahead of every file
/// in it, because it opens each file without creating its parent.
pub fn pack_stack(dir: &Path) -> Result<Vec<u8>> {
    let devfile = std::fs::read(dir.join("devfile.yaml"))
        .map_err(|e| anyhow::anyhow!("{}: no readable devfile.yaml: {e}", dir.display()))?;
    let mut entries: Vec<(String, Option<Vec<u8>>, bool)> = Vec::new();
    collect_stack_files(dir, "", &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut starters = Vec::new();
    let mut archive = Vec::new();
    for (rel, bytes, executable) in entries {
        match rel.strip_prefix("starter-projects/") {
            Some(zip) if zip.ends_with(".zip") && !zip.contains('/') => {
                if let Some(bytes) = bytes {
                    starters.push((rel.clone(), bytes));
                }
            }
            Some(_) => {}
            None if rel == "starter-projects/" || rel == "devfile.yaml" => {}
            None => archive.push((rel, bytes, executable)),
        }
    }

    let mut members: Vec<(String, Vec<u8>)> = vec![("devfile.yaml".to_owned(), devfile)];
    if archive.iter().any(|(_, bytes, _)| bytes.is_some()) {
        let mut tar = tar::Builder::new(Vec::new());
        for (rel, bytes, executable) in &archive {
            match bytes {
                None => append_entry(&mut tar, rel, tar::EntryType::Directory, 0o755, &[])?,
                Some(b) => {
                    let mode = if *executable { 0o755 } else { 0o644 };
                    append_entry(&mut tar, rel, tar::EntryType::Regular, mode, b)?
                }
            }
        }
        let mut gz = flate2::GzBuilder::new()
            .mtime(0)
            .write(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &tar.into_inner()?)?;
        members.push(("archive.tar".to_owned(), gz.finish()?));
    }
    members.extend(starters);

    let mut bundle = tar::Builder::new(Vec::new());
    for (path, bytes) in &members {
        append_entry(&mut bundle, path, tar::EntryType::Regular, 0o644, bytes)?;
    }
    Ok(bundle.into_inner()?)
}

/// Every file and directory under `dir`, relative, dotfiles skipped. A
/// directory is `(path + "/", None, _)`; a link is refused rather than
/// followed or packed as a link, which the server would refuse anyway.
fn collect_stack_files(
    dir: &Path,
    prefix: &str,
    out: &mut Vec<(String, Option<Vec<u8>>, bool)>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let rel = format!("{prefix}{name}");
        let meta = entry.path().symlink_metadata()?;
        if meta.is_dir() {
            out.push((format!("{rel}/"), None, false));
            collect_stack_files(&entry.path(), &format!("{rel}/"), out)?;
        } else if meta.is_file() {
            #[cfg(unix)]
            let executable =
                std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0;
            #[cfg(not(unix))]
            let executable = false;
            out.push((rel, Some(std::fs::read(entry.path())?), executable));
        } else {
            anyhow::bail!(
                "{rel}: only regular files and directories can be packed into a devfile stack"
            );
        }
    }
    Ok(())
}

fn append_entry(
    tar: &mut tar::Builder<Vec<u8>>,
    path: &str,
    kind: tar::EntryType,
    mode: u32,
    bytes: &[u8],
) -> Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_entry_type(kind);
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    tar.append_data(&mut header, path, bytes)?;
    Ok(())
}

/// What `POST api/stacks` answers.
#[derive(Debug, serde::Deserialize)]
pub struct DevfilePublished {
    pub stack: String,
    pub version: String,
    pub digest: String,
}

impl BatleHubClient {
    /// Upload a devfile stack bundle to a local or hybrid devfile registry.
    pub async fn publish_devfile(
        &self,
        registry: &str,
        bundle: Vec<u8>,
    ) -> Result<DevfilePublished> {
        let req = self
            .request(
                reqwest::Method::POST,
                &format!("/proxy/{registry}/api/stacks"),
            )
            .header("Content-Type", "application/x-tar")
            .body(bundle);
        let resp = self.send(req).await?;
        super::expect_ok(resp).await
    }

    /// Upload a `.nupkg` artifact to a NuGet local/hybrid registry.
    pub async fn publish_nuget(&self, registry: &str, file_path: &Path) -> Result<()> {
        let bytes = tokio::fs::read(file_path).await?;
        let file_name = file_path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("package.nupkg")
            .to_string();

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name)
            .mime_str("application/octet-stream")?;
        let form = reqwest::multipart::Form::new().part("package", part);

        self.put_multipart_void(&format!("/proxy/{registry}/nuget/api/v2/package"), form)
            .await
    }

    /// Upload a `.pkg.tar.{zst,xz,gz}` artifact to a pacman local/hybrid registry.
    /// The server reads name/version/arch from the archive's `.PKGINFO`, so the
    /// raw bytes are PUT directly to the upload endpoint.
    pub async fn publish_pacman(&self, registry: &str, file_path: &Path) -> Result<()> {
        let bytes = tokio::fs::read(file_path).await?;
        self.put_bytes(&format!("/proxy/{registry}/pacman/upload"), bytes)
            .await
    }

    /// Upload a Python distribution (`.whl`/`.tar.gz`) to a PyPI local/hybrid
    /// registry, using the same `multipart/form-data` fields as `twine upload`.
    /// `name` and `version` are required top-level fields, not derived from
    /// the file's own contents server-side.
    pub async fn publish_pypi(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        file_path: &Path,
    ) -> Result<()> {
        let bytes = tokio::fs::read(file_path).await?;
        let file_name = file_path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("package.whl")
            .to_string();
        let sha2_hex = hex::encode(Sha256::digest(&bytes));

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name)
            .mime_str("application/octet-stream")?;
        let form = reqwest::multipart::Form::new()
            .text(":action", "file_upload")
            .text("name", name.to_string())
            .text("version", version.to_string())
            .text("sha2", sha2_hex)
            .part("content", part);

        self.post_multipart_void(&format!("/proxy/{registry}/legacy/"), form)
            .await
    }

    /// Upload a `.gem` artifact to a RubyGems local/hybrid registry (`gem push`).
    /// The server reads name/version/platform from the gem's own metadata, so
    /// the raw bytes are POSTed directly to the upload endpoint.
    pub async fn publish_rubygems(&self, registry: &str, file_path: &Path) -> Result<()> {
        let bytes = tokio::fs::read(file_path).await?;
        self.post_bytes(&format!("/proxy/{registry}/api/v1/gems"), bytes)
            .await
    }

    /// Upload an npm tarball (`npm publish`'s wire format): a JSON envelope
    /// with the package metadata under `versions` and the base64-encoded
    /// tarball under `_attachments`.
    pub async fn publish_npm(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        file_path: &Path,
    ) -> Result<()> {
        let bytes = tokio::fs::read(file_path).await?;
        let file_name = file_path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("package.tgz")
            .to_string();
        let length = bytes.len();
        let data_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        let body = serde_json::json!({
            "name": name,
            "versions": {
                version: { "name": name, "version": version },
            },
            "_attachments": {
                file_name: {
                    "content_type": "application/octet-stream",
                    "data": data_b64,
                    "length": length,
                },
            },
        });

        self.put(&format!("/proxy/{registry}/{name}"), &body).await
    }

    /// Upload a `.crate` artifact using the `cargo publish` binary wire format:
    /// a `u32` little-endian metadata length, the metadata JSON, a `u32`
    /// little-endian crate length, then the crate bytes.
    pub async fn publish_cargo(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        file_path: &Path,
    ) -> Result<()> {
        let crate_bytes = tokio::fs::read(file_path).await?;
        let metadata = serde_json::json!({ "name": name, "vers": version });
        let metadata_bytes = serde_json::to_vec(&metadata)?;

        let mut body = Vec::with_capacity(8 + metadata_bytes.len() + crate_bytes.len());
        body.extend_from_slice(&(metadata_bytes.len() as u32).to_le_bytes());
        body.extend_from_slice(&metadata_bytes);
        body.extend_from_slice(&(crate_bytes.len() as u32).to_le_bytes());
        body.extend_from_slice(&crate_bytes);

        self.put_bytes(&format!("/proxy/{registry}/api/v1/crates/new"), body)
            .await
    }

    /// Upload a Composer package ZIP. The server parses `composer.json` from
    /// the archive itself for name/version; `version_override` maps to the
    /// endpoint's optional `?version=` query parameter.
    pub async fn publish_composer(
        &self,
        registry: &str,
        file_path: &Path,
        version_override: Option<&str>,
    ) -> Result<()> {
        let bytes = tokio::fs::read(file_path).await?;
        let path = match version_override {
            Some(v) => format!("/proxy/{registry}/api/upload?version={v}"),
            None => format!("/proxy/{registry}/api/upload"),
        };
        self.post_bytes(&path, bytes).await
    }

    /// Upload a conda package (`.tar.bz2` or `.conda`). `platform` is only a
    /// fallback subdir — the server prefers the `subdir` embedded in the
    /// archive's own `info/index.json` when present.
    pub async fn publish_conda(
        &self,
        registry: &str,
        platform: &str,
        file_path: &Path,
    ) -> Result<()> {
        let bytes = tokio::fs::read(file_path).await?;
        self.post_bytes(&format!("/proxy/{registry}/{platform}/"), bytes)
            .await
    }

    /// Upload a VS Code/OpenVSX extension `.vsix` package.
    pub async fn publish_openvsx(
        &self,
        registry: &str,
        extension_id: &str,
        version: &str,
        file_path: &Path,
    ) -> Result<()> {
        let bytes = tokio::fs::read(file_path).await?;
        self.put_bytes(
            &format!("/proxy/{registry}/{extension_id}/{version}/vsix"),
            bytes,
        )
        .await
    }

    /// Upload a `.deb` package into a suite/component pool. The server reads
    /// name/version/architecture from the package's own control file.
    pub async fn publish_deb(
        &self,
        registry: &str,
        distribution: &str,
        component: &str,
        file_path: &Path,
    ) -> Result<()> {
        let bytes = tokio::fs::read(file_path).await?;
        self.put_bytes(
            &format!("/proxy/{registry}/deb/pool/{distribution}/{component}/upload"),
            bytes,
        )
        .await
    }

    /// Upload a `.rpm` package. The server reads name/version/architecture
    /// from the package's own header.
    pub async fn publish_rpm(&self, registry: &str, file_path: &Path) -> Result<()> {
        let bytes = tokio::fs::read(file_path).await?;
        self.put_bytes(&format!("/proxy/{registry}/rpm/upload"), bytes)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn stack_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::write(
            p.join("devfile.yaml"),
            "schemaVersion: 2.2.0\nmetadata:\n  name: acme-go\n  version: 1.2.0\n\
             starterProjects:\n  - name: hello\n",
        )
        .unwrap();
        std::fs::create_dir_all(p.join("kubernetes")).unwrap();
        std::fs::write(p.join("kubernetes/deploy.yaml"), "kind: Deployment\n").unwrap();
        std::fs::create_dir_all(p.join("starter-projects")).unwrap();
        std::fs::write(p.join("starter-projects/hello.zip"), b"PK zip").unwrap();
        std::fs::write(p.join(".git-keep"), b"dotfiles are not packed").unwrap();
        dir
    }

    #[test]
    fn a_stack_directory_packs_deterministically_into_a_bundle_the_server_reads() {
        let dir = stack_dir();
        let first = pack_stack(dir.path()).unwrap();
        assert_eq!(first, pack_stack(dir.path()).unwrap(), "same bytes twice");
        let bundle = batlehub_core::services::devfile::read_bundle(&first).unwrap();
        assert_eq!(
            (bundle.name.as_str(), bundle.version.as_str()),
            ("acme-go", "1.2.0")
        );
        assert_eq!(
            bundle.member(&first, "starter-projects/hello.zip"),
            Some(&b"PK zip"[..])
        );

        // The layer is a gzip tar with the directory ahead of its file.
        let gz = bundle.member(&first, "layer/archive.tar").unwrap();
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(gz));
        let paths: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().display().to_string())
            .collect();
        assert_eq!(paths, ["kubernetes/", "kubernetes/deploy.yaml"]);
    }

    #[test]
    fn a_devfile_alone_packs_with_no_archive_layer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("devfile.yaml"),
            "schemaVersion: 2.2.0\nmetadata:\n  name: bare\n  version: 1.0.0\n",
        )
        .unwrap();
        let bytes = devfile_bundle(&dir.path().join("devfile.yaml")).unwrap();
        let bundle = batlehub_core::services::devfile::read_bundle(&bytes).unwrap();
        assert!(bundle.archive.is_none());
    }

    #[test]
    fn detect_meta_reads_a_devfile_in_its_three_spellings() {
        let dir = stack_dir();
        let tar_path = dir.path().join("acme-go-1.2.0.devfile.tar");
        let packed = pack_stack(dir.path()).unwrap();
        for path in [dir.path().to_path_buf(), dir.path().join("devfile.yaml")] {
            let meta = detect_meta(&path).expect("detected");
            assert_eq!(meta.registry_type, "devfile");
            assert_eq!(
                (meta.name.as_str(), meta.version.as_str()),
                ("acme-go", "1.2.0")
            );
        }
        std::fs::write(&tar_path, packed).unwrap();
        assert_eq!(detect_meta(&tar_path).unwrap().name, "acme-go");
    }

    #[test]
    fn detect_meta_pacman_package() {
        let meta =
            detect_meta(&PathBuf::from("hello-world-1.0-2-x86_64.pkg.tar.zst")).expect("detected");
        assert_eq!(meta.registry_type, "pacman");
        assert_eq!(meta.name, "hello-world");
        assert_eq!(meta.version, "1.0-2");

        // .xz and .gz variants are also recognised.
        assert_eq!(
            detect_meta(&PathBuf::from("foo-2.3-1-any.pkg.tar.xz"))
                .unwrap()
                .registry_type,
            "pacman"
        );
    }

    #[test]
    fn detect_meta_nuget_still_works() {
        let meta = detect_meta(&PathBuf::from("Newtonsoft.Json.13.0.3.nupkg")).expect("detected");
        assert_eq!(meta.registry_type, "nuget");
        assert_eq!(meta.name, "Newtonsoft.Json");
        assert_eq!(meta.version, "13.0.3");
    }

    #[test]
    fn detect_meta_npm_tarball() {
        let meta = detect_meta(&PathBuf::from("left-pad-1.3.0.tgz")).expect("detected");
        assert_eq!(meta.registry_type, "npm");
        assert_eq!(meta.name, "left-pad");
        assert_eq!(meta.version, "1.3.0");
    }

    #[test]
    fn detect_meta_cargo_crate() {
        let meta = detect_meta(&PathBuf::from("serde-1.0.200.crate")).expect("detected");
        assert_eq!(meta.registry_type, "cargo");
        assert_eq!(meta.name, "serde");
        assert_eq!(meta.version, "1.0.200");
    }

    #[test]
    fn detect_meta_openvsx_vsix() {
        let meta = detect_meta(&PathBuf::from("my-org.my-ext-2.1.0.vsix")).expect("detected");
        assert_eq!(meta.registry_type, "openvsx");
        assert_eq!(meta.name, "my-org.my-ext");
        assert_eq!(meta.version, "2.1.0");
    }

    #[test]
    fn detect_meta_deb_is_cosmetic_but_always_detects() {
        let meta = detect_meta(&PathBuf::from("hello_1.0-1_amd64.deb")).expect("detected");
        assert_eq!(meta.registry_type, "deb");
    }

    #[test]
    fn detect_meta_rpm_is_cosmetic_but_always_detects() {
        let meta = detect_meta(&PathBuf::from("hello-1.0-1.x86_64.rpm")).expect("detected");
        assert_eq!(meta.registry_type, "rpm");
    }

    #[test]
    fn detect_meta_conda_tar_bz2() {
        let meta = detect_meta(&PathBuf::from("numpy-1.26.0-py311h0.tar.bz2")).expect("detected");
        assert_eq!(meta.registry_type, "conda");
        assert_eq!(meta.name, "numpy");
        assert_eq!(meta.version, "1.26.0");
    }

    #[test]
    fn detect_meta_conda_new_format() {
        let meta = detect_meta(&PathBuf::from("numpy-1.26.0-py311h0.conda")).expect("detected");
        assert_eq!(meta.registry_type, "conda");
        assert_eq!(meta.name, "numpy");
        assert_eq!(meta.version, "1.26.0");
    }
}
