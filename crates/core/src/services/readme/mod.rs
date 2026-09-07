//! Storing and serving each version's own README (RFC 0007).
//!
//! Everything that *writes* here is driven by resolution, publication or the
//! single introspection pass over a cached artifact. **Nothing on this path runs
//! on a package-manager request in a way that fetches anything new, and nothing
//! on it is triggered by a page view** — a page view for a version we hold reads
//! one row and, on a miss in the render cache, runs the renderer.

use std::sync::Arc;

use chrono::Utc;

use crate::entities::{
    readme_digest, MetadataReadme, PackageMetadata, PackageReadme, ReadmeFormat, ReadmeSource,
};
use crate::error::CoreError;
use crate::ports::{CacheEntry, CacheStore, ReadmeRepository};
use crate::services::hot_config::ReadmeConfig;
use crate::services::svg;

pub mod detect;
pub mod image;
pub mod render;
pub mod sanitize;

/// Storing and serving the README of a version.
pub struct ReadmeService {
    pub repo: Arc<dyn ReadmeRepository>,
    /// The render cache, keyed by content digest and renderer version.
    ///
    /// Optional because the write paths — capture on resolve, on publish, on the
    /// introspection pass — do not render anything, and a test that only
    /// exercises those should not have to build a cache. A `None` here costs a
    /// render per read, not a wrong answer.
    pub cache: Option<Arc<dyn CacheStore>>,
    /// Fetches a README's images, for `remote_images = "proxy"`.
    ///
    /// `None` means images are never fetched, which is what every instance on
    /// the default policy wants and what a test that is not about images wants.
    /// The endpoint answers `404` and the panel shows the chip — the same answer
    /// as an image that could not be got, which is the point (RFC 0007-bis §4.2).
    pub image_fetcher: Option<Arc<dyn crate::ports::ReadmeImageFetcher>>,
}

/// One README, and whether it is the version the caller asked about.
#[derive(Debug, Clone)]
pub struct ReadmeAnswer {
    pub readme: PackageReadme,
    /// The stored README belongs to a different version than the one requested.
    /// The panel says so in words rather than showing prose that belongs to
    /// different code (RFC 0007 §4.2).
    pub is_fallback: bool,
}

/// What a record attempt did, so the caller can log the interesting cases
/// without the service deciding how loud they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    /// A row was written or replaced.
    Stored,
    /// The stored text is byte-identical, so nothing was written.
    ///
    /// This is the common case on a re-resolve: metadata TTLs expire far more
    /// often than upstreams edit a published README, and rewriting an identical
    /// row would move `extracted_at` and make the page claim a re-read that
    /// changed nothing.
    Unchanged,
    /// There was nothing to record — the document carried no README, or it was
    /// empty once trimmed.
    Nothing,
}

/// The text with everything the store needs to describe it.
///
/// One struct rather than five parameters because the three `record_*` entry
/// points differ only in where they got it, and a positional `(String,
/// ReadmeFormat, bool)` at three call sites is how a `truncated` flag ends up
/// in the `package_level` slot.
pub struct ReadmeCapture {
    pub content: String,
    pub format: ReadmeFormat,
    pub source: ReadmeSource,
    pub package_level: bool,
}

impl ReadmeService {
    pub fn new(repo: Arc<dyn ReadmeRepository>) -> Self {
        Self {
            repo,
            cache: None,
            image_fetcher: None,
        }
    }

    /// The same service with a render cache attached.
    pub fn with_cache(mut self, cache: Arc<dyn CacheStore>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// The same service able to fetch a README's images.
    pub fn with_image_fetcher(
        mut self,
        fetcher: Arc<dyn crate::ports::ReadmeImageFetcher>,
    ) -> Self {
        self.image_fetcher = Some(fetcher);
        self
    }

    /// The README to show for `version`, applying the fallback rule.
    ///
    /// An exact hit answers for itself. When the requested version has none, the
    /// newest version that does answers instead, flagged — because a package
    /// whose 2.0.0-rc1 ships no README is better served by 1.4.2's than by an
    /// empty panel, as long as the page says which it is showing.
    ///
    /// `ineligible` is the caller's list of versions that may not be a fallback
    /// source: a blocked version serves no README at all, and an unlisted one is
    /// hidden from listings, so neither may be substituted in silently. The
    /// repository knows nothing about firewall state, so the decision stays with
    /// the layer that does (RFC 0007 §4.4).
    pub async fn get_for_version(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        ineligible: &[String],
    ) -> Result<Option<ReadmeAnswer>, CoreError> {
        if let Some(exact) = self.repo.get(registry, name, version).await? {
            return Ok(Some(ReadmeAnswer {
                readme: exact,
                is_fallback: false,
            }));
        }
        // The requested version is itself ineligible only in the sense that it
        // has nothing to give; the exclusion list is about *substitution*, and
        // asking for a version's own README is never a substitution.
        let mut excluded = ineligible.to_vec();
        excluded.push(version.to_owned());
        Ok(self
            .repo
            .get_latest_with_readme(registry, name, &excluded)
            .await?
            .map(|readme| ReadmeAnswer {
                readme,
                is_fallback: true,
            }))
    }

    /// Render `readme` to sanitised HTML, through the cache.
    ///
    /// Keyed by the content digest and the renderer version, so two versions
    /// with an identical README — the common case for a patch release — render
    /// once, and a fix to the sanitiser invalidates every rendering by bumping
    /// [`sanitize::RENDERER_VERSION`] rather than by a backfill.
    ///
    /// A cache failure is not an error: the renderer is a pure function and
    /// running it again is the correct answer, just slower.
    pub async fn render_cached(
        &self,
        readme: &PackageReadme,
        opts: &render::RenderOptions,
    ) -> String {
        let key = render_cache_key(&readme.digest, readme.format, opts);
        if let Some(cache) = &self.cache {
            if let Ok(Some(entry)) = cache.get(&key).await {
                if let Some(html) = entry
                    .metadata
                    .extra
                    .get("readme_html")
                    .and_then(|v| v.as_str())
                {
                    return html.to_owned();
                }
            }
        }
        let html = render::render(&readme.content, readme.format, opts);
        if let Some(cache) = &self.cache {
            let entry = CacheEntry {
                metadata: crate::entities::PackageMetadata {
                    // The coordinate is not part of the key — the cache is
                    // content-addressed — but a `PackageMetadata` needs one, and
                    // recording the version this rendering happened to come from
                    // is more useful to somebody reading the cache than a blank.
                    id: crate::entities::PackageId::new(
                        &readme.registry,
                        &readme.name,
                        &readme.version,
                    ),
                    published_at: None,
                    download_url: None,
                    checksum: None,
                    is_signed: None,
                    extra: serde_json::json!({ "readme_html": html }),
                    cache_control: None,
                },
                cached_at: Utc::now(),
                expires_at: None,
            };
            // No TTL: the entry is keyed by content digest and renderer version,
            // so it can only ever be right. It goes when the cache evicts it.
            if let Err(e) = cache.set(&key, entry, None).await {
                tracing::debug!(error = %e, "readme: render cache write failed (non-fatal)");
            }
        }
        html
    }

    /// The *n*th image of `readme`, fetched by this server and cached.
    ///
    /// The index is resolved with [`render::image_urls`] — the renderer's own
    /// pass over the same bytes — so the `n` a browser asks for is the `n` the
    /// rendering emitted. **The caller supplies no URL at all**, which is what
    /// stops this being an open image proxy (RFC 0007-bis §5.1).
    ///
    /// `None` for every ordinary miss: no such index, no fetcher configured, the
    /// upstream refused, the type is not an image, it is over the cap, or it is
    /// an SVG the sanitiser would not vouch for. The endpoint turns all of them
    /// into a `404` and the panel falls back to the chip `strip` would have
    /// shown, which is a better answer than a broken-image icon.
    ///
    /// A miss is remembered for `negative_ttl`. 3.3 % of the image URLs in real
    /// READMEs are dead (RFC 0007-bis §13.2), and without this each one is
    /// re-fetched on every render-cache miss for as long as the README exists.
    pub async fn image_at(
        &self,
        readme: &PackageReadme,
        index: usize,
        cfg: &ReadmeImageConfig,
    ) -> Option<image::FetchedImage> {
        let fetcher = self.image_fetcher.as_ref()?;
        let urls = self.image_urls_cached(readme, &cfg.allowed_hosts).await;
        let url = urls.get(index)?;

        // Belt and braces: the walk above already excludes a disallowed host, so
        // this can only fire if the two ever disagree. It is one comparison, and
        // the thing it guards is this server making a request to a host an
        // operator has said no to.
        if !image::image_host_allowed(url, &cfg.allowed_hosts) {
            return None;
        }

        let key = image_cache_key(url);
        if let Some(cache) = &self.cache {
            if let Ok(Some(entry)) = cache.get(&key).await {
                return decode_cached_image(&entry.metadata.extra);
            }
        }

        let fetched = match fetcher.fetch(url, &cfg.allowed_hosts, cfg.max_bytes).await {
            Ok(Some(fetched)) => vouch_for(fetched, url),
            Ok(None) => None,
            Err(e) => {
                // An outage is remembered as briefly as a `404`: the negative
                // TTL is short, and the alternative is re-dialling a host that
                // is down on every page view.
                tracing::debug!(url, error = %e, "readme image: fetch failed");
                None
            }
        };

        if let Some(cache) = &self.cache {
            let (payload, ttl) = match &fetched {
                Some(image) => (encode_cached_image(image), cfg.ttl),
                None => (encode_missing_image(), cfg.negative_ttl),
            };
            let entry = CacheEntry {
                metadata: PackageMetadata {
                    id: crate::entities::PackageId::new(
                        &readme.registry,
                        &readme.name,
                        &readme.version,
                    ),
                    published_at: None,
                    download_url: Some(url.clone()),
                    checksum: None,
                    is_signed: None,
                    extra: payload,
                    cache_control: None,
                },
                cached_at: Utc::now(),
                expires_at: None,
            };
            if let Err(e) = cache.set(&key, entry, Some(ttl)).await {
                tracing::debug!(error = %e, "readme image: cache write failed (non-fatal)");
            }
        }
        fetched
    }

    /// The image URLs of `readme`, in the order the rendering emits them, from
    /// the cache where there is one.
    ///
    /// [`render::image_urls`] is a **full pipeline run** — parse, chip pass,
    /// complete sanitise — over the whole stored document, and it was being run
    /// once per image request, ahead of the per-image cache. Both the number of
    /// images on a page and the size of the document are chosen by whoever
    /// published the package: a 256 KiB README (the `max_bytes` default) holding
    /// a few thousand `![](…)` costs tens of milliseconds a call, so one reader
    /// opening that panel cost minutes of CPU, and a warm image cache changed
    /// nothing because the render happened first.
    ///
    /// Content-addressed exactly as [`render_cached`](Self::render_cached) is —
    /// digest, renderer version and the host list, which is the only option that
    /// changes the answer — so it needs no TTL and a sanitiser fix invalidates it
    /// by bumping [`sanitize::RENDERER_VERSION`].
    async fn image_urls_cached(
        &self,
        readme: &PackageReadme,
        allowed_hosts: &[String],
    ) -> Vec<String> {
        let key = image_urls_cache_key(&readme.digest, readme.format, allowed_hosts);
        if let Some(cache) = &self.cache {
            if let Ok(Some(entry)) = cache.get(&key).await {
                if let Some(urls) = entry
                    .metadata
                    .extra
                    .get("readme_image_urls")
                    .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                {
                    return urls;
                }
            }
        }
        let urls = render::image_urls(&readme.content, readme.format, allowed_hosts);
        if let Some(cache) = &self.cache {
            let entry = CacheEntry {
                metadata: PackageMetadata {
                    id: crate::entities::PackageId::new(
                        &readme.registry,
                        &readme.name,
                        &readme.version,
                    ),
                    published_at: None,
                    download_url: None,
                    checksum: None,
                    is_signed: None,
                    extra: serde_json::json!({ "readme_image_urls": urls }),
                    cache_control: None,
                },
                cached_at: Utc::now(),
                expires_at: None,
            };
            if let Err(e) = cache.set(&key, entry, None).await {
                tracing::debug!(error = %e, "readme: image-url cache write failed (non-fatal)");
            }
        }
        urls
    }

    /// Record the README a registry client parsed out of a metadata document.
    ///
    /// Reads [`MetadataReadme`] off `PackageMetadata::extra`, so a registry
    /// type that carries no README costs one absent map lookup. A *linked*
    /// README (a URL, not text) is not followed here — that is an outbound
    /// request and belongs in the detached task, not on the resolve path.
    pub async fn record_from_metadata(
        &self,
        meta: &PackageMetadata,
        cfg: &ReadmeConfig,
    ) -> Result<RecordOutcome, CoreError> {
        let Some(found) = MetadataReadme::from_extra(&meta.extra) else {
            return Ok(RecordOutcome::Nothing);
        };
        let Some(content) = found.content else {
            return Ok(RecordOutcome::Nothing);
        };
        self.record(
            &meta.id.registry,
            &meta.id.name,
            &meta.id.version,
            ReadmeCapture {
                content,
                format: found.format,
                source: ReadmeSource::UpstreamMetadata,
                package_level: found.package_level,
            },
            cfg,
        )
        .await
    }

    /// Record the README read from a URL the metadata document linked to.
    ///
    /// [`ReadmeSource::UpstreamMetadata`], not `Archive`: the text came from the
    /// upstream's own answer about this version, and an operator asking where a
    /// document came from wants "the upstream said so", not "we opened the
    /// bytes". The link is followed by the caller, which is the layer with the
    /// HTTP client and the same-origin guard (RFC 0007 §7.4).
    pub async fn record_from_linked(
        &self,
        id: &crate::entities::PackageId,
        content: String,
        format: ReadmeFormat,
        cfg: &ReadmeConfig,
    ) -> Result<RecordOutcome, CoreError> {
        self.record(
            &id.registry,
            &id.name,
            &id.version,
            ReadmeCapture {
                content,
                format,
                source: ReadmeSource::UpstreamMetadata,
                package_level: false,
            },
            cfg,
        )
        .await
    }

    /// Record the README a publish request carried in its own metadata.
    ///
    /// `LocalPublish` rather than `UpstreamMetadata` because the difference is
    /// visible to an operator asking where a document came from, and on a hybrid
    /// registry both are possible for the same package name.
    pub async fn record_from_publish(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        content: String,
        format: ReadmeFormat,
        cfg: &ReadmeConfig,
    ) -> Result<RecordOutcome, CoreError> {
        self.record(
            registry,
            name,
            version,
            ReadmeCapture {
                content,
                format,
                source: ReadmeSource::LocalPublish,
                package_level: false,
            },
            cfg,
        )
        .await
    }

    /// Record a README read out of the artifact on the single introspection
    /// pass (RFC 0007 §5.2).
    pub async fn record_from_archive(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        content: String,
        format: ReadmeFormat,
        cfg: &ReadmeConfig,
    ) -> Result<RecordOutcome, CoreError> {
        self.record(
            registry,
            name,
            version,
            ReadmeCapture {
                content,
                format,
                source: ReadmeSource::Archive,
                package_level: false,
            },
            cfg,
        )
        .await
    }

    /// The one writer. Trims, caps, and refuses to rewrite identical text.
    async fn record(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        capture: ReadmeCapture,
        cfg: &ReadmeConfig,
    ) -> Result<RecordOutcome, CoreError> {
        if !cfg.enabled {
            return Ok(RecordOutcome::Nothing);
        }
        // Whitespace-only is *nothing*, not a document. An empty panel and a
        // panel showing three blank lines look identical to a reader, and only
        // one of them is honest about there being no README.
        let trimmed = capture.content.trim();
        if trimmed.is_empty() {
            return Ok(RecordOutcome::Nothing);
        }
        let (content, truncated) = truncate_to(trimmed, cfg.max_bytes);
        let digest = readme_digest(&content);

        // An unchanged digest means the upstream did not edit anything, which is
        // the normal outcome of a TTL expiry. Rewriting the row would move
        // `extracted_at` and make the page report a re-read that changed nothing.
        if let Some(existing) = self.repo.get(registry, name, version).await? {
            if existing.digest == digest {
                return Ok(RecordOutcome::Unchanged);
            }
        }

        self.repo
            .upsert(PackageReadme {
                registry: registry.to_owned(),
                name: name.to_owned(),
                version: version.to_owned(),
                content,
                format: capture.format,
                source: capture.source,
                digest,
                truncated,
                package_level: capture.package_level,
                extracted_at: Utc::now(),
            })
            .await?;
        Ok(RecordOutcome::Stored)
    }
}

/// What the image path needs from configuration, per registry.
///
/// The TTLs are the registry's own rather than a second set of numbers: the
/// bytes ride the metadata cache, for the reason RFC 0007 §4.1 gives about
/// `upstream_detail` — a second, independently clocked expiry for bytes that
/// already have one is how two caches come to disagree.
#[derive(Debug, Clone)]
pub struct ReadmeImageConfig {
    /// Per image, and separate from [`ReadmeConfig::max_bytes`], which caps a
    /// stored README's *text*. A 256 KiB text cap and a 2 MiB image cap are not
    /// the same number for the same reason, and sharing one would make raising
    /// either a decision about the other (RFC 0007-bis §4.1).
    pub max_bytes: usize,
    pub ttl: std::time::Duration,
    pub negative_ttl: std::time::Duration,
    /// The registry's `remote_image_hosts`. Empty means every host.
    ///
    /// Passed to the *fetch* side as well as the render side on purpose. The
    /// rendering decided what the page shows; this decides what this server
    /// dials, and the two are separated in time by a cache — an operator who
    /// narrows the list wants the narrowing to take effect on the next request,
    /// not on the next render-cache miss.
    pub allowed_hosts: Vec<String>,
}

/// The cache key for one image URL.
///
/// Keyed by the **URL** rather than by the coordinate, so the shields.io badge
/// that appears in a thousand READMEs is one entry rather than a thousand. The
/// pipeline version is in the key for the same reason `RENDERER_VERSION` is in
/// the render cache's: an SVG stored under an older sanitiser must not be served
/// after that sanitiser is fixed.
fn image_cache_key(url: &str) -> String {
    format!(
        "readme-image:{IMAGE_PIPELINE_VERSION}:{}",
        readme_digest(url)
    )
}

/// Bump when [`svg::sanitize_svg`] or the content-type allow-list changes.
///
/// `2`: quick-xml 0.42 validates the encoding, so a document with invalid UTF-8
/// is now refused whole ([`svg::SvgRejected::Malformed`]) where `1` dropped the
/// affected text node and served the rest. Entries stored under `1` were
/// sanitised by the looser rule and must not be served under the stricter one.
pub const IMAGE_PIPELINE_VERSION: u32 = 2;

/// Everything an image has to survive before it is stored or served.
///
/// An SVG goes through the allow-list, and a document the sanitiser refuses is
/// not served at all: `remote_images = "proxy"` is not an undertaking to render
/// whatever a badge host returns. Every other type is opaque bytes with a
/// checked `Content-Type`, and there is nothing to vouch for beyond that.
fn vouch_for(fetched: image::FetchedImage, url: &str) -> Option<image::FetchedImage> {
    if !fetched.is_svg() {
        return Some(fetched);
    }
    match svg::sanitize_svg(&fetched.bytes) {
        Ok(bytes) => Some(image::FetchedImage { bytes, ..fetched }),
        Err(e) => {
            tracing::debug!(url, reason = %e, "readme image: SVG refused by the sanitiser");
            None
        }
    }
}

fn encode_cached_image(image: &image::FetchedImage) -> serde_json::Value {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    serde_json::json!({
        "readme_image": {
            "content_type": image.content_type,
            "body_b64": STANDARD.encode(&image.bytes),
        }
    })
}

/// A remembered miss.
///
/// Distinct from an absent entry so a cache hit can mean "asked, and there is
/// nothing there" — the whole point of the negative cache.
fn encode_missing_image() -> serde_json::Value {
    serde_json::json!({ "readme_image": null })
}

fn decode_cached_image(value: &serde_json::Value) -> Option<image::FetchedImage> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let entry = value.get("readme_image")?;
    let content_type = image::image_content_type(entry.get("content_type")?.as_str()?)?;
    Some(image::FetchedImage {
        content_type,
        bytes: STANDARD.decode(entry.get("body_b64")?.as_str()?).ok()?,
    })
}

/// The render cache key for one document under one set of options.
///
/// Content-addressed, so nothing about *which package* is in it: two versions
/// with the same README are one entry, and so are two packages that copied one.
/// The renderer version and the options are in the key because both change the
/// output — a rendering made under `remote_images = "strip"` must never be
/// served to a registry configured to proxy them.
///
/// `image_hosts` is in the key for the same reason and is easy to leave out of
/// it: it decides, per image, chip or `<img>`, so two renderings of one document
/// under two lists are two different documents. Without it, widening
/// `remote_image_hosts` would serve the pre-widening rendering — and these
/// entries are written with **no TTL** (they are content-addressed and were
/// therefore taken to be timeless), so "until the cache evicts it" means until
/// something else needs the room. An operator who adds a host and sees no change
/// has no way to tell that from the setting not working.
fn render_cache_key(digest: &str, format: ReadmeFormat, opts: &render::RenderOptions) -> String {
    let variant = readme_digest(&format!(
        "{}|{}|{}|{}",
        format.as_str(),
        opts.remote_images.as_str(),
        opts.image_proxy_prefix.as_deref().unwrap_or(""),
        // Joined rather than hashed piecewise: `\u{1f}` is a unit separator and
        // cannot occur in a hostname, so no two lists can collide by juggling
        // where one entry ends and the next begins.
        opts.image_hosts.join("\u{1f}")
    ));
    format!(
        "readme-html:{}:{}:{digest}",
        sanitize::RENDERER_VERSION,
        &variant[..16]
    )
}

/// The key for [`ReadmeService::image_urls_cached`].
///
/// The same content-addressed shape as [`render_cache_key`], over the only two
/// inputs `image_urls` reads besides the document: the format, and the host
/// allow-list that decides which `<img>` survive the chip pass.
fn image_urls_cache_key(digest: &str, format: ReadmeFormat, allowed_hosts: &[String]) -> String {
    let variant = readme_digest(&format!(
        "{}|{}",
        format.as_str(),
        allowed_hosts.join("\u{1f}")
    ));
    format!(
        "readme-images:{}:{}:{digest}",
        sanitize::RENDERER_VERSION,
        &variant[..16]
    )
}

/// Cut `text` to at most `max_bytes` **bytes**, on a character boundary.
///
/// Bytes rather than characters because the cap exists to bound a database row
/// and the memory a render holds, and both are measured in bytes. Cutting on a
/// boundary matters: a `String` cannot hold half a code point, and a cap that
/// panicked on a README with an emoji in the wrong place would be a crash
/// triggered by publishing.
pub fn truncate_to(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

#[cfg(test)]
mod tests;
