//! Listings synthesised from the held set (RFC 0008-bis).
//!
//! A bundle carries artifacts and the metadata entry that finds each one; it
//! does not carry the listing a client resolves through — the packument, the
//! simple page, the release list. On a disconnected instance that listing is
//! a `503`, and phase 0 measured what every client does with one: retries
//! it and never asks for the artifact it could have had (§2). This module is
//! the answer the RFC chose over carrying documents: **a listing is a
//! projection of what the instance holds**, built from the artifact-meta
//! rows joined to their `meta:` entries and rendered in the registry's own
//! shape. It is true by construction — every version it names is served by
//! the next request — which a snapshot taken on the connected side is not
//! (§5.2).
//!
//! What lives here and nowhere else: the join (`held_versions`), the pointer
//! rule (`highest`) and one renderer per row of §4.3. The filters a served
//! listing goes through — the block list, the verdict gate, the URL rewrite —
//! are *not* here: a synthesised document is handed to them exactly as a
//! fetched one is, so nothing that reads a listing can tell the two apart
//! except by the header.

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};

use crate::entities::RegistryKind;
use crate::ports::{ArtifactCacheMeta, CacheStore, DocumentKind, VersionDocument};
use crate::services::version_order::{is_prerelease, newest_first};

/// One artifact this instance holds for a package, with what its `meta:`
/// entry says about it. A version with several files (a wheel and an
/// sdist, a release's assets) is several of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldVersion {
    pub version: String,
    /// The artifact sub-coordinate when the key has one — a PyPI filename,
    /// a forge `filename/<asset>` — and `None` for a one-file version.
    pub artifact: Option<String>,
    /// SHA-256, hex, from the `meta:` entry (the import writes the bundle's
    /// digest there) — or absent, in which case nothing is invented.
    pub checksum: Option<String>,
    pub size: Option<u64>,
    /// The upstream's publication date when the entry has one. An import
    /// leaves it `None` on purpose (RFC 0008 §14.2), so a synthesised
    /// listing carries no `time` for it.
    pub published_at: Option<DateTime<Utc>>,
    /// When this instance received the bytes — the artifact row's own date.
    /// The one date an import leaves behind, and the only honest value for
    /// a field a client *requires* (a forge release's `created_at`, §13.2).
    pub received_at: DateTime<Utc>,
    /// The entry's registry-specific `extra`, for a renderer that can use it.
    pub extra: Value,
}

impl HeldVersion {
    /// The bare file name of a forge asset, when this row is one.
    pub fn asset_name(&self) -> Option<&str> {
        self.artifact.as_deref()?.strip_prefix("filename/")
    }

    /// The name of a GitLab release link, when this row is one.
    pub fn link_name(&self) -> Option<&str> {
        self.artifact.as_deref()?.strip_prefix("link/")
    }
}

/// The join of §5.1: every artifact-meta row for the package whose `meta:`
/// entry the store still holds. A row without its entry is not listed,
/// because nothing could serve it — the resolve that precedes every
/// download reads that entry first.
///
/// Fails **open to empty**: a store error is logged and the caller sees no
/// held set, which is the `503` of today rather than a listing of guesses.
pub async fn held_versions(
    meta: &dyn ArtifactCacheMeta,
    cache: &dyn CacheStore,
    registry: &str,
    name: &str,
) -> Vec<HeldVersion> {
    held_versions_of(meta, cache, registry, &[name.to_owned()]).await
}

/// The package names the held rows of a listing coordinate are filed
/// under. One for most kinds; a PyPI page is also asked under the file
/// spelling (underscores), and an SDKMAN listing (`candidate/platform`)
/// under its candidate.
pub fn package_names_for(kind: RegistryKind, name: &str) -> Vec<String> {
    match kind {
        RegistryKind::Pypi => {
            let mut v = vec![name.to_owned()];
            let alt = name.replace('-', "_");
            if alt != name {
                v.push(alt);
            }
            let alt = name.replace('_', "-");
            if alt != name && !v.contains(&alt) {
                v.push(alt);
            }
            v
        }
        RegistryKind::Sdkman => vec![name.split('/').next().unwrap_or(name).to_owned()],
        RegistryKind::Terraform => vec![terraform_listing_name(name).to_owned()],
        _ => vec![name.to_owned()],
    }
}

/// The package a Terraform coordinate's held rows are filed under. A
/// provider's download document is addressed in full —
/// `providers/{ns}/{type}/{version}/download/{os}/{arch}` — and its rows
/// are the provider's (`providers/{ns}/{type}`); every other coordinate
/// is its own name.
fn terraform_listing_name(name: &str) -> &str {
    if !name.starts_with("providers/") || !name.contains("/download/") {
        return name;
    }
    let mut end = 0;
    for (i, seg) in name.split('/').enumerate() {
        if i == 3 {
            break;
        }
        end += seg.len() + usize::from(i > 0);
    }
    &name[..end]
}

/// [`held_versions`] over several package spellings, merged.
pub async fn held_versions_of(
    meta: &dyn ArtifactCacheMeta,
    cache: &dyn CacheStore,
    registry: &str,
    names: &[String],
) -> Vec<HeldVersion> {
    let mut rows = Vec::new();
    for name in names {
        match meta.list_package_artifacts(registry, name).await {
            Ok(r) => rows.extend(r),
            Err(e) => {
                tracing::warn!(
                    registry = %registry,
                    package = %name,
                    error = %e,
                    "listing synthesis: could not read the held set; answering as unheld"
                );
                return Vec::new();
            }
        }
    }
    let mut held = Vec::with_capacity(rows.len());
    for row in rows {
        if row.version.is_empty() {
            continue;
        }
        if held
            .iter()
            .any(|h: &HeldVersion| h.version == row.version && key_artifact(&row) == h.artifact)
        {
            continue;
        }
        let key = row
            .artifact_key
            .strip_prefix("artifact:")
            .unwrap_or(&row.artifact_key);
        // The key was built from the row's own package name, which is not
        // always the listing's (a PyPI file names `typing_extensions`, its
        // page `typing-extensions`; SDKMAN lists `java/linuxx64`, stores
        // `java`). `package_names_for` maps one to the other before the
        // query; the key is read back with the row's.
        let prefix = format!("{registry}/{}/{}", row.package_name, row.version);
        let artifact = match key.strip_prefix(&prefix) {
            Some("") => None,
            Some(rest) => rest.strip_prefix('/').map(str::to_owned),
            // A key not shaped by this coordinate: whatever it is, the
            // listing cannot address it.
            None => continue,
        };
        let _ = &artifact;
        let entry = match cache.get(&format!("meta:{key}")).await {
            Ok(Some(entry)) => entry,
            _ => continue,
        };
        held.push(HeldVersion {
            version: row.version,
            artifact,
            checksum: entry.metadata.checksum,
            size: row.size_bytes,
            published_at: entry.metadata.published_at,
            received_at: row.cached_at,
            extra: entry.metadata.extra,
        });
    }
    held
}

/// The join over a whole registry: every held row of every package, each
/// paired with its package name — what a registry-wide document (RubyGems'
/// compact `/versions`, a conda subdir's `repodata.json`) is composed from.
pub async fn held_registry(
    meta: &dyn ArtifactCacheMeta,
    cache: &dyn CacheStore,
    registry: &str,
) -> Vec<(String, HeldVersion)> {
    let rows = match meta.list_registry_artifacts(registry).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(registry = %registry, error = %e,
                "listing synthesis: could not read the registry's held set; answering as unheld");
            return Vec::new();
        }
    };
    let mut held: Vec<(String, HeldVersion)> = Vec::with_capacity(rows.len());
    for row in rows {
        if row.version.is_empty() {
            continue;
        }
        let artifact = key_artifact(&row);
        if held.iter().any(|(n, h)| {
            *n == row.package_name && h.version == row.version && h.artifact == artifact
        }) {
            continue;
        }
        let key = row
            .artifact_key
            .strip_prefix("artifact:")
            .unwrap_or(&row.artifact_key);
        let Ok(Some(entry)) = cache.get(&format!("meta:{key}")).await else {
            continue;
        };
        held.push((
            row.package_name.clone(),
            HeldVersion {
                version: row.version,
                artifact,
                checksum: entry.metadata.checksum,
                size: row.size_bytes,
                published_at: entry.metadata.published_at,
                received_at: row.cached_at,
                extra: entry.metadata.extra,
            },
        ));
    }
    held
}

/// Whether a listing document describes the whole registry rather than
/// one package, and so is composed from [`held_registry`].
pub fn is_registry_wide(kind: RegistryKind, doc_kind: DocumentKind) -> bool {
    match kind {
        RegistryKind::Rubygems => {
            matches!(doc_kind.as_str(), "compact-versions" | "compact-names")
        }
        RegistryKind::Conda => matches!(doc_kind.as_str(), "versions" | "current-repodata"),
        _ => false,
    }
}

/// The registry-wide documents of §4.3: RubyGems' compact `/versions` and
/// `/names`, a conda subdir's `repodata.json`. `name` is the coordinate the
/// route asked under (the subdir, for conda).
pub fn render_registry(
    kind: RegistryKind,
    doc_kind: DocumentKind,
    name: &str,
    held: &[(String, HeldVersion)],
) -> Option<VersionDocument> {
    if held.is_empty() {
        return None;
    }
    let count = held.len() as u32;
    let doc = match (kind, doc_kind.as_str()) {
        (RegistryKind::Rubygems, "compact-versions") => VersionDocument::text(
            "text/plain; charset=utf-8",
            rubygems_compact_versions(held)?,
        ),
        (RegistryKind::Rubygems, "compact-names") => {
            VersionDocument::text("text/plain; charset=utf-8", rubygems_compact_names(held))
        }
        (RegistryKind::Conda, "versions" | "current-repodata") => {
            VersionDocument::json(conda_repodata(name, held))
        }
        _ => return None,
    };
    Some(VersionDocument {
        synthesised: Some(count),
        ..doc
    })
}

/// The artifact sub-coordinate of a row, read off its key.
fn key_artifact(row: &crate::ports::ArtifactMeta) -> Option<String> {
    let key = row
        .artifact_key
        .strip_prefix("artifact:")
        .unwrap_or(&row.artifact_key);
    let prefix = format!("{}/{}/{}", row.registry, row.package_name, row.version);
    match key.strip_prefix(&prefix) {
        Some("") => None,
        Some(rest) => rest.strip_prefix('/').map(str::to_owned),
        None => None,
    }
}

/// The pointer rule, written once (§4.2): the highest held version under
/// the one order this server uses, a pre-release only when nothing else is
/// held.
pub fn highest(held: &[HeldVersion]) -> Option<&str> {
    let mut versions: Vec<&str> = held.iter().map(|h| h.version.as_str()).collect();
    versions.sort_by(|a, b| newest_first(a, b));
    versions.dedup();
    versions
        .iter()
        .find(|v| !is_prerelease(v))
        .or_else(|| versions.first())
        .copied()
}

/// The listing document of §4.3 for a kind, or `None` for a kind this
/// instance cannot compose one for — which the caller turns into the `503`
/// of RFC 0008.
pub fn render(
    kind: RegistryKind,
    doc_kind: DocumentKind,
    name: &str,
    held: &[HeldVersion],
    public_base: &str,
) -> Option<VersionDocument> {
    if held.is_empty() {
        return None;
    }
    let base = public_base.trim_end_matches('/');
    let count = held.len() as u32;
    let is = |k: DocumentKind, s: &str| k.as_str() == s;
    let doc = match (kind, doc_kind) {
        (RegistryKind::Npm, DocumentKind::Versions) => {
            VersionDocument::json(npm_packument(name, held, base))
        }
        (RegistryKind::Pypi, k) if k == DocumentKind::SIMPLE_JSON => VersionDocument {
            content_type: "application/vnd.pypi.simple.v1+json".to_owned(),
            body: crate::ports::DocumentBody::Json(pypi_simple_json(name, held, base)),
            synthesised: None,
        },
        (RegistryKind::Pypi, DocumentKind::Versions) => VersionDocument::text(
            "text/html; charset=utf-8",
            pypi_simple_html(name, held, base),
        ),
        (RegistryKind::Cargo, DocumentKind::Versions) => {
            VersionDocument::text("text/plain; charset=utf-8", cargo_sparse_index(name, held)?)
        }
        (RegistryKind::Goproxy, DocumentKind::Versions) => {
            VersionDocument::text("text/plain; charset=utf-8", go_list(held)?)
        }
        (RegistryKind::Goproxy, k) if k == DocumentKind::LATEST => {
            VersionDocument::json(go_info(highest_with_artifact(held, "zip")?, held))
        }
        (RegistryKind::Maven, DocumentKind::Versions) => {
            VersionDocument::text("application/xml", maven_metadata(name, held)?)
        }
        (RegistryKind::Nuget, DocumentKind::Versions) => {
            VersionDocument::json(nuget_flat_index(held)?)
        }
        (RegistryKind::Github | RegistryKind::Forgejo, DocumentKind::Versions) => {
            let releases = forge_releases(name, held, base);
            if releases.is_empty() {
                return None;
            }
            VersionDocument::json(Value::Array(releases))
        }
        (RegistryKind::Gitlab, DocumentKind::Versions) => {
            let releases = gitlab_releases(name, held, base);
            if releases.is_empty() {
                return None;
            }
            VersionDocument::json(Value::Array(releases))
        }
        (RegistryKind::Nodedist, DocumentKind::Versions) => {
            VersionDocument::text("text/plain; charset=utf-8", nodedist_index_tab(held)?)
        }
        (RegistryKind::Nodedist, k) if is(k, "index-json") => {
            VersionDocument::json(nodedist_index_json(held)?)
        }
        (RegistryKind::Sdkman, DocumentKind::Versions) => VersionDocument::text(
            "text/plain; charset=utf-8",
            sdkman_versions_all(name, held)?,
        ),
        (RegistryKind::Sdkman, k) if is(k, "sdkman-default") => {
            VersionDocument::text("text/plain; charset=utf-8", sdkman_default(name, held)?)
        }
        (RegistryKind::Rubygems, k) if is(k, "compact-info") => {
            VersionDocument::text("text/plain; charset=utf-8", rubygems_compact_info(held)?)
        }
        (RegistryKind::Rubygems, DocumentKind::Versions) => {
            VersionDocument::json(rubygems_versions_json(held)?)
        }
        (RegistryKind::Nuget, k) if is(k, "registration") => {
            VersionDocument::json(nuget_registration(name, held, base)?)
        }
        (RegistryKind::Composer, DocumentKind::Versions) => {
            VersionDocument::json(composer_p2(name, held, base, false)?)
        }
        (RegistryKind::Composer, k) if is(k, "p2-dev") => {
            VersionDocument::json(composer_p2(name, held, base, true)?)
        }
        (RegistryKind::Terraform, DocumentKind::Versions) => {
            VersionDocument::json(terraform_versions(name, held)?)
        }
        (RegistryKind::Terraform, k) if k == DocumentKind::PROVIDER_DOWNLOAD => {
            VersionDocument::json(terraform_provider_download(name, held, base)?)
        }
        _ => return None,
    };
    Some(VersionDocument {
        synthesised: Some(count),
        ..doc
    })
}

/// One release, by tag (§13.1 of the RFC: the document a pinned `mise
/// install` asks for first). `None` when no asset of that tag is held.
pub fn render_release(
    kind: RegistryKind,
    owner_repo: &str,
    tag: &str,
    held: &[HeldVersion],
    public_base: &str,
) -> Option<VersionDocument> {
    let base = public_base.trim_end_matches('/');
    let (doc, count) = match kind {
        RegistryKind::Github | RegistryKind::Forgejo => {
            let assets: Vec<&HeldVersion> = held
                .iter()
                .filter(|h| h.version == tag && h.asset_name().is_some())
                .collect();
            if assets.is_empty() {
                return None;
            }
            (forge_release(owner_repo, tag, &assets, base), assets.len())
        }
        RegistryKind::Gitlab => {
            let links: Vec<&HeldVersion> = held
                .iter()
                .filter(|h| h.version == tag && h.link_name().is_some())
                .collect();
            if links.is_empty() {
                return None;
            }
            (gitlab_release(owner_repo, tag, &links, base), links.len())
        }
        _ => return None,
    };
    let mut doc = VersionDocument::json(doc);
    doc.synthesised = Some(count as u32);
    Some(doc)
}

/// A document about one version that its kind serves through the artifact
/// route — a forge release by tag, Go's `@v/{v}.info` — composed from the
/// held set (§4.3). `None` when the kind has no such document or the
/// version is not held.
pub fn render_artifact_document(
    kind: RegistryKind,
    name: &str,
    version: &str,
    held: &[HeldVersion],
    public_base: &str,
) -> Option<VersionDocument> {
    match kind {
        RegistryKind::Github | RegistryKind::Forgejo | RegistryKind::Gitlab => {
            render_release(kind, name, version, held, public_base)
        }
        RegistryKind::Goproxy => {
            let zip = held
                .iter()
                .find(|h| h.version == version && h.artifact.as_deref() == Some("zip"))?;
            let mut doc = VersionDocument::json(go_info(zip, held));
            doc.synthesised = Some(1);
            Some(doc)
        }
        _ => None,
    }
}

/// Whether a coordinate on this kind is a document about one version that
/// the artifact route serves — the request that names its version before
/// any listing (§4.4), and the one composed by [`render_artifact_document`].
pub fn is_document_by_version(kind: RegistryKind, version: &str, artifact: Option<&str>) -> bool {
    if artifact.is_some() || version.is_empty() {
        return false;
    }
    match kind {
        RegistryKind::Github | RegistryKind::Forgejo | RegistryKind::Gitlab => {
            version != "releases"
        }
        RegistryKind::Goproxy => version != "latest",
        _ => false,
    }
}

/// The key a by-version document miss is filed under: the package and
/// the document's name, the way a listing miss is.
pub fn document_by_version_key(kind: RegistryKind, name: &str) -> String {
    match kind {
        RegistryKind::Goproxy => format!("{name} (info)"),
        _ => format!("{name} (release)"),
    }
}

/// The packument npm parses: `versions`, `dist-tags.latest`, and `time`
/// only for the versions whose entry is dated.
fn npm_packument(name: &str, held: &[HeldVersion], base: &str) -> Value {
    let mut versions = Map::new();
    let mut time = Map::new();
    for h in held {
        let mut dist = Map::new();
        dist.insert(
            "tarball".to_owned(),
            Value::String(format!("{base}/{name}/{}/tarball", h.version)),
        );
        if let Some(sri) = h.checksum.as_deref().and_then(sha256_sri) {
            dist.insert("integrity".to_owned(), Value::String(sri));
        }
        let mut entry = Map::new();
        entry.insert("name".to_owned(), Value::String(name.to_owned()));
        entry.insert("version".to_owned(), Value::String(h.version.clone()));
        entry.insert("dist".to_owned(), Value::Object(dist));
        if let Some(desc) = h.extra.get("description").and_then(Value::as_str) {
            entry.insert("description".to_owned(), Value::String(desc.to_owned()));
        }
        versions.insert(h.version.clone(), Value::Object(entry));
        if let Some(at) = h.published_at {
            time.insert(h.version.clone(), Value::String(at.to_rfc3339()));
        }
    }
    let mut doc = json!({
        "name": name,
        "dist-tags": { "latest": highest(held) },
        "versions": Value::Object(versions),
    });
    if !time.is_empty() {
        doc["time"] = Value::Object(time);
    }
    doc
}

/// PEP 691, the page pip negotiates for: one `files[]` entry per held
/// file, the URL on this proxy's own `packages/` route with the hash
/// fragment the served page carries.
fn pypi_simple_json(name: &str, held: &[HeldVersion], base: &str) -> Value {
    let mut files = Vec::with_capacity(held.len());
    let mut versions: Vec<&str> = Vec::new();
    for h in held {
        let Some(filename) = h.artifact.as_deref() else {
            continue;
        };
        let mut file = json!({
            "filename": filename,
            "url": match &h.checksum {
                Some(sum) => format!("{base}/packages/{filename}#sha256={sum}"),
                None => format!("{base}/packages/{filename}"),
            },
            "hashes": match &h.checksum {
                Some(sum) => json!({ "sha256": sum }),
                None => json!({}),
            },
        });
        if let Some(size) = h.size {
            file["size"] = json!(size);
        }
        if let Some(at) = h.published_at {
            file["upload-time"] = Value::String(at.to_rfc3339());
        }
        files.push(file);
        if !versions.contains(&h.version.as_str()) {
            versions.push(&h.version);
        }
    }
    versions.sort_by(|a, b| newest_first(b, a));
    json!({
        "meta": { "api-version": "1.1" },
        "name": name,
        "versions": versions,
        "files": files,
    })
}

/// The release listing: one release per held tag, newest first, the way
/// the forge orders its own.
fn forge_releases(owner_repo: &str, held: &[HeldVersion], base: &str) -> Vec<Value> {
    let mut tags: Vec<&str> = held
        .iter()
        .filter(|h| h.asset_name().is_some())
        .map(|h| h.version.as_str())
        .collect();
    tags.sort_by(|a, b| newest_first(a, b));
    tags.dedup();
    tags.into_iter()
        .map(|tag| {
            let assets: Vec<&HeldVersion> = held
                .iter()
                .filter(|h| h.version == tag && h.asset_name().is_some())
                .collect();
            forge_release(owner_repo, tag, &assets, base)
        })
        .collect()
}

/// A release as the forge's API shapes one, reduced to what a client
/// resolving from it reads: `tag_name` and `assets[]` with `name`, `size`,
/// and the two URL fields both pointing at the download by name. No `id`,
/// no `body`: nothing the held keys do not say (§4.2, last rule).
fn forge_release(owner_repo: &str, tag: &str, assets: &[&HeldVersion], base: &str) -> Value {
    let tag_seg = crate::services::blocking::encode_package_segment(tag);
    let published_at = assets.iter().find_map(|h| h.published_at);
    let asset_docs: Vec<Value> = assets
        .iter()
        .filter_map(|h| {
            let name = h.asset_name()?;
            let download = format!(
                "{base}/{owner_repo}/releases/download/{tag_seg}/{}",
                crate::services::blocking::encode_package_segment(name)
            );
            // `url` and `size` are required fields of an asset in the forge's
            // own schema, and mise deserialises strictly: without them the
            // whole release fails to decode (measured, §13.2). There is no
            // forge id to give `url` an asset endpoint, so it names the
            // download route by name — the one address this instance holds.
            let mut asset = json!({
                "name": name,
                "browser_download_url": download,
                "url": download,
                "size": h.size.unwrap_or(0),
                "content_type": "application/octet-stream",
                "state": "uploaded",
            });
            if let Some(sum) = &h.checksum {
                asset["digest"] = Value::String(format!("sha256:{sum}"));
            }
            Some(asset)
        })
        .collect();
    // `created_at` is a required field of the forge's release schema and
    // mise decodes strictly (measured, §13.2). An import records no upstream
    // date (RFC 0008 §14.2), so the release is dated by when this instance
    // received its first asset — a true statement about this instance, and
    // the same caveat 0008 §14.2 gives: a client's own release-age gate reads
    // an imported release as new. `published_at` is only what upstream said.
    let created_at = published_at
        .or_else(|| assets.iter().map(|h| h.received_at).min())
        .unwrap_or_else(Utc::now);
    let mut release = json!({
        "tag_name": tag,
        "created_at": created_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "name": tag,
        "draft": false,
        "prerelease": is_prerelease(tag),
        "assets": asset_docs,
    });
    if let Some(at) = published_at {
        release["published_at"] =
            Value::String(at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    }
    release
}

/// The highest held version that has the named artifact.
fn highest_with_artifact<'a>(held: &'a [HeldVersion], artifact: &str) -> Option<&'a HeldVersion> {
    let with: Vec<HeldVersion> = held
        .iter()
        .filter(|h| h.artifact.as_deref() == Some(artifact))
        .cloned()
        .collect();
    let top = highest(&with)?.to_owned();
    held.iter()
        .find(|h| h.version == top && h.artifact.as_deref() == Some(artifact))
}

/// Distinct held versions, oldest first, optionally only those with an
/// artifact the predicate accepts.
fn versions_ascending(held: &[HeldVersion], accept: impl Fn(&HeldVersion) -> bool) -> Vec<String> {
    let mut v: Vec<String> = held
        .iter()
        .filter(|h| accept(h))
        .map(|h| h.version.clone())
        .collect();
    v.sort_by(|a, b| newest_first(b, a));
    v.dedup();
    v
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// PEP 503: the HTML simple page, for the pip that does not negotiate and
/// the tools that read it by hand.
fn pypi_simple_html(name: &str, held: &[HeldVersion], base: &str) -> String {
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html>\n  <head>\n");
    out.push_str("    <meta name=\"pypi:repository-version\" content=\"1.0\">\n");
    out.push_str(&format!(
        "    <title>Links for {}</title>\n  </head>\n  <body>\n",
        html_escape(name)
    ));
    out.push_str(&format!("    <h1>Links for {}</h1>\n", html_escape(name)));
    for h in held {
        let Some(filename) = h.artifact.as_deref() else {
            continue;
        };
        let href = match &h.checksum {
            Some(sum) => format!("{base}/packages/{filename}#sha256={sum}"),
            None => format!("{base}/packages/{filename}"),
        };
        out.push_str(&format!(
            "    <a href=\"{}\">{}</a><br />\n",
            html_escape(&href),
            html_escape(filename)
        ));
    }
    out.push_str("  </body>\n</html>\n");
    out
}

/// The sparse index: one line per held version, from the facts the import
/// read off the crate (`extra.cargo`, §13.4). A version without them is
/// not listed, and a package with none composes nothing — cargo would
/// build against a line that lied about its dependencies.
fn cargo_sparse_index(name: &str, held: &[HeldVersion]) -> Option<String> {
    let mut rows: Vec<&HeldVersion> = held
        .iter()
        .filter(|h| h.extra.get("cargo").is_some_and(Value::is_object))
        .collect();
    if rows.is_empty() {
        return None;
    }
    rows.sort_by(|a, b| newest_first(&b.version, &a.version));
    let mut out = String::new();
    for h in rows {
        let facts = &h.extra["cargo"];
        let line = json!({
            "name": name,
            "vers": h.version,
            "deps": facts.get("deps").cloned().unwrap_or_else(|| json!([])),
            "cksum": h.checksum.clone().unwrap_or_default(),
            "features": facts.get("features").cloned().unwrap_or_else(|| json!({})),
            "yanked": false,
            "links": facts.get("links").cloned().unwrap_or(Value::Null),
            "v": 2,
        });
        out.push_str(&line.to_string());
        out.push('\n');
    }
    Some(out)
}

/// `@v/list`: the versions whose module zip is held, one per line.
fn go_list(held: &[HeldVersion]) -> Option<String> {
    let versions = versions_ascending(held, |h| h.artifact.as_deref() == Some("zip"));
    if versions.is_empty() {
        return None;
    }
    Some(versions.join("\n") + "\n")
}

/// `.info` / `@latest`: `Version` and `Time`. The time is when this
/// instance received the zip unless upstream's date is known (§13.2).
fn go_info(zip: &HeldVersion, held: &[HeldVersion]) -> Value {
    let time = zip.published_at.unwrap_or_else(|| {
        held.iter()
            .filter(|h| h.version == zip.version)
            .map(|h| h.received_at)
            .min()
            .unwrap_or(zip.received_at)
    });
    json!({
        "Version": zip.version,
        "Time": time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    })
}

/// `maven-metadata.xml` for `group:artifact`: every held version, `latest`
/// the highest, `release` the highest that is not a snapshot or pre-release.
fn maven_metadata(name: &str, held: &[HeldVersion]) -> Option<String> {
    let (group, artifact) = name.split_once(':')?;
    let versions = versions_ascending(held, |h| h.artifact.is_some());
    if versions.is_empty() {
        return None;
    }
    let latest = versions.last()?.clone();
    let release = versions
        .iter()
        .rev()
        .find(|v| !is_prerelease(v) && !v.ends_with("-SNAPSHOT"))
        .cloned();
    let updated = held
        .iter()
        .map(|h| h.received_at)
        .max()
        .map(|t| t.format("%Y%m%d%H%M%S").to_string())
        .unwrap_or_default();
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<metadata>\n");
    xml.push_str(&format!("  <groupId>{}</groupId>\n", xml_escape(group)));
    xml.push_str(&format!(
        "  <artifactId>{}</artifactId>\n",
        xml_escape(artifact)
    ));
    xml.push_str("  <versioning>\n");
    xml.push_str(&format!("    <latest>{}</latest>\n", xml_escape(&latest)));
    if let Some(r) = release {
        xml.push_str(&format!("    <release>{}</release>\n", xml_escape(&r)));
    }
    xml.push_str("    <versions>\n");
    for v in &versions {
        xml.push_str(&format!("      <version>{}</version>\n", xml_escape(v)));
    }
    xml.push_str("    </versions>\n");
    xml.push_str(&format!("    <lastUpdated>{updated}</lastUpdated>\n"));
    xml.push_str("  </versioning>\n</metadata>\n");
    Some(xml)
}

fn xml_escape(s: &str) -> String {
    html_escape(s).replace('\'', "&apos;")
}

/// The flat index: `{"versions": [...]}` of the versions whose package is
/// held, lowest first as nuget.org orders it.
fn nuget_flat_index(held: &[HeldVersion]) -> Option<Value> {
    let versions = versions_ascending(held, |h| {
        h.artifact.as_deref().is_some_and(|a| a.ends_with(".nupkg"))
    });
    if versions.is_empty() {
        return None;
    }
    Some(json!({ "versions": versions }))
}

/// GitLab's release listing: one release per held tag, its `assets.links`
/// the held downloads, `sources` none (an archive is held under its
/// commit, not its tag).
fn gitlab_releases(project: &str, held: &[HeldVersion], base: &str) -> Vec<Value> {
    let mut tags: Vec<&str> = held
        .iter()
        .filter(|h| h.link_name().is_some())
        .map(|h| h.version.as_str())
        .collect();
    tags.sort_by(|a, b| newest_first(a, b));
    tags.dedup();
    tags.into_iter()
        .map(|tag| {
            let links: Vec<&HeldVersion> = held
                .iter()
                .filter(|h| h.version == tag && h.link_name().is_some())
                .collect();
            gitlab_release(project, tag, &links, base)
        })
        .collect()
}

fn gitlab_release(project: &str, tag: &str, links: &[&HeldVersion], base: &str) -> Value {
    let tag_seg = crate::services::blocking::encode_package_segment(tag);
    let created_at = links
        .iter()
        .find_map(|h| h.published_at)
        .or_else(|| links.iter().map(|h| h.received_at).min())
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let link_docs: Vec<Value> = links
        .iter()
        .filter_map(|h| {
            let name = h.link_name()?;
            let url = format!(
                "{base}/{project}/-/releases/{tag_seg}/downloads/{}",
                crate::services::blocking::encode_package_segment(name)
            );
            Some(json!({
                "name": name,
                "url": url,
                "direct_asset_url": url,
                "link_type": "other",
            }))
        })
        .collect();
    json!({
        "tag_name": tag,
        "name": tag,
        "created_at": created_at,
        "released_at": created_at,
        "upcoming_release": false,
        "assets": {
            "count": link_docs.len(),
            "sources": [],
            "links": link_docs,
        },
    })
}

/// The platform a nodedist file name carries: `node-v22.11.0-linux-x64.tar.xz`
/// → `linux-x64`. Checksum files and unknown shapes carry none.
fn nodedist_platform(version: &str, file: &str) -> Option<String> {
    let rest = file.strip_prefix(&format!("node-{version}-"))?;
    for ext in [".tar.xz", ".tar.gz", ".zip", ".7z", ".msi", ".pkg"] {
        if let Some(p) = rest.strip_suffix(ext) {
            return Some(p.to_owned());
        }
    }
    None
}

/// The rows of `index.tab` / `index.json`: `(version, date, files)`, newest
/// first, for the versions with at least one platform file held.
fn nodedist_rows(held: &[HeldVersion]) -> Vec<(String, String, Vec<String>)> {
    let mut versions: Vec<&str> = held.iter().map(|h| h.version.as_str()).collect();
    versions.sort_by(|a, b| newest_first(a, b));
    versions.dedup();
    versions
        .into_iter()
        .filter_map(|v| {
            let rows: Vec<&HeldVersion> = held.iter().filter(|h| h.version == v).collect();
            let mut files: Vec<String> = rows
                .iter()
                .filter_map(|h| nodedist_platform(v, h.artifact.as_deref()?))
                .collect();
            files.sort();
            files.dedup();
            if files.is_empty() {
                return None;
            }
            let date = rows
                .iter()
                .find_map(|h| h.published_at)
                .or_else(|| rows.iter().map(|h| h.received_at).min())
                .unwrap_or_else(Utc::now)
                .format("%Y-%m-%d")
                .to_string();
            Some((v.to_owned(), date, files))
        })
        .collect()
}

/// `index.tab`, header first (nvm drops line 1 unconditionally). The
/// columns nvm does not read carry `-`; `lts` is `-` because this
/// instance cannot know a release's LTS status, and `security` `false`.
fn nodedist_index_tab(held: &[HeldVersion]) -> Option<String> {
    let rows = nodedist_rows(held);
    if rows.is_empty() {
        return None;
    }
    let mut out =
        String::from("version\tdate\tfiles\tnpm\tv8\tuv\tzlib\topenssl\tmodules\tlts\tsecurity\n");
    for (v, date, files) in rows {
        out.push_str(&format!(
            "{v}\t{date}\t{}\t-\t-\t-\t-\t-\t-\t-\tfalse\n",
            files.join(",")
        ));
    }
    Some(out)
}

fn nodedist_index_json(held: &[HeldVersion]) -> Option<Value> {
    let rows = nodedist_rows(held);
    if rows.is_empty() {
        return None;
    }
    Some(Value::Array(
        rows.into_iter()
            .map(|(v, date, files)| {
                json!({
                    "version": v,
                    "date": date,
                    "files": files,
                    "npm": "-",
                    "v8": "-",
                    "uv": "-",
                    "zlib": "-",
                    "openssl": "-",
                    "modules": "-",
                    "lts": false,
                    "security": false,
                })
            })
            .collect(),
    ))
}

/// The held versions of `candidate` for `platform` (or a universal
/// binary), as `versions/all` lists them: comma-separated.
fn sdkman_held(name: &str, held: &[HeldVersion]) -> Vec<String> {
    let (_, platform) = name.split_once('/').unwrap_or((name, "UNIVERSAL"));
    versions_ascending(held, |h| {
        h.artifact.as_deref().is_some_and(|p| {
            p.eq_ignore_ascii_case(platform) || p.eq_ignore_ascii_case("universal")
        })
    })
}

fn sdkman_versions_all(name: &str, held: &[HeldVersion]) -> Option<String> {
    let versions = sdkman_held(name, held);
    if versions.is_empty() {
        return None;
    }
    Some(versions.join(","))
}

fn sdkman_default(name: &str, held: &[HeldVersion]) -> Option<String> {
    let versions = sdkman_held(name, held);
    versions.into_iter().max_by(|a, b| newest_first(b, a))
}

/// One line of `/info/{gem}`: `VERSION[-PLATFORM] dep:req,dep:req|checksum:SHA256`,
/// from the facts the import read off the gemspec (`extra.rubygems`). A
/// gem without them is not listed: a resolver handed an empty dependency
/// list installs a gem without the gems it needs.
fn rubygems_info_lines(held: &[HeldVersion]) -> Vec<String> {
    let mut rows: Vec<&HeldVersion> = held
        .iter()
        .filter(|h| h.artifact.as_deref() == Some("gem") && h.extra.get("rubygems").is_some())
        .collect();
    rows.sort_by(|a, b| newest_first(&b.version, &a.version));
    rows.iter()
        .map(|h| {
            let facts = &h.extra["rubygems"];
            let platform = facts
                .get("platform")
                .and_then(Value::as_str)
                .unwrap_or("ruby");
            let version = if platform == "ruby" || platform.is_empty() {
                h.version.clone()
            } else {
                format!("{}-{platform}", h.version)
            };
            let deps: Vec<String> = facts
                .get("dependencies")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|d| {
                            let name = d.get("name")?.as_str()?;
                            let req = d.get("requirement").and_then(Value::as_str).unwrap_or("");
                            Some(format!("{name}:{req}"))
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!(
                "{version} {}|checksum:{}",
                deps.join(","),
                h.checksum.clone().unwrap_or_default()
            )
        })
        .collect()
}

fn rubygems_compact_info(held: &[HeldVersion]) -> Option<String> {
    let lines = rubygems_info_lines(held);
    if lines.is_empty() {
        return None;
    }
    Some(format!("---\n{}\n", lines.join("\n")))
}

/// `/versions`: one line per gem — `name v1,v2 md5(info)` — the md5 being
/// that of the `/info/{gem}` document Bundler will fetch next, which is
/// how Bundler pairs the two.
fn rubygems_compact_versions(held: &[(String, HeldVersion)]) -> Option<String> {
    use md5::Digest as _;
    let mut names: Vec<&str> = held.iter().map(|(n, _)| n.as_str()).collect();
    names.sort();
    names.dedup();
    let created = held
        .iter()
        .map(|(_, h)| h.received_at)
        .max()
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut out = format!("created_at: {created}\n---\n");
    let mut any = false;
    for name in names {
        let rows: Vec<HeldVersion> = held
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, h)| h.clone())
            .collect();
        let Some(info) = rubygems_compact_info(&rows) else {
            continue;
        };
        let versions: Vec<String> = rubygems_info_lines(&rows)
            .iter()
            .filter_map(|l| l.split(' ').next().map(str::to_owned))
            .collect();
        out.push_str(&format!(
            "{name} {} {}\n",
            versions.join(","),
            hex::encode(md5::Md5::digest(info.as_bytes()))
        ));
        any = true;
    }
    any.then_some(out)
}

fn rubygems_compact_names(held: &[(String, HeldVersion)]) -> String {
    let mut names: Vec<&str> = held
        .iter()
        .filter(|(_, h)| h.artifact.as_deref() == Some("gem"))
        .map(|(n, _)| n.as_str())
        .collect();
    names.sort();
    names.dedup();
    format!("---\n{}\n", names.join("\n"))
}

/// `/api/v1/versions/{name}.json`.
fn rubygems_versions_json(held: &[HeldVersion]) -> Option<Value> {
    let mut rows: Vec<&HeldVersion> = held
        .iter()
        .filter(|h| h.artifact.as_deref() == Some("gem"))
        .collect();
    if rows.is_empty() {
        return None;
    }
    rows.sort_by(|a, b| newest_first(&a.version, &b.version));
    Some(Value::Array(
        rows.iter()
            .map(|h| {
                json!({
                    "number": h.version,
                    "platform": h.extra.get("rubygems").and_then(|r| r.get("platform")).and_then(Value::as_str).unwrap_or("ruby"),
                    "sha": h.checksum,
                    "created_at": h.published_at.unwrap_or(h.received_at).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "prerelease": is_prerelease(&h.version),
                })
            })
            .collect(),
    ))
}

/// A subdir's `repodata.json`: each held package of that subdir, keyed by
/// file name, its entry the `info/index.json` the import read
/// (`extra.conda`). A subdir with nothing held gets an *empty* repodata
/// when the registry holds anything at all, because a client reads every
/// subdir of a channel and "no packages here" is a true statement.
fn conda_repodata(subdir: &str, held: &[(String, HeldVersion)]) -> Value {
    let mut packages = Map::new();
    let mut packages_conda = Map::new();
    for (_, h) in held {
        let Some(facts) = h.extra.get("conda").filter(|f| f.is_object()) else {
            continue;
        };
        // The proxy files a package as `{platform}/{filename}`; a locally
        // published one as its file name alone. The subdir is the one the
        // package's own index names, or the selector's.
        let Some(selector) = h.artifact.as_deref() else {
            continue;
        };
        let (selector_subdir, filename) = match selector.split_once('/') {
            Some((p, f)) => (Some(p), f),
            None => (None, selector),
        };
        let in_subdir = facts
            .get("subdir")
            .and_then(Value::as_str)
            .or(selector_subdir)
            .unwrap_or("noarch");
        if in_subdir != subdir {
            continue;
        }
        let mut entry = json!({
            "name": facts.get("name").cloned().unwrap_or(Value::Null),
            "version": facts.get("version").cloned().unwrap_or(Value::String(h.version.clone())),
            "build": facts.get("build").cloned().unwrap_or(Value::Null),
            "build_number": facts.get("build_number").cloned().unwrap_or(json!(0)),
            "depends": facts.get("depends").cloned().unwrap_or_else(|| json!([])),
            "subdir": subdir,
        });
        if let Some(l) = facts.get("license") {
            entry["license"] = l.clone();
        }
        if let Some(sum) = &h.checksum {
            entry["sha256"] = Value::String(sum.clone());
        }
        if let Some(size) = h.size {
            entry["size"] = json!(size);
        }
        entry["timestamp"] = json!(h.published_at.unwrap_or(h.received_at).timestamp_millis());
        if filename.ends_with(".conda") {
            packages_conda.insert(filename.to_owned(), entry);
        } else {
            packages.insert(filename.to_owned(), entry);
        }
    }
    json!({
        "info": { "subdir": subdir },
        "packages": Value::Object(packages),
        "packages.conda": Value::Object(packages_conda),
        "removed": [],
        "repodata_version": 1,
    })
}

/// The registration index, inline, one page, from the `.nuspec` the
/// import read (`extra.nuget`).
fn nuget_registration(id: &str, held: &[HeldVersion], base: &str) -> Option<Value> {
    let mut rows: Vec<&HeldVersion> = held
        .iter()
        .filter(|h| h.artifact.as_deref().is_some_and(|a| a.ends_with(".nupkg")))
        .filter(|h| h.extra.get("nuget").is_some())
        .collect();
    if rows.is_empty() {
        return None;
    }
    rows.sort_by(|a, b| newest_first(&b.version, &a.version));
    let items: Vec<Value> = rows
        .iter()
        .map(|h| {
            let facts = &h.extra["nuget"];
            let v = &h.version;
            let entry_id = format!("{base}/nuget/v3/registration5/{id}/{v}.json");
            json!({
                "@id": entry_id,
                "catalogEntry": {
                    "@id": entry_id,
                    "id": facts.get("id").cloned().unwrap_or(Value::String(id.to_owned())),
                    "version": v,
                    "description": facts.get("description").cloned().unwrap_or(Value::Null),
                    "authors": facts.get("authors").cloned().unwrap_or(Value::Null),
                    "tags": facts.get("tags").cloned().unwrap_or(Value::Null),
                    "listed": true,
                    "published": h.published_at.unwrap_or(h.received_at).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                },
                "packageContent": format!("{base}/nuget/v3/flat/{id}/{v}/{id}.{v}.nupkg"),
            })
        })
        .collect();
    let lower = rows.first().map(|h| h.version.clone()).unwrap_or_default();
    let upper = rows.last().map(|h| h.version.clone()).unwrap_or_default();
    Some(json!({
        "@id": format!("{base}/nuget/v3/registration5/{id}/index.json"),
        "count": 1,
        "items": [{
            "@id": format!("{base}/nuget/v3/registration5/{id}/page/{lower}/{upper}.json"),
            "lower": lower,
            "upper": upper,
            "count": items.len(),
            "items": items,
        }],
    }))
}

/// The `p2` document: one entry per held dist, its body the `composer.json`
/// the import read (`extra.composer`), its `dist` this proxy's own route.
/// The `~dev` variant is answered empty when nothing dev is held, because
/// Composer asks for it whether or not anything is.
fn composer_p2(name: &str, held: &[HeldVersion], base: &str, dev: bool) -> Option<Value> {
    let mut rows: Vec<&HeldVersion> = held
        .iter()
        .filter(|h| h.artifact.as_deref() == Some("dist") && h.extra.get("composer").is_some())
        .filter(|h| h.version.starts_with("dev-") == dev)
        .collect();
    if rows.is_empty() && !dev {
        return None;
    }
    rows.sort_by(|a, b| newest_first(&a.version, &b.version));
    let (vendor, package) = name.split_once('/')?;
    let entries: Vec<Value> = rows
        .iter()
        .map(|h| {
            let mut entry = h.extra["composer"].clone();
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("name".into(), Value::String(name.to_owned()));
                obj.insert("version".into(), Value::String(h.version.clone()));
                let mut dist = json!({
                    "type": "zip",
                    "url": format!("{base}/dist/{vendor}/{package}/{}", h.version),
                });
                if let Some(sum) = &h.checksum {
                    dist["shasum"] = Value::String(sum.clone());
                }
                obj.insert("dist".into(), dist);
                obj.remove("source");
            }
            entry
        })
        .collect();
    Some(json!({
        "packages": { name: entries },
        "minified": "composer/2.0",
    }))
}

/// npm's `dist.integrity` for a hex SHA-256: the Subresource Integrity
/// form, `sha256-<base64 of the raw digest>`.
/// A row that is a provider archive: its selector is a platform, `os/arch`.
fn terraform_platform(h: &HeldVersion) -> Option<(&str, &str)> {
    h.artifact.as_deref()?.split_once('/')
}

/// Terraform's `versions` listing (RFC 0008-bis §13.7). A provider lists
/// each held version with the platforms its archives are held for and the
/// protocols its download document named; a module lists its held
/// tarballs. A provider version with no archive — a checksum list alone —
/// is not a version anyone can install, and is not listed.
fn terraform_versions(name: &str, held: &[HeldVersion]) -> Option<Value> {
    if name.starts_with("modules/") {
        let versions = versions_ascending(held, |h| h.artifact.is_none());
        if versions.is_empty() {
            return None;
        }
        return Some(json!({ "modules": [{
            "source": name.trim_start_matches("modules/"),
            "versions": versions.iter().map(|v| json!({ "version": v })).collect::<Vec<_>>(),
        }] }));
    }
    let mut versions: Vec<&str> = held
        .iter()
        .filter(|h| terraform_platform(h).is_some())
        .map(|h| h.version.as_str())
        .collect();
    versions.sort_by(|a, b| newest_first(a, b));
    versions.dedup();
    if versions.is_empty() {
        return None;
    }
    let listed: Vec<Value> = versions
        .iter()
        .map(|v| {
            let mut platforms: Vec<(&str, &str)> = held
                .iter()
                .filter(|h| h.version == *v)
                .filter_map(terraform_platform)
                .collect();
            platforms.sort_unstable();
            platforms.dedup();
            json!({
                "version": v,
                "protocols": terraform_protocols(held, v),
                "platforms": platforms
                    .iter()
                    .map(|(os, arch)| json!({ "os": os, "arch": arch }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    Some(json!({ "versions": listed }))
}

/// The protocols a version's download document named, from whichever of
/// its archive rows carries them; `[]` when none does, which Terraform
/// reads as "not stated" rather than as an error.
fn terraform_protocols(held: &[HeldVersion], version: &str) -> Value {
    held.iter()
        .filter(|h| h.version == version)
        .find_map(|h| {
            h.extra
                .get("terraform")?
                .get("protocols")?
                .as_array()
                .cloned()
        })
        .map(Value::Array)
        .unwrap_or_else(|| json!([]))
}

/// A provider's download document, composed for one held archive (RFC
/// 0008-bis §13.7). `name` is the coordinate the route asked under,
/// `providers/{ns}/{type}/{version}/download/{os}/{arch}`.
///
/// Terraform installs nothing it cannot verify: it fetches the checksum
/// list and the list's signature the document names, checks the signature
/// against `signing_keys`, then the archive against its line in the list.
/// So the document is composed only when all three of the archive, the
/// checksum list and the signature are held — a document for a provider
/// this instance could not let the client verify would lead it to a
/// refusal, and the `503` names the gap instead. The keys are the
/// publisher's, carried by the bundle off the connected side's document;
/// this instance signs nothing.
fn terraform_provider_download(name: &str, held: &[HeldVersion], base: &str) -> Option<Value> {
    let parts: Vec<&str> = name.split('/').collect();
    let ["providers", ns, ptype, version, "download", os, arch] = parts.as_slice() else {
        return None;
    };
    let platform = format!("{os}/{arch}");
    let archive = held
        .iter()
        .find(|h| h.version == *version && h.artifact.as_deref() == Some(platform.as_str()))?;
    let shasum = archive.checksum.as_deref()?.to_ascii_lowercase();
    let sidecar = |what: &str| {
        held.iter()
            .find(|h| h.version == *version && h.artifact.as_deref() == Some(what))
    };
    let shasums = sidecar("shasums")?;
    sidecar("shasums.sig")?;
    // The archive's line in the list is looked up by file name, and the
    // name is in the list: the entry whose digest is this archive's.
    let filename = shasums
        .extra
        .get("terraform")
        .and_then(|t| t.get("sums"))
        .and_then(Value::as_object)
        .and_then(|sums| {
            sums.iter()
                .find(|(_, d)| d.as_str().is_some_and(|d| d.eq_ignore_ascii_case(&shasum)))
                .map(|(f, _)| f.clone())
        })
        .unwrap_or_else(|| format!("terraform-provider-{ptype}_{version}_{os}_{arch}.zip"));
    let facts = archive.extra.get("terraform");
    let signing_keys = facts
        .and_then(|t| t.get("signing_keys"))
        .filter(|k| k.is_object())
        .cloned()
        .unwrap_or_else(|| json!({ "gpg_public_keys": [] }));
    let prefix = format!("{base}/v1/providers/{ns}/{ptype}/{version}");
    Some(json!({
        "protocols": terraform_protocols(held, version),
        "os": os,
        "arch": arch,
        "filename": filename,
        "download_url": format!("{prefix}/artifact/{os}/{arch}"),
        "shasums_url": format!("{prefix}/shasums"),
        "shasums_signature_url": format!("{prefix}/shasums.sig"),
        "shasum": shasum,
        "signing_keys": signing_keys,
    }))
}

fn sha256_sri(hex_digest: &str) -> Option<String> {
    use base64::Engine;
    let raw = hex::decode(hex_digest).ok()?;
    if raw.len() != 32 {
        return None;
    }
    Some(format!(
        "sha256-{}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(version: &str, artifact: Option<&str>) -> HeldVersion {
        HeldVersion {
            version: version.to_owned(),
            artifact: artifact.map(str::to_owned),
            checksum: Some("ab".repeat(32)),
            size: Some(1234),
            published_at: None,
            received_at: "2026-09-05T08:00:00Z".parse().unwrap(),
            extra: Value::Null,
        }
    }

    #[test]
    fn the_pointer_is_the_highest_stable_held_and_a_prerelease_only_alone() {
        let set = [
            held("1.2.0", None),
            held("1.10.0", None),
            held("2.0.0-rc.1", None),
        ];
        assert_eq!(
            highest(&set),
            Some("1.10.0"),
            "semver order, not string order"
        );
        let only_pre = [held("2.0.0-rc.1", None), held("2.0.0-rc.2", None)];
        assert_eq!(highest(&only_pre), Some("2.0.0-rc.2"));
        assert_eq!(highest(&[]), None);
    }

    #[test]
    fn an_npm_packument_names_every_held_version_and_points_home() {
        let set = [held("1.3.0", None), held("1.2.0", None)];
        let doc = render(
            RegistryKind::Npm,
            DocumentKind::Versions,
            "left-pad",
            &set,
            "http://hub/proxy/npm/",
        )
        .expect("npm has a packument");
        assert_eq!(doc.synthesised, Some(2));
        let crate::ports::DocumentBody::Json(v) = doc.body else {
            panic!("json")
        };
        assert_eq!(v["dist-tags"]["latest"], "1.3.0");
        assert_eq!(
            v["versions"]["1.3.0"]["dist"]["tarball"],
            "http://hub/proxy/npm/left-pad/1.3.0/tarball"
        );
        assert!(v["versions"]["1.3.0"]["dist"]["integrity"]
            .as_str()
            .unwrap()
            .starts_with("sha256-"));
        assert!(v.get("time").is_none(), "an undated entry invents no date");
        assert_eq!(v["versions"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn a_pypi_simple_page_lists_the_held_files_with_their_hashes() {
        let set = [
            held("1.17.0", Some("six-1.17.0-py2.py3-none-any.whl")),
            held("1.17.0", Some("six-1.17.0.tar.gz")),
        ];
        let doc = render(
            RegistryKind::Pypi,
            DocumentKind::SIMPLE_JSON,
            "six",
            &set,
            "http://hub/proxy/pypi",
        )
        .unwrap();
        assert_eq!(doc.content_type, "application/vnd.pypi.simple.v1+json");
        let crate::ports::DocumentBody::Json(v) = doc.body else {
            panic!("json")
        };
        assert_eq!(v["files"].as_array().unwrap().len(), 2);
        assert_eq!(
            v["files"][0]["url"],
            format!(
                "http://hub/proxy/pypi/packages/six-1.17.0-py2.py3-none-any.whl#sha256={}",
                "ab".repeat(32)
            )
        );
        assert_eq!(v["files"][0]["hashes"]["sha256"], "ab".repeat(32));
        assert_eq!(v["versions"], json!(["1.17.0"]));
        // The HTML page too (phase 3), for the pip that does not negotiate.
        let html = render(
            RegistryKind::Pypi,
            DocumentKind::Versions,
            "six",
            &set,
            "http://hub/proxy/pypi",
        )
        .expect("the PEP 503 page");
        assert_eq!(html.content_type, "text/html; charset=utf-8");
    }

    #[test]
    fn a_forge_release_is_one_per_held_tag_with_assets_that_point_home() {
        let set = [
            held("v2.60.0", Some("filename/gh_2.60.0_linux_amd64.tar.gz")),
            held("v2.60.0", Some("filename/gh_2.60.0_macOS_arm64.zip")),
            held("v2.59.0", Some("filename/gh_2.59.0_linux_amd64.tar.gz")),
            // A source archive held by commit is not a release asset.
            held("abc123", Some("tarball/abc123")),
        ];
        let doc = render(
            RegistryKind::Github,
            DocumentKind::Versions,
            "cli/cli",
            &set,
            "http://hub/proxy/gh",
        )
        .unwrap();
        let crate::ports::DocumentBody::Json(v) = doc.body else {
            panic!("json")
        };
        let releases = v.as_array().unwrap();
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0]["tag_name"], "v2.60.0", "newest first");
        assert_eq!(releases[0]["assets"].as_array().unwrap().len(), 2);
        assert_eq!(
            releases[0]["assets"][0]["browser_download_url"],
            "http://hub/proxy/gh/cli/cli/releases/download/v2.60.0/gh_2.60.0_linux_amd64.tar.gz"
        );
        assert!(
            releases[0].get("id").is_none(),
            "nothing the keys do not say"
        );
        assert_eq!(
            releases[0]["created_at"], "2026-09-05T08:00:00Z",
            "a required date, from when the instance received the asset"
        );
        assert!(
            releases[0].get("published_at").is_none(),
            "upstream's date is not invented"
        );
        assert_eq!(releases[0]["draft"], false);
        assert_eq!(
            releases[0]["assets"][0]["url"], releases[0]["assets"][0]["browser_download_url"],
            "mise reads `url`; without an id it is the download by name"
        );
        assert_eq!(releases[0]["assets"][0]["size"], 1234);

        let one = render_release(
            RegistryKind::Github,
            "cli/cli",
            "v2.59.0",
            &set,
            "http://hub/proxy/gh/",
        )
        .unwrap();
        assert_eq!(one.synthesised, Some(1));
        let crate::ports::DocumentBody::Json(v) = one.body else {
            panic!("json")
        };
        assert_eq!(v["tag_name"], "v2.59.0");
        assert!(render_release(RegistryKind::Github, "cli/cli", "v9.9.9", &set, "").is_none());
        assert!(
            render_release(RegistryKind::Gitlab, "cli/cli", "v2.59.0", &set, "").is_none(),
            "a GitHub asset key is not a GitLab link"
        );
    }

    fn with_extra(mut h: HeldVersion, extra: Value) -> HeldVersion {
        h.extra = extra;
        h
    }

    #[test]
    fn the_pypi_html_page_lists_the_held_files_and_escapes_them() {
        let set = [held("1.17.0", Some("six-1.17.0-py2.py3-none-any.whl"))];
        let doc = render(
            RegistryKind::Pypi,
            DocumentKind::Versions,
            "six",
            &set,
            "http://h/p/",
        )
        .unwrap();
        let crate::ports::DocumentBody::Text(html) = doc.body else {
            panic!()
        };
        assert!(html.contains("<h1>Links for six</h1>"));
        assert!(html.contains(&format!(
            "<a href=\"http://h/p/packages/six-1.17.0-py2.py3-none-any.whl#sha256={}\">six-1.17.0-py2.py3-none-any.whl</a>",
            "ab".repeat(32)
        )), "{html}");
        assert!(html.contains("pypi:repository-version"));
    }

    #[test]
    fn a_cargo_index_needs_the_facts_the_import_read_and_lists_nothing_without_them() {
        let facts = json!({ "cargo": { "deps": [{ "name": "libc", "req": "^0.2", "features": [], "optional": false, "default_features": true, "target": null, "kind": "normal" }], "features": { "std": [] }, "links": null } });
        let set = [
            with_extra(held("0.2.5", Some("dl")), facts.clone()),
            held("0.2.4", Some("dl")),
        ];
        let doc = render(
            RegistryKind::Cargo,
            DocumentKind::Versions,
            "unicode-xid",
            &set,
            "",
        )
        .unwrap();
        let crate::ports::DocumentBody::Text(text) = doc.body else {
            panic!()
        };
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            lines.len(),
            1,
            "the version without facts is not listed: {text}"
        );
        assert_eq!(lines[0]["vers"], "0.2.5");
        assert_eq!(lines[0]["deps"][0]["name"], "libc");
        assert_eq!(lines[0]["cksum"], "ab".repeat(32));
        assert_eq!(lines[0]["yanked"], false);
        let none = [held("0.2.4", Some("dl"))];
        assert!(render(
            RegistryKind::Cargo,
            DocumentKind::Versions,
            "unicode-xid",
            &none,
            ""
        )
        .is_none());
    }

    #[test]
    fn go_lists_the_held_zips_and_dates_info_by_receipt() {
        let set = [
            held("v1.5.0", Some("zip")),
            held("v1.5.0", Some("mod")),
            held("v1.6.0", Some("zip")),
            held("v1.4.0", Some("mod")),
        ];
        let m = "github.com/google/uuid";
        let doc = render(RegistryKind::Goproxy, DocumentKind::Versions, m, &set, "").unwrap();
        let crate::ports::DocumentBody::Text(list) = doc.body else {
            panic!()
        };
        assert_eq!(list, "v1.5.0\nv1.6.0\n", "only versions whose zip is held");
        let latest = render(RegistryKind::Goproxy, DocumentKind::LATEST, m, &set, "").unwrap();
        let crate::ports::DocumentBody::Json(v) = latest.body else {
            panic!()
        };
        assert_eq!(v["Version"], "v1.6.0");
        assert_eq!(v["Time"], "2026-09-05T08:00:00Z");
        let info = render_artifact_document(RegistryKind::Goproxy, m, "v1.5.0", &set, "").unwrap();
        let crate::ports::DocumentBody::Json(v) = info.body else {
            panic!()
        };
        assert_eq!(v["Version"], "v1.5.0");
        assert!(render_artifact_document(RegistryKind::Goproxy, m, "v1.4.0", &set, "").is_none());
        assert!(is_document_by_version(
            RegistryKind::Goproxy,
            "v1.5.0",
            None
        ));
        assert!(!is_document_by_version(
            RegistryKind::Goproxy,
            "latest",
            None
        ));
        assert!(!is_document_by_version(
            RegistryKind::Goproxy,
            "v1.5.0",
            Some("zip")
        ));
        assert_eq!(
            document_by_version_key(RegistryKind::Goproxy, m),
            format!("{m} (info)")
        );
    }

    #[test]
    fn maven_metadata_names_every_held_version_and_the_two_pointers() {
        let set = [
            held("3.12.0", Some("commons-lang3-3.12.0.jar")),
            held("3.12.0", Some("commons-lang3-3.12.0.pom")),
            held("3.13.0-SNAPSHOT", Some("commons-lang3-3.13.0-SNAPSHOT.jar")),
            held("3.11", Some("commons-lang3-3.11.pom")),
        ];
        let doc = render(
            RegistryKind::Maven,
            DocumentKind::Versions,
            "org.apache.commons:commons-lang3",
            &set,
            "",
        )
        .unwrap();
        assert_eq!(doc.content_type, "application/xml");
        let crate::ports::DocumentBody::Text(xml) = doc.body else {
            panic!()
        };
        assert!(xml.contains("<groupId>org.apache.commons</groupId>"));
        assert!(xml.contains("<artifactId>commons-lang3</artifactId>"));
        assert!(xml.contains("<latest>3.13.0-SNAPSHOT</latest>"), "{xml}");
        assert!(xml.contains("<release>3.12.0</release>"), "{xml}");
        assert_eq!(xml.matches("<version>").count(), 3, "{xml}");
        assert!(xml.contains("<lastUpdated>20260905080000</lastUpdated>"));
        assert!(render(
            RegistryKind::Maven,
            DocumentKind::Versions,
            "no-colon",
            &set,
            ""
        )
        .is_none());
    }

    #[test]
    fn the_nuget_flat_index_lists_the_versions_whose_package_is_held() {
        let set = [
            held("13.0.3", Some("newtonsoft.json.13.0.3.nupkg")),
            held("13.0.1", Some("newtonsoft.json.13.0.1.nupkg")),
            held("12.0.0", Some("newtonsoft.json.12.0.0.snupkg")),
        ];
        let doc = render(
            RegistryKind::Nuget,
            DocumentKind::Versions,
            "newtonsoft.json",
            &set,
            "",
        )
        .unwrap();
        let crate::ports::DocumentBody::Json(v) = doc.body else {
            panic!()
        };
        assert_eq!(v["versions"], json!(["13.0.1", "13.0.3"]));
    }

    #[test]
    fn gitlab_releases_carry_the_held_links_and_the_by_tag_document_matches() {
        let set = [
            held("v2.0.0", Some("link/tool-linux-amd64")),
            held("v2.0.0", Some("link/tool-darwin-arm64")),
            held("v1.0.0", Some("link/tool-linux-amd64")),
        ];
        let doc = render(
            RegistryKind::Gitlab,
            DocumentKind::Versions,
            "group/tool",
            &set,
            "http://h/gl",
        )
        .unwrap();
        let crate::ports::DocumentBody::Json(v) = doc.body else {
            panic!()
        };
        let releases = v.as_array().unwrap();
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0]["tag_name"], "v2.0.0");
        assert_eq!(releases[0]["assets"]["count"], 2);
        assert_eq!(
            releases[0]["assets"]["links"][0]["url"],
            "http://h/gl/group/tool/-/releases/v2.0.0/downloads/tool-linux-amd64"
        );
        let one = render_release(
            RegistryKind::Gitlab,
            "group/tool",
            "v1.0.0",
            &set,
            "http://h/gl",
        )
        .unwrap();
        assert_eq!(one.synthesised, Some(1));
        assert!(is_document_by_version(RegistryKind::Gitlab, "v1.0.0", None));
        assert!(!is_document_by_version(
            RegistryKind::Gitlab,
            "releases",
            None
        ));
    }

    #[test]
    fn nodedist_index_tab_keeps_the_header_and_derives_platforms_from_file_names() {
        let set = [
            held("v22.11.0", Some("node-v22.11.0-linux-x64.tar.xz")),
            held("v22.11.0", Some("node-v22.11.0-darwin-arm64.tar.gz")),
            held("v22.11.0", Some("SHASUMS256.txt")),
            held("v20.18.0", Some("node-v20.18.0-linux-x64.tar.xz")),
            held("v19.0.0", Some("SHASUMS256.txt")),
        ];
        let doc = render(
            RegistryKind::Nodedist,
            DocumentKind::Versions,
            "node",
            &set,
            "",
        )
        .unwrap();
        let crate::ports::DocumentBody::Text(tab) = doc.body else {
            panic!()
        };
        let lines: Vec<&str> = tab.lines().collect();
        assert!(lines[0].starts_with("version\tdate\tfiles"));
        assert_eq!(
            lines.len(),
            3,
            "a version with only a checksum file is not listed: {tab}"
        );
        assert!(
            lines[1].starts_with("v22.11.0\t2026-09-05\tdarwin-arm64,linux-x64\t"),
            "{}",
            lines[1]
        );
        assert!(lines[1].ends_with("\t-\tfalse"), "{}", lines[1]);
        let json = render(
            RegistryKind::Nodedist,
            DocumentKind::Secondary("index-json"),
            "node",
            &set,
            "",
        )
        .unwrap();
        let crate::ports::DocumentBody::Json(v) = json.body else {
            panic!()
        };
        assert_eq!(v[0]["version"], "v22.11.0");
        assert_eq!(v[0]["files"], json!(["darwin-arm64", "linux-x64"]));
        assert_eq!(v[0]["lts"], false);
    }

    #[test]
    fn sdkman_lists_the_candidate_versions_held_for_the_platform() {
        let set = [
            held("17.0.2-tem", Some("linuxx64")),
            held("21.0.1-tem", Some("linuxx64")),
            held("21.0.1-tem", Some("darwinarm64")),
            held("8.0.1-tem", Some("UNIVERSAL")),
        ];
        let doc = render(
            RegistryKind::Sdkman,
            DocumentKind::Versions,
            "java/linuxx64",
            &set,
            "",
        )
        .unwrap();
        let crate::ports::DocumentBody::Text(csv) = doc.body else {
            panic!()
        };
        assert_eq!(csv, "8.0.1-tem,17.0.2-tem,21.0.1-tem");
        let def = render(
            RegistryKind::Sdkman,
            DocumentKind::Secondary("sdkman-default"),
            "java/linuxx64",
            &set,
            "",
        )
        .unwrap();
        let crate::ports::DocumentBody::Text(v) = def.body else {
            panic!()
        };
        assert_eq!(v, "21.0.1-tem");
        assert!(render(
            RegistryKind::Sdkman,
            DocumentKind::Versions,
            "java/windowsx64",
            &[held("17.0.2-tem", Some("linuxx64"))],
            ""
        )
        .is_none());
        assert_eq!(
            package_names_for(RegistryKind::Sdkman, "java/linuxx64"),
            vec!["java"]
        );
        assert_eq!(
            package_names_for(RegistryKind::Pypi, "typing-extensions"),
            vec!["typing-extensions", "typing_extensions"]
        );
    }

    #[test]
    fn rubygems_compact_info_and_versions_pair_by_md5() {
        let facts = json!({ "rubygems": { "platform": "ruby", "dependencies": [{ "name": "rack", "requirement": ">= 2.0" }] } });
        let mut a = with_extra(held("13.2.1", Some("gem")), facts.clone());
        a.checksum = Some("cd".repeat(32));
        let b = held("13.0.0", Some("gem")); // no facts: not listed
        let set = [a.clone(), b];
        let info = render(
            RegistryKind::Rubygems,
            DocumentKind::Secondary("compact-info"),
            "rake",
            &set,
            "",
        )
        .unwrap();
        let crate::ports::DocumentBody::Text(text) = info.body else {
            panic!()
        };
        assert_eq!(
            text,
            format!("---\n13.2.1 rack:>= 2.0|checksum:{}\n", "cd".repeat(32))
        );
        let registry = vec![
            ("rake".to_owned(), a),
            ("other".to_owned(), held("1.0", Some("gem"))),
        ];
        let versions = render_registry(
            RegistryKind::Rubygems,
            DocumentKind::Secondary("compact-versions"),
            "_versions",
            &registry,
        )
        .unwrap();
        let crate::ports::DocumentBody::Text(v) = versions.body else {
            panic!()
        };
        let line = v.lines().last().unwrap();
        assert!(line.starts_with("rake 13.2.1 "), "{v}");
        let md5 = {
            use md5::Digest as _;
            hex::encode(md5::Md5::digest(text.as_bytes()))
        };
        assert!(line.ends_with(&md5), "the md5 is the info document's: {v}");
        assert!(
            !v.contains("other"),
            "a gem without facts is not listed: {v}"
        );
        assert!(is_registry_wide(
            RegistryKind::Rubygems,
            DocumentKind::Secondary("compact-versions")
        ));
        assert!(!is_registry_wide(
            RegistryKind::Rubygems,
            DocumentKind::Secondary("compact-info")
        ));
        let names = render_registry(
            RegistryKind::Rubygems,
            DocumentKind::Secondary("compact-names"),
            "_names",
            &registry,
        )
        .unwrap();
        let crate::ports::DocumentBody::Text(n) = names.body else {
            panic!()
        };
        assert_eq!(n, "---\nother\nrake\n");
    }

    #[test]
    fn conda_repodata_is_per_subdir_and_empty_for_a_subdir_with_nothing() {
        let facts = json!({ "conda": { "name": "_libgcc_mutex", "version": "0.1", "build": "conda_forge", "build_number": 0, "depends": [], "subdir": "linux-64", "license": "None" } });
        let row = with_extra(
            held(
                "0.1",
                Some("linux-64/_libgcc_mutex-0.1-conda_forge.tar.bz2"),
            ),
            facts,
        );
        let registry = vec![("_libgcc_mutex".to_owned(), row)];
        let doc = render_registry(
            RegistryKind::Conda,
            DocumentKind::Versions,
            "linux-64",
            &registry,
        )
        .unwrap();
        let crate::ports::DocumentBody::Json(v) = doc.body else {
            panic!()
        };
        let entry = &v["packages"]["_libgcc_mutex-0.1-conda_forge.tar.bz2"];
        assert_eq!(entry["name"], "_libgcc_mutex");
        assert_eq!(entry["build"], "conda_forge");
        assert_eq!(entry["subdir"], "linux-64");
        assert_eq!(entry["sha256"], "ab".repeat(32));
        assert_eq!(v["repodata_version"], 1);
        let noarch = render_registry(
            RegistryKind::Conda,
            DocumentKind::Versions,
            "noarch",
            &registry,
        )
        .unwrap();
        let crate::ports::DocumentBody::Json(v) = noarch.body else {
            panic!()
        };
        assert!(
            v["packages"].as_object().unwrap().is_empty(),
            "empty, not absent: {v}"
        );
        assert!(
            render_registry(RegistryKind::Conda, DocumentKind::Versions, "noarch", &[]).is_none()
        );
    }

    #[test]
    fn nuget_registration_and_composer_p2_come_from_the_facts_the_import_filed() {
        let facts = json!({ "nuget": { "id": "Newtonsoft.Json", "version": "13.0.3", "description": "Json.NET", "authors": "James", "tags": null } });
        let set = [with_extra(
            held("13.0.3", Some("newtonsoft.json.13.0.3.nupkg")),
            facts,
        )];
        let doc = render(
            RegistryKind::Nuget,
            DocumentKind::Secondary("registration"),
            "newtonsoft.json",
            &set,
            "http://h/n",
        )
        .unwrap();
        let crate::ports::DocumentBody::Json(v) = doc.body else {
            panic!()
        };
        assert_eq!(v["count"], 1);
        assert_eq!(
            v["items"][0]["items"][0]["catalogEntry"]["id"],
            "Newtonsoft.Json"
        );
        assert_eq!(
            v["items"][0]["items"][0]["packageContent"],
            "http://h/n/nuget/v3/flat/newtonsoft.json/13.0.3/newtonsoft.json.13.0.3.nupkg"
        );
        assert!(render(
            RegistryKind::Nuget,
            DocumentKind::Secondary("registration"),
            "x",
            &[held("1.0", Some("x.1.0.nupkg"))],
            ""
        )
        .is_none());

        let cj = json!({ "composer": { "name": "vendor/pkg", "require": { "php": ">=8.1" }, "autoload": { "psr-4": { "V\\\\": "src/" } } } });
        let set = [with_extra(held("1.2.0", Some("dist")), cj)];
        let doc = render(
            RegistryKind::Composer,
            DocumentKind::Versions,
            "vendor/pkg",
            &set,
            "http://h/c",
        )
        .unwrap();
        let crate::ports::DocumentBody::Json(v) = doc.body else {
            panic!()
        };
        let entry = &v["packages"]["vendor/pkg"][0];
        assert_eq!(entry["version"], "1.2.0");
        assert_eq!(entry["dist"]["url"], "http://h/c/dist/vendor/pkg/1.2.0");
        assert_eq!(entry["require"]["php"], ">=8.1");
        let dev = render(
            RegistryKind::Composer,
            DocumentKind::Secondary("p2-dev"),
            "vendor/pkg",
            &set,
            "http://h/c",
        )
        .unwrap();
        let crate::ports::DocumentBody::Json(v) = dev.body else {
            panic!()
        };
        assert!(
            v["packages"]["vendor/pkg"].as_array().unwrap().is_empty(),
            "{v}"
        );
    }

    #[test]
    fn kinds_without_a_composable_listing_answer_none() {
        let set = [held("1.0", Some("pool/main/x/x_1.0_amd64.deb"))];
        for kind in [RegistryKind::Deb, RegistryKind::Generic, RegistryKind::Rpm] {
            assert!(render(kind, DocumentKind::Versions, "x", &set, "http://h").is_none());
        }
        assert!(render(
            RegistryKind::Npm,
            DocumentKind::Versions,
            "x",
            &[],
            "http://h"
        )
        .is_none());
    }

    #[test]
    fn sri_is_the_base64_of_the_raw_digest() {
        assert_eq!(
            sha256_sri(&"00".repeat(32)).unwrap(),
            "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        );
        assert!(sha256_sri("zz").is_none());
        assert!(sha256_sri("ab").is_none());
    }

    fn held_facts(version: &str, artifact: Option<&str>, extra: Value) -> HeldVersion {
        HeldVersion {
            extra,
            ..held(version, artifact)
        }
    }

    fn provider_facts() -> Value {
        json!({ "terraform": {
            "protocols": ["5.0"],
            "signing_keys": { "gpg_public_keys": [{ "key_id": "34365D9472D7468F", "ascii_armor": "-----BEGIN PGP PUBLIC KEY BLOCK-----" }] },
        } })
    }

    fn shasums_facts(filename: &str) -> Value {
        json!({ "terraform": { "sums": { filename: "ab".repeat(32), "other.zip": "cd".repeat(32) } } })
    }

    #[test]
    fn a_terraform_download_coordinate_lists_under_its_provider() {
        assert_eq!(
            package_names_for(
                RegistryKind::Terraform,
                "providers/hashicorp/null/3.2.2/download/linux/amd64"
            ),
            vec!["providers/hashicorp/null".to_owned()]
        );
        assert_eq!(
            package_names_for(RegistryKind::Terraform, "providers/hashicorp/null"),
            vec!["providers/hashicorp/null".to_owned()]
        );
        assert_eq!(
            package_names_for(RegistryKind::Terraform, "modules/hashicorp/dir/template"),
            vec!["modules/hashicorp/dir/template".to_owned()]
        );
    }

    #[test]
    fn a_held_provider_lists_its_versions_with_platforms_and_protocols() {
        let set = [
            held_facts("3.2.2", Some("linux/amd64"), provider_facts()),
            held("3.2.2", Some("darwin/arm64")),
            held("3.2.2", Some("shasums")),
            held("3.2.2", Some("shasums.sig")),
            held("3.1.0", Some("linux/amd64")),
            // A checksum list alone is not an installable version.
            held("3.0.0", Some("shasums")),
        ];
        let doc = render(
            RegistryKind::Terraform,
            DocumentKind::Versions,
            "providers/hashicorp/null",
            &set,
            "http://h",
        )
        .expect("a provider with archives is listed");
        assert_eq!(doc.synthesised, Some(6));
        let body = doc.body.as_json().unwrap();
        let versions = body["versions"].as_array().unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0]["version"], "3.2.2");
        assert_eq!(versions[0]["protocols"], json!(["5.0"]));
        assert_eq!(
            versions[0]["platforms"],
            json!([{ "os": "darwin", "arch": "arm64" }, { "os": "linux", "arch": "amd64" }])
        );
        assert_eq!(versions[1]["version"], "3.1.0");
        assert_eq!(versions[1]["protocols"], json!([]));
    }

    #[test]
    fn a_held_module_lists_its_tarballs() {
        let set = [held("1.0.2", None), held("1.0.1", None)];
        let doc = render(
            RegistryKind::Terraform,
            DocumentKind::Versions,
            "modules/hashicorp/dir/template",
            &set,
            "http://h",
        )
        .unwrap();
        let body = doc.body.as_json().unwrap();
        assert_eq!(body["modules"][0]["source"], "hashicorp/dir/template");
        assert_eq!(
            body["modules"][0]["versions"],
            json!([{ "version": "1.0.1" }, { "version": "1.0.2" }])
        );
    }

    #[test]
    fn the_download_document_is_composed_from_the_archive_its_list_and_its_signature() {
        let set = [
            held_facts("3.2.2", Some("linux/amd64"), provider_facts()),
            held_facts(
                "3.2.2",
                Some("shasums"),
                shasums_facts("terraform-provider-null_3.2.2_linux_amd64.zip"),
            ),
            held("3.2.2", Some("shasums.sig")),
        ];
        let doc = render(
            RegistryKind::Terraform,
            DocumentKind::PROVIDER_DOWNLOAD,
            "providers/hashicorp/null/3.2.2/download/linux/amd64",
            &set,
            "http://h/proxy/tf",
        )
        .expect("all three held");
        let body = doc.body.as_json().unwrap();
        assert_eq!(body["os"], "linux");
        assert_eq!(body["arch"], "amd64");
        assert_eq!(body["shasum"], "ab".repeat(32));
        assert_eq!(
            body["filename"],
            "terraform-provider-null_3.2.2_linux_amd64.zip"
        );
        assert_eq!(body["protocols"], json!(["5.0"]));
        assert_eq!(
            body["download_url"],
            "http://h/proxy/tf/v1/providers/hashicorp/null/3.2.2/artifact/linux/amd64"
        );
        assert_eq!(
            body["shasums_url"],
            "http://h/proxy/tf/v1/providers/hashicorp/null/3.2.2/shasums"
        );
        assert_eq!(
            body["shasums_signature_url"],
            "http://h/proxy/tf/v1/providers/hashicorp/null/3.2.2/shasums.sig"
        );
        assert_eq!(
            body["signing_keys"]["gpg_public_keys"][0]["key_id"],
            "34365D9472D7468F"
        );
    }

    #[test]
    fn the_download_document_names_the_conventional_file_when_the_list_has_no_facts() {
        let set = [
            held("3.2.2", Some("linux/amd64")),
            held("3.2.2", Some("shasums")),
            held("3.2.2", Some("shasums.sig")),
        ];
        let body = render(
            RegistryKind::Terraform,
            DocumentKind::PROVIDER_DOWNLOAD,
            "providers/hashicorp/null/3.2.2/download/linux/amd64",
            &set,
            "http://h",
        )
        .unwrap();
        let body = body.body.as_json().unwrap();
        assert_eq!(
            body["filename"],
            "terraform-provider-null_3.2.2_linux_amd64.zip"
        );
        assert_eq!(body["signing_keys"], json!({ "gpg_public_keys": [] }));
        assert_eq!(body["protocols"], json!([]));
    }

    #[test]
    fn no_download_document_without_the_list_and_its_signature_or_for_another_platform() {
        let archive_only = [held("3.2.2", Some("linux/amd64"))];
        let no_sig = [
            held("3.2.2", Some("linux/amd64")),
            held("3.2.2", Some("shasums")),
        ];
        let complete = [
            held("3.2.2", Some("linux/amd64")),
            held("3.2.2", Some("shasums")),
            held("3.2.2", Some("shasums.sig")),
        ];
        let ask = |set: &[HeldVersion], name: &str| {
            render(
                RegistryKind::Terraform,
                DocumentKind::PROVIDER_DOWNLOAD,
                name,
                set,
                "http://h",
            )
        };
        let linux = "providers/hashicorp/null/3.2.2/download/linux/amd64";
        assert!(ask(&archive_only, linux).is_none());
        assert!(ask(&no_sig, linux).is_none());
        assert!(ask(&complete, linux).is_some());
        assert!(ask(
            &complete,
            "providers/hashicorp/null/3.2.2/download/darwin/arm64"
        )
        .is_none());
        assert!(ask(
            &complete,
            "providers/hashicorp/null/3.2.1/download/linux/amd64"
        )
        .is_none());
        assert!(ask(&complete, "providers/hashicorp/null").is_none());
    }
}
