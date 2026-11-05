use super::{
    terraform_provider_shasums_storage_key, CoreError, Identity, LocalRegistryService, StreamExt,
    TerraformPlatform, TerraformShasumsFile,
};
use crate::services::openpgp::{verify_detached, PgpVerdict};

impl LocalRegistryService {
    /// Build the Terraform module versions envelope from `local_packages`.
    pub async fn get_terraform_module_versions_response(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
    ) -> Result<serde_json::Value, CoreError> {
        let versions = self
            .load_visible_versions_or_not_found(registry, name, identity, "module")
            .await?;
        let version_list: Vec<serde_json::Value> = versions
            .iter()
            .filter(|v| !v.yanked)
            .map(|v| serde_json::json!({"version": v.version}))
            .collect();
        Ok(serde_json::json!({"modules": [{"versions": version_list}]}))
    }

    /// Build the Terraform provider versions envelope from `local_packages`.
    pub async fn get_terraform_provider_versions_response(
        &self,
        registry: &str,
        name: &str,
        identity: &Identity,
    ) -> Result<serde_json::Value, CoreError> {
        let versions = self
            .load_visible_versions_or_not_found(registry, name, identity, "provider")
            .await?;
        let version_list: Vec<serde_json::Value> = versions
            .iter()
            .filter(|v| !v.yanked)
            .map(|v| {
                let meta = &v.index_metadata;
                let protocols = meta
                    .get("protocols")
                    .and_then(|p| p.as_array())
                    .cloned()
                    .unwrap_or_default();
                let platforms = meta
                    .get("platforms")
                    .and_then(|p| p.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|p| {
                                serde_json::json!({
                                    "os": p.get("os").and_then(|v| v.as_str()).unwrap_or(""),
                                    "arch": p.get("arch").and_then(|v| v.as_str()).unwrap_or(""),
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                serde_json::json!({
                    "version": v.version,
                    "protocols": protocols,
                    "platforms": platforms,
                })
            })
            .collect();
        Ok(serde_json::json!({"versions": version_list}))
    }

    /// Build the Terraform provider download-info response for a specific version+platform.
    ///
    /// Rewrites `download_url` to point at `base_url`, the registry's public base
    /// as seen by the requesting client (see
    /// [`LocalRegistryService::get_npm_packument`]).
    pub async fn get_terraform_provider_download_response(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        platform: TerraformPlatform<'_>,
        base_url: &str,
        identity: &Identity,
    ) -> Result<serde_json::Value, CoreError> {
        let TerraformPlatform { os, arch } = platform;
        // Terraform is local-only — there is no proxy fall-through behind this
        // document to run the registry's rule chain later, so this read is the
        // only place it can run at all (survey finding 8).
        self.check_read_access(registry, name, identity).await?;
        self.check_prerelease_access(registry, version, identity)
            .await?;
        let versions = self.backend.get_versions(registry, name).await?;
        let pkg = versions
            .into_iter()
            .find(|v| v.version == version)
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "provider {name}@{version} not found in registry '{registry}'"
                ))
            })?;

        let platforms = pkg
            .index_metadata
            .get("platforms")
            .and_then(|p| p.as_array())
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "provider {name}@{version} has no platforms metadata"
                ))
            })?;

        let platform = platforms
            .iter()
            .find(|p| {
                p.get("os").and_then(|v| v.as_str()) == Some(os)
                    && p.get("arch").and_then(|v| v.as_str()) == Some(arch)
            })
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "provider {name}@{version} has no platform {os}/{arch}"
                ))
            })?;

        // Extract namespace/type from name like "providers/{ns}/{type}"
        let parts: Vec<&str> = name.splitn(3, '/').collect();
        let (ns, ptype) = if parts.len() == 3 {
            (parts[1], parts[2])
        } else {
            ("", "")
        };

        // Read before the response is built: the namespace is known by now, and
        // an `entry(...)` closure cannot await.
        let signing_keys: Vec<crate::entities::SigningKey> = {
            let port = { self.hot.read().await.signing_keys.clone() };
            match port {
                Some(port) => port.list_signing_keys(registry, ns).await?,
                None => Vec::new(),
            }
        };

        let base = base_url.trim_end_matches('/');
        let download_url =
            format!("{base}/v1/providers/{ns}/{ptype}/{version}/artifact/{os}/{arch}");

        let mut resp = platform.clone();
        if let Some(obj) = resp.as_object_mut() {
            obj.insert("os".to_owned(), serde_json::json!(os));
            obj.insert("arch".to_owned(), serde_json::json!(arch));
            obj.insert("download_url".to_owned(), serde_json::json!(download_url));
            let meta = &pkg.index_metadata;
            obj.entry("protocols").or_insert_with(|| {
                meta.get("protocols")
                    .cloned()
                    .unwrap_or(serde_json::json!([]))
            });
            // RFC 0015 §4.2 — the namespace's registered keys, where there are
            // any.
            //
            // This was `{"gpg_public_keys": []}`, hardcoded. Terraform verifies a
            // provider's `SHASUMS` signature against whatever is here, and an
            // empty list is not "unsigned, proceed" — it is the registry saying
            // there is nothing to verify against, so **no locally published
            // provider could be verified by anybody**.
            //
            // `entry(...).or_insert_with` still, so a provider whose own
            // `index_metadata` carries `signing_keys` (a mirrored upstream one)
            // keeps what it was published with. The store answers for providers
            // published here, which is the set that had no answer at all.
            obj.entry("signing_keys")
                .or_insert_with(|| serde_json::json!({ "gpg_public_keys": signing_keys }));
            // The publisher's `SHA256SUMS` and its signature, uploaded beside the
            // archives (issue #281). Without them Terraform has no checksum list
            // to verify the archive against and refuses it. A manifest that
            // names its own (off-origin) URLs keeps them.
            let root = format!("{base}/v1/providers/{ns}/{ptype}/{version}");
            obj.entry("shasums_url")
                .or_insert_with(|| serde_json::json!(format!("{root}/shasums")));
            obj.entry("shasums_signature_url")
                .or_insert_with(|| serde_json::json!(format!("{root}/shasums.sig")));
        }
        Ok(resp)
    }

    /// Check a provider version's `SHA256SUMS` against the manifest it was
    /// published with (issue #281), before it is stored.
    ///
    /// Every platform the manifest names must be listed under its `filename`,
    /// with the manifest's `shasum` when it has one: Terraform refuses an
    /// archive whose name the list lacks, so a list that disagrees is refused
    /// here, at upload, instead of at every `terraform init`. The signature over
    /// the list is not checked here — Terraform checks it against the
    /// namespace's signing keys, which is where the trust is.
    pub async fn check_terraform_provider_shasums(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        sums: &[u8],
    ) -> Result<(), CoreError> {
        let listed = parse_sha256sums(sums)?;
        let versions = self.backend.get_versions(registry, name).await?;
        let pkg = versions
            .into_iter()
            .find(|v| v.version == version)
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "provider {name}@{version} not found in registry '{registry}': \
                     publish its manifest first"
                ))
            })?;
        let platforms = pkg
            .index_metadata
            .get("platforms")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default();
        for p in &platforms {
            let Some(filename) = p.get("filename").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(hash) = listed.get(filename) else {
                return Err(CoreError::InvalidInput(format!(
                    "SHA256SUMS does not list {filename}, which the manifest publishes"
                )));
            };
            if let Some(expected) = p.get("shasum").and_then(|v| v.as_str()) {
                if !expected.eq_ignore_ascii_case(hash) {
                    return Err(CoreError::InvalidInput(format!(
                        "SHA256SUMS gives {filename} the hash {hash}, the manifest {expected}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Check a provider's `SHA256SUMS` signature against the namespace's keys
    /// before it is stored, so the publisher sees a bad one rather than the
    /// first `terraform init` (issue #281). The list must already be up: the
    /// signature is over its stored bytes.
    ///
    /// A namespace with no key, or a key or signature this instance cannot
    /// read, is [`PgpVerdict::Unsupported`] and still stored — Terraform
    /// verifies regardless. A signature that fails under the key it names is
    /// `InvalidInput`.
    pub async fn check_terraform_provider_signature(
        &self,
        registry: &str,
        namespace: &str,
        ptype: &str,
        version: &str,
        signature: &[u8],
    ) -> Result<PgpVerdict, CoreError> {
        let key = terraform_provider_shasums_storage_key(
            registry,
            namespace,
            ptype,
            version,
            TerraformShasumsFile::Sums,
        );
        let Some(stored) = self.storage.retrieve(&key).await? else {
            return Err(CoreError::Conflict(format!(
                "upload the SHA256SUMS of {namespace}/{ptype} {version} before its signature"
            )));
        };
        let mut sums = Vec::new();
        let mut stream = stored.stream;
        while let Some(chunk) = stream.next().await {
            sums.extend_from_slice(&chunk?);
        }
        let port = { self.hot.read().await.signing_keys.clone() };
        let keys = match port {
            Some(port) => port.list_signing_keys(registry, namespace).await?,
            None => Vec::new(),
        };
        if keys.is_empty() {
            return Ok(PgpVerdict::Unsupported(format!(
                "namespace '{namespace}' registers no signing key"
            )));
        }
        let armored: Vec<&str> = keys.iter().map(|k| k.ascii_armor.as_str()).collect();
        verify_detached(&armored, signature, &sums)
    }
}

/// `sha256sum`'s output, as goreleaser writes a provider's `SHA256SUMS`:
/// `<64 hex>  <filename>` per line, the filename optionally `*`-prefixed.
fn parse_sha256sums(sums: &[u8]) -> Result<std::collections::HashMap<String, String>, CoreError> {
    let text = std::str::from_utf8(sums)
        .map_err(|_| CoreError::InvalidInput("SHA256SUMS is not UTF-8 text".to_owned()))?;
    let mut out = std::collections::HashMap::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let malformed = || {
            CoreError::InvalidInput(format!(
                "SHA256SUMS line {}: expected `<sha256 hex>  <filename>`",
                i + 1
            ))
        };
        let (hash, filename) = line.split_once(char::is_whitespace).ok_or_else(malformed)?;
        let filename = filename.trim_start().trim_start_matches('*');
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) || filename.is_empty() {
            return Err(malformed());
        }
        out.insert(filename.to_owned(), hash.to_ascii_lowercase());
    }
    if out.is_empty() {
        return Err(CoreError::InvalidInput(
            "SHA256SUMS lists no file".to_owned(),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod shasums_tests {
    use super::parse_sha256sums;

    #[test]
    fn reads_goreleaser_lines_and_refuses_anything_else() {
        let h = "a".repeat(64);
        let sums = format!("{h}  terraform-provider-x_1.0.0_linux_amd64.zip\n{h} *b.zip\n\n");
        let m = parse_sha256sums(sums.as_bytes()).unwrap();
        assert_eq!(m["terraform-provider-x_1.0.0_linux_amd64.zip"], h);
        assert_eq!(m["b.zip"], h);
        assert!(parse_sha256sums(b"").is_err());
        assert!(parse_sha256sums(b"abc  x.zip").is_err());
        assert!(parse_sha256sums(h.as_bytes()).is_err());
        assert!(parse_sha256sums(&[0xff, 0xfe]).is_err());
    }
}
