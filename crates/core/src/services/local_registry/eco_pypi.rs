use bytes::Bytes;

use super::{
    maven_artifact_storage_key, CoreError, Identity, LocalRegistryService, PublishPolicyRequest,
    StorageMeta,
};
use crate::entities::PublishedPackage;
use crate::services::escaping::{escape_html, percent_encode_path_segment};

/// One more distribution file for a PyPI version that is already published —
/// the wheel that follows the sdist of the same `twine upload dist/*`.
pub struct PypiFileUpload<'a> {
    pub registry: &'a str,
    /// The PEP 503 name.
    pub name: &'a str,
    pub version: &'a str,
    pub filename: &'a str,
    pub bytes: Bytes,
    /// SHA-256 hex of `bytes`, computed by the caller.
    pub sha256: &'a str,
    pub requires_python: Option<&'a str>,
}

/// Where a PyPI file asked for by name lives in this local registry.
#[derive(Debug, PartialEq, Eq)]
pub enum PypiLocalFile {
    /// The version's primary file (or no local version at all): served by the
    /// ordinary single-artifact path, which keeps its re-serve checksum and
    /// signature checks.
    Primary,
    /// A file added after the first, at this storage key.
    Extra(String),
}

/// Storage key of a file added after a version's first. The digest is in the
/// key so that the loser of two racing uploads of one filename never
/// overwrites the bytes the winner's recorded `sha256` describes.
fn extra_file_key(
    registry: &str,
    name: &str,
    version: &str,
    sha256: &str,
    filename: &str,
) -> String {
    maven_artifact_storage_key(registry, name, version, &format!("{sha256}/{filename}"))
}

/// The primary file's name, as the Simple page has always rendered it.
fn primary_filename(pkg: &PublishedPackage) -> String {
    pkg.index_metadata
        .get("filename")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}-{}.tar.gz", pkg.name, pkg.version))
}

/// Every file of a version as `(filename, sha256, requires_python)`, primary first.
fn version_files(pkg: &PublishedPackage) -> Vec<(String, String, Option<String>)> {
    let meta = &pkg.index_metadata;
    let primary_sha = meta
        .get("sha256")
        .and_then(|v| v.as_str())
        .unwrap_or(&pkg.checksum)
        .to_owned();
    let requires = |m: &serde_json::Value| {
        m.get("requires_python")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let mut files = vec![(primary_filename(pkg), primary_sha, requires(meta))];
    for f in meta
        .get("files")
        .and_then(|f| f.as_array())
        .into_iter()
        .flatten()
    {
        if let (Some(name), Some(sha)) = (
            f.get("filename").and_then(|v| v.as_str()),
            f.get("sha256").and_then(|v| v.as_str()),
        ) {
            files.push((name.to_owned(), sha.to_owned(), requires(f)));
        }
    }
    files
}

impl LocalRegistryService {
    /// Build a PyPI Simple API HTML page listing all versions of `package_name`
    /// published in this local registry, formatted so `pip` can parse it.
    ///
    /// `base_url` is the registry's public base as seen by the requesting client
    /// (see [`LocalRegistryService::get_npm_packument`]).
    ///
    /// **Every interpolated value is escaped**, and the filename is
    /// percent-encoded before it becomes a path segment. None of the four is
    /// trustworthy: `filename` and `sha256` are read back from the publisher's
    /// `index_metadata`, `package_name` is only normalised (which does not
    /// remove `<`), and `base_url` derives from the request's `Host`. This
    /// document is served as `text/html` from the console's own origin, so an
    /// unescaped value here is stored XSS against an operator's session — and,
    /// because `pip` follows absolute `href`s out of a Simple index, an injected
    /// second anchor is an artifact-source substitution that bypasses the cache,
    /// the block list and the whole rule chain.
    pub async fn get_pypi_simple_page(
        &self,
        registry: &str,
        package_name: &str,
        base_url: &str,
        identity: &Identity,
    ) -> Result<String, CoreError> {
        let versions = self
            .load_visible_versions_or_not_found(registry, package_name, identity, "pypi package")
            .await?;
        let base = base_url.trim_end_matches('/');
        let mut links = String::new();
        for pkg in &versions {
            for (filename, sha256, requires_python) in version_files(pkg) {
                let href = escape_html(&format!(
                    "{}/packages/{}#sha256={}",
                    base,
                    percent_encode_path_segment(&filename),
                    percent_encode_path_segment(&sha256),
                ));
                let requires = requires_python
                    .map(|r| format!(" data-requires-python=\"{}\"", escape_html(&r)))
                    .unwrap_or_default();
                let text = escape_html(&filename);
                links.push_str(&format!("    <a href=\"{href}\"{requires}>{text}</a>\n"));
            }
        }
        let package_name = escape_html(package_name);
        Ok(format!(
            "<!DOCTYPE html>\n<html>\n  <head><title>Links for {package_name}</title></head>\n  <body>\n    <h1>Links for {package_name}</h1>\n{links}  </body>\n</html>\n"
        ))
    }

    /// Add a file to a published PyPI version.
    ///
    /// A PyPI version is several files uploaded one request at a time, so the
    /// second and later ones cannot go through [`Self::publish`], whose row is
    /// one per version. Each is judged by the same publish policy (grants,
    /// ownership, immutability on *this file's* key, size, quota), stored under
    /// its own key, then recorded with `append_file`. A filename the version
    /// already has is a `409`: PyPI files are immutable.
    pub async fn add_pypi_file(
        &self,
        req: PypiFileUpload<'_>,
        publisher: &Identity,
    ) -> Result<(), CoreError> {
        let key = extra_file_key(
            req.registry,
            req.name,
            req.version,
            req.sha256,
            req.filename,
        );
        let len = req.bytes.len() as u64;
        self.enforce_publish_policy(
            &PublishPolicyRequest {
                registry: req.registry,
                name: req.name,
                version: req.version,
                artifact_len: len,
                signature_bytes: None,
                signature_type: None,
                artifact_key: Some(&key),
            },
            publisher,
        )
        .await?;

        let stored = self
            .storage
            .store(
                &key,
                req.bytes,
                StorageMeta {
                    content_type: Some("application/octet-stream".to_owned()),
                    size: None,
                    checksum: Some(req.sha256.to_owned()),
                },
            )
            .await;
        let mut entry = serde_json::json!({ "filename": req.filename, "sha256": req.sha256 });
        if let Some(r) = req.requires_python {
            entry["requires_python"] = serde_json::Value::String(r.to_owned());
        }
        let appended = match stored {
            Ok(_) => {
                self.backend
                    .append_file(req.registry, req.name, req.version, entry)
                    .await
            }
            Err(e) => Err(e),
        };
        match appended {
            Ok(true) => {
                self.invalidate_documents(req.registry).await;
                Ok(())
            }
            // The bytes stay: they sit under the version, so a delete of the
            // version drops them, and they may be a racing twin's (same digest,
            // same key) that the row does describe.
            Ok(false) => {
                self.revoke_publish_quota(publisher, req.registry, len)
                    .await;
                Err(CoreError::Conflict(format!(
                    "{} already exists in {}@{}, or that version is not published",
                    req.filename, req.name, req.version
                )))
            }
            Err(e) => {
                self.revoke_publish_quota(publisher, req.registry, len)
                    .await;
                Err(e)
            }
        }
    }

    /// Which stored file a PyPI download by `filename` names.
    ///
    /// `NotFound` when the version exists locally but has no file of that name:
    /// the single-artifact path would otherwise answer a wheel request with the
    /// sdist's bytes.
    pub async fn pypi_local_file(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        filename: &str,
    ) -> Result<PypiLocalFile, CoreError> {
        let versions = self.backend.get_versions(registry, name).await?;
        let Some(pkg) = versions.iter().find(|p| p.version == version) else {
            return Ok(PypiLocalFile::Primary);
        };
        if primary_filename(pkg) == filename {
            return Ok(PypiLocalFile::Primary);
        }
        version_files(pkg)
            .into_iter()
            .skip(1)
            .find(|(f, _, _)| f == filename)
            .map(|(f, sha, _)| {
                PypiLocalFile::Extra(extra_file_key(registry, name, version, &sha, &f))
            })
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "{filename} is not a file of {name}@{version} in registry '{registry}'"
                ))
            })
    }

    /// The names of the files a published version already has, primary first;
    /// `None` when the version is not published here.
    pub async fn pypi_version_filenames(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<Vec<String>>, CoreError> {
        let versions = self.backend.get_versions(registry, name).await?;
        Ok(versions
            .iter()
            .find(|p| p.version == version)
            .map(|p| version_files(p).into_iter().map(|(f, _, _)| f).collect()))
    }
}
