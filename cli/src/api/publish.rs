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
    let file_name = path.file_name()?.to_str()?;
    let found = batlehub_core::services::coordinate_from_filename(file_name)?;
    Some(ArtifactMeta {
        registry_type: found.registry_type,
        name: found.name,
        version: found.version,
    })
}

impl BatleHubClient {
    /// Upload a `.nupkg` artifact to a NuGet local/hybrid registry.
    pub async fn publish_nuget(&self, registry: &str, file_path: &Path) -> Result<()> {
        let bytes = std::fs::read(file_path)?;
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
        let bytes = std::fs::read(file_path)?;
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
        let bytes = std::fs::read(file_path)?;
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
        let bytes = std::fs::read(file_path)?;
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
        let bytes = std::fs::read(file_path)?;
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
        let crate_bytes = std::fs::read(file_path)?;
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
        let bytes = std::fs::read(file_path)?;
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
        let bytes = std::fs::read(file_path)?;
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
        let bytes = std::fs::read(file_path)?;
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
        let bytes = std::fs::read(file_path)?;
        self.put_bytes(
            &format!("/proxy/{registry}/deb/pool/{distribution}/{component}/upload"),
            bytes,
        )
        .await
    }

    /// Upload a `.rpm` package. The server reads name/version/architecture
    /// from the package's own header.
    pub async fn publish_rpm(&self, registry: &str, file_path: &Path) -> Result<()> {
        let bytes = std::fs::read(file_path)?;
        self.put_bytes(&format!("/proxy/{registry}/rpm/upload"), bytes)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
