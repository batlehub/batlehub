//! One import run: choose the releases, match the assets, publish what is new.

use bytes::Bytes;
use sha2::{Digest, Sha256};

use super::{ImportReport, ReleaseImportService, ReleaseSelector};
use crate::entities::PackageId;
use crate::error::CoreError;
use crate::ports::{collect_byte_stream, ForgeRelease};
use crate::services::local_registry::PublishRequest;

/// Whether `name` matches a shell-style glob over a file name.
///
/// `*` matches any run of characters, including none, and nothing else is
/// special: an asset name is one path segment, so there is no `/` for a
/// wildcard to be wrong about. Case-sensitive, because a release page's asset
/// names are.
pub(super) fn glob_matches(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    // No wildcard at all: the pattern is the name.
    let (Some(prefix), Some(suffix)) = (parts.first(), parts.last()) else {
        return false;
    };
    if parts.len() == 1 {
        return pattern == name;
    }
    let Some(mut rest) = name.strip_prefix(prefix) else {
        return false;
    };
    // Each middle segment is found in what the previous one left, so the
    // segments match in order and never overlap.
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    // A pattern ending in `*` takes whatever is left; otherwise the trailing
    // literal has to end the name, inside what is still unconsumed.
    suffix.is_empty() || (rest.len() >= suffix.len() && rest.ends_with(suffix))
}

impl ReleaseImportService {
    /// Import what this configuration selects.
    ///
    /// Never `Err`: one unreadable asset does not fail a release, and one
    /// failing release does not fail a run, for the same reason `WarmingReport`
    /// counts rather than propagates — an operator wants the eight that worked
    /// *and* the names of the three that did not.
    pub async fn import(&self) -> ImportReport {
        self.import_selected(&self.select).await
    }

    /// Import one release by tag, whatever the configured selector says.
    ///
    /// The operator's override: a pre-release is reachable this way and by no
    /// other, since `latest` will not choose one (RFC 0021 §4.2).
    pub async fn import_tag(&self, tag: &str) -> ImportReport {
        self.import_selected(&ReleaseSelector::Tag(tag.to_owned()))
            .await
    }

    async fn import_selected(&self, select: &ReleaseSelector) -> ImportReport {
        let releases = match self.select_releases(select).await {
            Ok(r) => r,
            Err(e) => return ImportReport::failed(selector_label(select), "-", e),
        };
        let mut report = ImportReport::default();
        for release in releases {
            report += self.import_release(&release).await;
        }
        report
    }

    /// The releases this selector names, drafts already gone.
    ///
    /// A draft is dropped here rather than at the call sites so that no future
    /// selector can forget: it is not published on the forge, and importing one
    /// would serve bytes the producing team has not released.
    async fn select_releases(
        &self,
        select: &ReleaseSelector,
    ) -> Result<Vec<ForgeRelease>, CoreError> {
        // Refused at load, so this is the belt to that config check's braces:
        // a client with no releases is a registry kind that serves none.
        let source = self.client.releases().ok_or_else(|| {
            CoreError::NotSupported(format!(
                "registry '{}' is a '{}' registry and serves no releases",
                self.from,
                self.client.registry_type()
            ))
        })?;
        match select {
            ReleaseSelector::Latest => {
                let all = source.list_releases(&self.repo).await?;
                Ok(all
                    .into_iter()
                    .find(ForgeRelease::is_stable)
                    .into_iter()
                    .collect())
            }
            ReleaseSelector::All => Ok(source
                .list_releases(&self.repo)
                .await?
                .into_iter()
                .filter(|r| !r.draft)
                .collect()),
            ReleaseSelector::Tag(tag) => {
                let release = source.release_by_tag(&self.repo, tag).await?;
                if release.draft {
                    return Err(CoreError::InvalidInput(format!(
                        "release '{tag}' of {} is a draft: it is not published, and importing \
                         it would serve bytes nobody has released",
                        self.repo
                    )));
                }
                Ok(vec![release])
            }
        }
    }

    async fn import_release(&self, release: &ForgeRelease) -> ImportReport {
        let mut report = ImportReport::default();
        for asset in &release.assets {
            if !self.assets.iter().any(|g| glob_matches(g, &asset.name)) {
                continue;
            }
            report += self.import_asset(release, asset).await;
        }
        report
    }

    async fn import_asset(
        &self,
        release: &ForgeRelease,
        asset: &crate::ports::ForgeAsset,
    ) -> ImportReport {
        // The cheap half of the held check, when the ecosystem's coordinate is
        // in the file name: an import that already holds every asset of a
        // release then costs one API call and downloads nothing, which is what
        // makes an interval affordable. Kinds that name themselves inside the
        // archive fall through and are checked after the fetch.
        if let Some((name, version)) = self.coordinates.read_from_name(&asset.name) {
            if let Some(done) = self.settled_by_held(release, asset, &name, &version).await {
                return done;
            }
        }

        let bytes = match self.fetch(release, asset).await {
            Ok(b) => b,
            Err(e) => return ImportReport::failed(&release.tag, &asset.name, e),
        };
        let Some((name, version)) = self.coordinates.read(&asset.name, &bytes) else {
            return ImportReport::failed(
                &release.tag,
                &asset.name,
                "the artifact does not name itself and its file name does not either, \
                 so there is no coordinate to publish it under",
            );
        };
        if let Some(done) = self.settled_by_held(release, asset, &name, &version).await {
            return done;
        }

        self.publish_asset(release, asset, &name, &version, bytes)
            .await
    }

    /// Create the version, then run whatever the publish path runs after one.
    ///
    /// Split from the choosing above because they answer different questions:
    /// everything before this decides *whether* an asset is imported, and this
    /// is the one place that says what importing it does.
    async fn publish_asset(
        &self,
        release: &ForgeRelease,
        asset: &crate::ports::ForgeAsset,
        name: &str,
        version: &str,
        bytes: Bytes,
    ) -> ImportReport {
        let index_metadata = self.coordinates.index_metadata(name, version, &bytes);
        let checksum = hex::encode(Sha256::digest(&bytes));
        let published = self
            .local
            .publish(PublishRequest {
                registry: self.into.clone(),
                name: name.to_owned(),
                version: version.to_owned(),
                artifact: bytes.clone(),
                checksum,
                index_metadata,
                unlisted: false,
                publisher: self.principal.identity(),
                signature_bytes: None,
                signature_type: None,
            })
            .await;
        match published {
            Ok(_) => {
                if let Some(hook) = &self.after_publish {
                    hook.after_publish(&self.into, name, version, &bytes).await;
                }
                tracing::info!(
                    into = %self.into, from = %self.from, repo = %self.repo,
                    tag = %release.tag, asset = %asset.name,
                    package = %name, version = %version,
                    publisher = %self.principal.user_id(),
                    "release import: published"
                );
                ImportReport {
                    imported: 1,
                    ..Default::default()
                }
            }
            Err(e) => ImportReport::failed(&release.tag, &asset.name, e),
        }
    }

    /// The bytes, through the **source registry's own client** — so the request
    /// carries that registry's credential and goes out through its allowlist and
    /// SSRF guard. The import adds no egress surface of its own (RFC 0021 §7),
    /// which is the whole reason `from` names a configured registry and never a
    /// URL.
    async fn fetch(
        &self,
        release: &ForgeRelease,
        asset: &crate::ports::ForgeAsset,
    ) -> Result<Bytes, CoreError> {
        let pkg = PackageId::new(&self.from, &self.repo, &release.tag)
            .with_artifact(asset.artifact.clone());
        let fetched = self.client.fetch_artifact(&pkg).await?;
        collect_byte_stream(fetched.stream).await
    }

    /// The report for an asset the held check finishes with, or `None` to carry
    /// on.
    ///
    /// Called at both points a coordinate can be known — from the file name
    /// before the download, and from the bytes after it — so the two answer the
    /// same way. They were the same ten lines twice, which is one place for
    /// them to drift.
    async fn settled_by_held(
        &self,
        release: &ForgeRelease,
        asset: &crate::ports::ForgeAsset,
        name: &str,
        version: &str,
    ) -> Option<ImportReport> {
        match self.is_held(name, version).await {
            Ok(true) => Some(ImportReport {
                skipped: 1,
                ..Default::default()
            }),
            Ok(false) => None,
            Err(e) => Some(ImportReport::failed(&release.tag, &asset.name, e)),
        }
    }

    /// Whether the target already holds this version.
    async fn is_held(&self, name: &str, version: &str) -> Result<bool, CoreError> {
        Ok(self
            .local
            .backend
            .get_versions(&self.into, name)
            .await?
            .iter()
            .any(|p| p.version == version))
    }
}

fn selector_label(select: &ReleaseSelector) -> String {
    match select {
        ReleaseSelector::Latest => "latest".to_owned(),
        ReleaseSelector::All => "all".to_owned(),
        ReleaseSelector::Tag(t) => t.clone(),
    }
}
