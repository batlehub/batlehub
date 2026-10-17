use async_trait::async_trait;
use futures::TryStreamExt;
use serde::Deserialize;

use super::super::http_client::{
    cache_control, fetch_json_document, fetch_text_document, percent_encode, to_registry_error,
};
use super::models::{GemDependency, GemInfo, GemVersion};
use super::{models, CoreError, RubyGemsRegistryClient};
use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    ports::{DocumentKind, FetchedArtifact, RegistryClient, UpstreamPackage, VersionDocument},
};
use models::GemMetadata;

// ── Local publish helpers ─────────────────────────────────────────────────────

/// Parse a `.gem` file (TAR archive containing `metadata.gz`) and extract gem metadata.
///
/// The `.gem` format is a TAR archive with:
/// - `metadata.gz` — gzip-compressed YAML gem specification
/// - `data.tar.gz` — the gem's actual files
#[cfg(feature = "local-registry")]
pub fn parse_gem_bytes(data: &[u8]) -> Result<GemMetadata, CoreError> {
    use std::io::{Cursor, Read};

    // A `.gem` is publisher-supplied, so its embedded `metadata.gz` is untrusted:
    // cap both the compressed read and the decompressed output so a "gzip bomb"
    // (a few KB expanding to many GB) cannot OOM the process on publish.
    const MAX_METADATA_COMPRESSED: u64 = 16 * 1024 * 1024; // 16 MiB
    const MAX_METADATA_DECOMPRESSED: u64 = 64 * 1024 * 1024; // 64 MiB

    let cursor = Cursor::new(data);
    let mut archive = tar::Archive::new(cursor);

    let mut metadata_bytes: Option<Vec<u8>> = None;
    for entry in archive
        .entries()
        .map_err(|e| CoreError::Registry(format!("rubygems: read gem tar: {e}")))?
    {
        let mut entry =
            entry.map_err(|e| CoreError::Registry(format!("rubygems: gem entry: {e}")))?;
        let is_metadata = entry
            .path()
            .map(|p| p.as_os_str() == "metadata.gz")
            .unwrap_or(false);
        if is_metadata {
            let mut buf = Vec::new();
            Read::take(&mut entry, MAX_METADATA_COMPRESSED)
                .read_to_end(&mut buf)
                .map_err(|e| CoreError::Registry(format!("rubygems: read metadata.gz: {e}")))?;
            if buf.len() as u64 >= MAX_METADATA_COMPRESSED {
                return Err(CoreError::Registry(
                    "rubygems: metadata.gz exceeds the maximum allowed size".to_owned(),
                ));
            }
            metadata_bytes = Some(buf);
            break;
        }
    }

    let compressed = metadata_bytes.ok_or_else(|| {
        CoreError::Registry("rubygems: metadata.gz not found in .gem archive".to_owned())
    })?;

    let decoder = flate2::read::GzDecoder::new(compressed.as_slice());
    let mut yaml = String::new();
    Read::take(decoder, MAX_METADATA_DECOMPRESSED)
        .read_to_string(&mut yaml)
        .map_err(|e| CoreError::Registry(format!("rubygems: decompress metadata.gz: {e}")))?;
    if yaml.len() as u64 >= MAX_METADATA_DECOMPRESSED {
        return Err(CoreError::Registry(
            "rubygems: decompressed metadata.gz exceeds the maximum allowed size".to_owned(),
        ));
    }

    parse_gem_yaml(&yaml)
}

fn extract_yaml_value<'a>(yaml: &'a str, key: &str) -> Option<&'a str> {
    for line in yaml.lines() {
        let trimmed = line.trim_start_matches(' ');
        if let Some(rest) = trimmed.strip_prefix(key) {
            let v = rest.trim();
            if v.starts_with('!') {
                return None;
            }
            return Some(strip_yaml_quotes(v));
        }
    }
    None
}

fn strip_yaml_quotes(s: &str) -> &str {
    if s.len() >= 2
        && ((s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Extract the gem version from the nested Gem::Version YAML block:
///   version: !ruby/object:Gem::Version
///     version: '1.0.0'
fn extract_gem_version(yaml: &str) -> Option<String> {
    let mut after_version_key = false;
    for line in yaml.lines() {
        if line.starts_with("version:") {
            after_version_key = true;
            continue;
        }
        if !after_version_key {
            continue;
        }
        let trimmed = line.trim_start_matches(' ');
        if let Some(rest) = trimmed.strip_prefix("version: ") {
            let v = rest.trim();
            if !v.starts_with('!') {
                return Some(strip_yaml_quotes(v).to_owned());
            }
        }
        if !line.starts_with(' ') {
            after_version_key = false;
        }
    }
    None
}

pub(super) fn parse_gem_yaml(yaml: &str) -> Result<GemMetadata, CoreError> {
    let name = extract_yaml_value(yaml, "name: ")
        .ok_or_else(|| CoreError::Registry("rubygems: gem name not found in metadata".to_owned()))?
        .to_owned();

    let version = extract_gem_version(yaml).ok_or_else(|| {
        CoreError::Registry("rubygems: gem version not found in metadata".to_owned())
    })?;

    let platform = extract_yaml_value(yaml, "platform: ")
        .unwrap_or("ruby")
        .to_owned();
    let summary = extract_yaml_value(yaml, "summary: ").map(str::to_owned);

    let mut authors = Vec::new();
    let mut in_authors = false;
    for line in yaml.lines() {
        if line == "authors:" || line.starts_with("authors:") {
            in_authors = true;
            continue;
        }
        if in_authors {
            let trimmed = line.trim_start_matches(' ');
            if let Some(author) = trimmed.strip_prefix("- ") {
                authors.push(strip_yaml_quotes(author.trim()).to_owned());
            } else {
                in_authors = false;
            }
        }
    }

    let meta = models::GemMetadata {
        name,
        version,
        platform,
        summary,
        authors,
        dependencies: parse_gem_dependencies(yaml),
    };
    validate_compact_index_fields(&meta)?;
    Ok(meta)
}

/// RubyGems' own grammar for every field the compact index writes inline.
///
/// `/info/{gem}` is `{version} {dep}:{req}&{req},{dep}:{req}|checksum:{sha}`,
/// one line per version, and Bundler splits it on exactly those delimiters. A
/// field carrying a space, `,`, `:`, `|`, `&` or a newline re-cuts the line: a
/// dependency named `x:>= 0|checksum:<other sha>` injects a checksum, and one
/// named `a,evil` a dependency the gem never declared. The gemspec is
/// publisher-supplied and scanned line-wise, so nothing upstream of here
/// enforces the grammar — `gem build` does, but `gem push` of a hand-made
/// archive does not have to come from it.
///
/// - names: `Gem::Specification::VALID_NAME_PATTERN`, `[A-Za-z0-9._-]+`;
/// - versions: `Gem::Version`'s pattern, reduced to its alphabet — a digit
///   first, then `[0-9A-Za-z.-]`;
/// - constraints: one of `Gem::Requirement::OPS` and a version.
fn validate_compact_index_fields(meta: &GemMetadata) -> Result<(), CoreError> {
    fn bad(what: &str, value: &str) -> CoreError {
        CoreError::InvalidInput(format!(
            "rubygems: {what} '{}' is not valid in a gemspec",
            value.escape_debug()
        ))
    }
    if !is_gem_name(&meta.name) {
        return Err(bad("gem name", &meta.name));
    }
    if !is_gem_version(&meta.version) {
        return Err(bad("version", &meta.version));
    }
    for dep in &meta.dependencies {
        if !is_gem_name(&dep.name) {
            return Err(bad("dependency name", &dep.name));
        }
        for constraint in dep.requirement.split('&').filter(|c| !c.is_empty()) {
            let ok = constraint.split_once(' ').is_some_and(|(op, v)| {
                matches!(op, "=" | "!=" | ">" | "<" | ">=" | "<=" | "~>") && is_gem_version(v)
            });
            if !ok {
                return Err(bad("dependency requirement", constraint));
            }
        }
    }
    Ok(())
}

fn is_gem_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn is_gem_version(s: &str) -> bool {
    s.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
}

/// Extract runtime dependencies from a gemspec's YAML.
///
/// The block looks like:
///
/// ```yaml
/// dependencies:
/// - !ruby/object:Gem::Dependency
///   name: rake
///   requirement: !ruby/object:Gem::Requirement
///     requirements:
///     - - "~>"
///       - !ruby/object:Gem::Version
///         version: '13.0'
///   type: :runtime
/// ```
///
/// Scanned line-wise like the rest of this parser rather than deserialised: the
/// document is full of Ruby object tags that a general YAML reader has to be
/// taught to ignore, and only four fields are wanted.
///
/// Development dependencies are skipped — they are not part of what an
/// installer resolves, and the compact index does not carry them.
fn parse_gem_dependencies(yaml: &str) -> Vec<GemDependency> {
    let mut acc = DependencyAccumulator::new();
    let mut in_block = false;

    for line in yaml.lines() {
        if !line.starts_with(' ') && !line.starts_with('-') {
            // A new top-level key ends the block — and starts it again when the
            // key is `dependencies:` itself.
            if in_block {
                acc.flush();
            }
            in_block = line.starts_with("dependencies:");
            continue;
        }
        if in_block {
            acc.absorb(line.trim());
        }
    }
    acc.finish()
}

/// The dependency [`parse_gem_dependencies`] is currently assembling.
///
/// One `!ruby/object:Gem::Dependency` entry arrives as several lines, and none
/// of them is complete on its own: the name, the operators and the versions
/// each land separately, and only the *next* entry (or the end of the block)
/// says the previous one is finished. This holds that partial state so the scan
/// itself stays a flat dispatch on the line's prefix.
struct DependencyAccumulator {
    name: Option<String>,
    /// Operators from `- - "~>"` lines, awaiting the version that closes them.
    ops: Vec<String>,
    constraints: Vec<String>,
    runtime: bool,
    deps: Vec<GemDependency>,
}

impl DependencyAccumulator {
    fn new() -> Self {
        Self {
            name: None,
            ops: Vec::new(),
            constraints: Vec::new(),
            // Absent `type:`, a Gem::Dependency is a runtime one.
            runtime: true,
            deps: Vec::new(),
        }
    }

    /// One line of the `dependencies:` block, already trimmed.
    fn absorb(&mut self, trimmed: &str) {
        if trimmed.starts_with("- !ruby/object:Gem::Dependency") {
            self.flush();
        } else if let Some(rest) = trimmed.strip_prefix("name: ") {
            // Only the first `name:` after a Dependency header is the gem's;
            // nested ones do not occur, but taking the first is what makes that
            // independent of order.
            if self.name.is_none() {
                self.name = Some(strip_yaml_quotes(rest.trim()).to_owned());
            }
        } else if let Some(rest) = trimmed.strip_prefix("type: ") {
            self.runtime = rest.trim() != ":development";
        } else if let Some(rest) = trimmed.strip_prefix("- - ") {
            // `- - "~>"` opens a constraint pair; `- '13.0'` and
            // `version: '13.0'` close it.
            self.ops.push(strip_yaml_quotes(rest.trim()).to_owned());
        } else if let Some(rest) = trimmed.strip_prefix("version: ") {
            self.close_constraint(rest.trim());
        }
    }

    /// Close the open operator with `value`, if `value` is a version at all.
    fn close_constraint(&mut self, value: &str) {
        // `!ruby/object:Gem::Version` — the tag, not the version under it.
        if value.starts_with('!') {
            return;
        }
        let v = strip_yaml_quotes(value);
        let op = self.ops.pop().unwrap_or_else(|| ">=".to_owned());
        // `>= 0` is how "no constraint" is written, and the compact index
        // leaves it out.
        if !(op == ">=" && v == "0") {
            self.constraints.push(format!("{op} {v}"));
        }
    }

    /// Emit the entry being assembled, if there is one, and reset.
    fn flush(&mut self) {
        if let Some(name) = self.name.take() {
            if self.runtime {
                self.deps.push(GemDependency {
                    name,
                    requirement: self.constraints.join("&"),
                });
            }
        }
        self.constraints.clear();
        self.ops.clear();
        self.runtime = true;
    }

    fn finish(mut self) -> Vec<GemDependency> {
        self.flush();
        self.deps
    }
}

/// Split a gem filename stem (without `.gem`) into `(name, version)`.
///
/// Gem filenames follow `{name}-{version}` where version starts with a digit.
/// For multi-hyphen gem names like `json-jwt`, the split point is at the first
/// `-` that is immediately followed by a digit.
pub fn split_gem_stem(stem: &str) -> Option<(&str, &str)> {
    let bytes = stem.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            return Some((&stem[..i], &stem[i + 1..]));
        }
    }
    None
}

// ── RegistryClient impl ───────────────────────────────────────────────────────

#[async_trait]
impl RegistryClient for RubyGemsRegistryClient {
    fn registry_type(&self) -> &str {
        "rubygems"
    }

    /// Two documents, both read by `bundler`/`gem` on different paths: the
    /// versions API resolves a constraint, the gem document answers "what is
    /// the current release". Both have to hide a blocked version.
    ///
    /// The Marshal indexes are deliberately absent — see
    /// `RegistryKind::listing_filter`.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        let base = self.base_url.trim_end_matches('/');
        let name = percent_encode(package);
        let (url, what) = match kind {
            DocumentKind::Versions => (
                format!("{base}/api/v1/versions/{name}.json"),
                format!("rubygems versions for '{package}'"),
            ),
            DocumentKind::GEM => (
                format!("{base}/api/v1/gems/{name}.json"),
                format!("rubygems gem document for '{package}'"),
            ),
            // The compact index — what Bundler actually resolves from. Plain
            // text, so `DocumentBody::Text` carries it and no Marshal encoder
            // is involved (RFC 0009 §7.3).
            DocumentKind::COMPACT_VERSIONS => {
                return fetch_text_document(
                    self.get(&format!("{base}/versions")),
                    "rubygems compact index /versions",
                    "text/plain; charset=utf-8",
                )
                .await
            }
            DocumentKind::COMPACT_NAMES => {
                return fetch_text_document(
                    self.get(&format!("{base}/names")),
                    "rubygems compact index /names",
                    "text/plain; charset=utf-8",
                )
                .await
            }
            DocumentKind::COMPACT_INFO => {
                return fetch_text_document(
                    self.get(&format!("{base}/info/{name}")),
                    &format!("rubygems compact info for '{package}'"),
                    "text/plain; charset=utf-8",
                )
                .await
            }
            other => {
                return Err(CoreError::NotSupported(format!(
                    "rubygems has no '{other}' listing document"
                )))
            }
        };
        fetch_json_document(self.get(&url), &what).await
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let url = self.artifact_url(pkg)?;

        let resp =
            self.get(&url).send().await.map_err(|e| {
                CoreError::Registry(format!("rubygems metadata request failed: {e}"))
            })?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "rubygems resource not found: {}",
                pkg.cache_key()
            )));
        }
        if !resp.status().is_success() {
            return Err(CoreError::Registry(format!(
                "rubygems upstream returned {} for {}",
                resp.status(),
                pkg.cache_key()
            )));
        }

        let cache_control = cache_control(&resp);

        // Parse published_at and checksum from the gem info JSON when available.
        if pkg.artifact.is_none() && pkg.name != "_index" {
            let body = resp.bytes().await.map_err(to_registry_error)?;
            if let Ok(info) = serde_json::from_slice::<GemInfo>(&body) {
                let published_at = info.created_at.as_deref().and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                });
                return Ok(PackageMetadata {
                    id: pkg.clone(),
                    published_at,
                    download_url: Some(url),
                    checksum: info.sha,
                    is_signed: None,
                    extra: serde_json::json!({ "version": info.version }),
                    cache_control,
                });
            }
        }

        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: None,
            download_url: Some(url),
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let url = self.artifact_url(pkg)?;

        tracing::debug!(url = %url, "fetching RubyGems artifact");

        let response = self.get(&url).send().await.map_err(to_registry_error)?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "rubygems artifact not found: {}",
                pkg.cache_key()
            )));
        }
        if !response.status().is_success() {
            return Err(CoreError::Registry(format!(
                "rubygems upstream returned {} for {}",
                response.status(),
                pkg.cache_key()
            )));
        }

        let cache_control = cache_control(&response);

        let stream = response.bytes_stream().map_err(to_registry_error);

        Ok(FetchedArtifact {
            stream: Box::pin(stream),
            cache_control,
        })
    }

    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/api/v1/versions/{package}.json");

        let resp = self.get(&url).send().await.map_err(to_registry_error)?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }

        let body = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .bytes()
            .await
            .map_err(to_registry_error)?;

        let versions: Vec<GemVersion> = serde_json::from_slice(&body)
            .map_err(|e| CoreError::Registry(format!("rubygems: parse versions: {e}")))?;

        // rubygems API returns newest-first; reverse to oldest-first.
        let mut result: Vec<String> = versions.into_iter().map(|v| v.number).collect();
        result.reverse();
        Ok(result)
    }

    async fn search_packages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<UpstreamPackage>, CoreError> {
        #[derive(Deserialize)]
        struct GemSearchResult {
            name: String,
            version: String,
            info: Option<String>,
        }

        let url = format!(
            "{}/api/v1/search.json?query={}&page=1",
            self.base_url,
            percent_encode(query),
        );
        let res = self.get(&url).send().await.map_err(to_registry_error)?;

        if !res.status().is_success() {
            return Ok(vec![]);
        }

        let gems: Vec<GemSearchResult> = res.json().await.map_err(to_registry_error)?;

        Ok(gems
            .into_iter()
            .take(limit)
            .map(|g| UpstreamPackage {
                name: g.name,
                latest_version: g.version,
                description: g.info,
            })
            .collect())
    }
}
