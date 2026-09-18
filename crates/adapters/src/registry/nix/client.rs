//! The upstream half of a Nix binary cache (RFC 0028 §6.4).

use async_trait::async_trait;
use futures::TryStreamExt;

use batlehub_core::services::nix::{self, NarInfo};
use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, VersionDocument},
};

use crate::registry::http_client::{
    basic_auth_get, cache_control, ensure_url_under_base, new_http_client, to_registry_error,
    UpstreamHttpOptions,
};

/// `cache.nixos.org`'s own content types, which a client validates nothing
/// about but a person reading `curl -I` does.
const CT_NARINFO: &str = "text/x-nix-narinfo";
const CT_CACHE_INFO: &str = "text/x-nix-cache-info";
const CT_NAR: &str = "application/x-nix-nar";

/// A client for one Nix binary cache.
///
/// The simplest adapter in this tree, because the protocol is: there is no
/// index, no search and no version list. Every read is `GET {base}/{path}` and
/// the only parsing is the narinfo's `Name: value` grammar.
pub struct NixBinaryCacheClient {
    http: reqwest::Client,
    /// The cache root — the URL a client would put in `substituters`. Never
    /// carries a query string; `validate_registry_nix` refuses one at boot,
    /// because `?priority=` is a setting on the *client's* store URL and means
    /// nothing forwarded upstream.
    base_url: String,
    basic_auth: Option<(String, String)>,
}

impl NixBinaryCacheClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let http = new_http_client(Some(30), opts)?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            basic_auth: opts.basic_auth.clone(),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    /// Fetch and parse one store path's narinfo.
    ///
    /// `404`, `410` **and** `403` all become [`CoreError::NotFound`], mirroring
    /// `HttpBinaryCacheStore::getFile`: it maps 404/410 to
    /// `FileTransfer::NotFound` and 403 to `Forbidden`, and `fileExists` treats
    /// the two the same way — `queryPathInfoUncached` turns either into "no
    /// info", so Nix consults the next substituter or builds. Reporting a `403`
    /// as an upstream *error* here would turn a cache that simply refuses
    /// anonymous reads into a failed build.
    pub async fn narinfo(&self, store_hash: &str) -> Result<NarInfo, CoreError> {
        if !nix::is_store_hash(store_hash) {
            return Err(CoreError::InvalidInput(format!(
                "'{store_hash}' is not a 32-character Nix base32 store hash"
            )));
        }
        let url = self.url(&format!("{store_hash}.narinfo"));
        let resp = self.get(&url).send().await.map_err(to_registry_error)?;
        let status = resp.status();
        if matches!(status.as_u16(), 403 | 404 | 410) {
            return Err(CoreError::NotFound(format!(
                "{store_hash}.narinfo is not in this cache ({status})"
            )));
        }
        let resp = resp.error_for_status().map_err(to_registry_error)?;
        let text = resp.text().await.map_err(to_registry_error)?;
        let info = NarInfo::parse(&text)?;
        info.require_fields()?;
        Ok(info)
    }

    /// The absolute upstream URL of the NAR a narinfo names.
    ///
    /// `URL:` is relative to the cache root — that is the one thing every cache
    /// agrees on, and why a substituter behind a path prefix needs no rewriting
    /// for the NAR to be found. It is still run through
    /// [`ensure_url_under_base`]: the narinfo is an *upstream* document, and a
    /// value in it can be absolute and point anywhere. That is not
    /// hypothetical — the Open VSX and Terraform cross-host defects both
    /// started at "our own document said so".
    pub fn nar_url(&self, info: &NarInfo) -> Result<String, CoreError> {
        let raw = info
            .get("URL")
            .ok_or_else(|| CoreError::Registry("upstream narinfo carries no 'URL'".to_owned()))?;
        let joined = if raw.starts_with("http://") || raw.starts_with("https://") {
            raw.to_owned()
        } else {
            self.url(raw)
        };
        ensure_url_under_base(&joined, &self.base_url)?;
        Ok(joined)
    }

    /// Split a NAR coordinate's `artifact` back into its two halves.
    ///
    /// The sub-coordinate is `{storeHash}/{basename}` — see RFC 0028 §4.3 and
    /// the todo's delta 8 for why the store hash is in there: `ProxyRequest`
    /// carries a `PackageId` and nothing else, so without it this client has no
    /// way to find the narinfo that names the file.
    fn split_nar_artifact(artifact: &str) -> Option<(&str, &str)> {
        let (hash, file) = artifact.split_once('/')?;
        (nix::is_store_hash(hash) && !file.is_empty() && !file.contains('/'))
            .then_some((hash, file))
    }

    /// The store hash a coordinate is about, whichever artifact shape it uses.
    fn store_hash_of(pkg: &PackageId) -> Result<String, CoreError> {
        let artifact = pkg.artifact.as_deref().ok_or_else(|| {
            CoreError::InvalidInput(format!(
                "nix coordinate '{pkg}' names no artifact: a store path is addressed by its hash"
            ))
        })?;
        if let Some((hash, _)) = Self::split_nar_artifact(artifact) {
            return Ok(hash.to_owned());
        }
        let bare = artifact
            .strip_suffix(".narinfo")
            .or_else(|| artifact.strip_suffix(".ls"))
            .unwrap_or(artifact);
        if nix::is_store_hash(bare) {
            return Ok(bare.to_owned());
        }
        Err(CoreError::InvalidInput(format!(
            "nix artifact '{artifact}' names no store hash"
        )))
    }
}

#[async_trait]
impl RegistryClient for NixBinaryCacheClient {
    fn registry_type(&self) -> &str {
        "nix"
    }

    /// Resolve one store path through its narinfo.
    ///
    /// `published_at` is **always** `None`, and that is a fact about the
    /// protocol rather than a gap here: a narinfo carries hashes, a closure and
    /// a deriver, and no date anywhere. It is why a `release_age_gate` on this
    /// kind must set `deny_missing_timestamp` explicitly — the field is the
    /// whole rule (RFC 0028 §4.5).
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let hash = Self::store_hash_of(pkg)?;
        let info = self.narinfo(&hash).await?;
        let path = info.store_path()?;

        // The download URL is only meaningful for a NAR coordinate; for a
        // narinfo or a `.ls` the artifact *is* the document.
        let download_url = match pkg.artifact.as_deref() {
            Some(a) if Self::split_nar_artifact(a).is_some() => Some(self.nar_url(&info)?),
            _ => None,
        };

        // See `nix_hash_to_sri`: the narinfo's own `sha256:{nix32}` spelling is
        // read by `integrity::parse_expected` as *nothing*, so passing it
        // through would silently disable cache-write verification with one
        // `WARN` per download as the only symptom (RFC 0031 §13's defect 1).
        // `FileHash` is the right one of the two: it covers the compressed
        // bytes, which is what this server stores and re-serves.
        //
        // Gated on the same coordinate shape as `download_url`, and for a
        // sharper reason than symmetry: `FileHash` covers the *NAR*, so
        // attaching it to a narinfo or a `{hash}.ls` coordinate hands the
        // integrity check a digest of bytes it is not looking at, and every
        // `nix store ls` fails verification against a cache that is serving
        // exactly the right document.
        let checksum = match pkg.artifact.as_deref() {
            Some(a) if Self::split_nar_artifact(a).is_some() => {
                info.get("FileHash").and_then(nix::nix_hash_to_sri)
            }
            _ => None,
        };

        let extra = serde_json::json!({
            "store_path": path.to_full_path(),
            "store_hash": path.hash,
            "nar_hash": info.get("NarHash"),
            "nar_size": info.get("NarSize"),
            "compression": info.get("Compression").unwrap_or("bzip2"),
            "references": info.get_all("References"),
            "deriver": info.get("Deriver"),
            "content_addressed": info.get("CA").is_some(),
            "signatures": info.get_all("Sig"),
        });

        Ok(PackageMetadata {
            id: PackageId {
                name: path.package.clone(),
                version: path.version.clone(),
                ..pkg.clone()
            },
            published_at: None,
            download_url,
            checksum,
            // A narinfo is signed or it is not, and the client is what
            // verifies it. Reporting the fact lets `require_upstream_sigs` and
            // the console say something true about a relayed path without this
            // server holding a second copy of `trusted-public-keys`.
            is_signed: Some(!info.get_all("Sig").is_empty()),
            extra,
            cache_control: None,
        })
    }

    /// The protocol's documents. Each is relayed as the upstream sends it; the
    /// narinfo's one rewritten line happens at the handler, which is also where
    /// the reverse index is written and the block is enforced.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        let (path, content_type, what) = if kind == DocumentKind::NARINFO {
            if !nix::is_store_hash(package) {
                return Err(CoreError::InvalidInput(format!(
                    "'{package}' is not a 32-character Nix base32 store hash"
                )));
            }
            (
                format!("{package}.narinfo"),
                CT_NARINFO,
                format!("narinfo for '{package}'"),
            )
        } else if kind == DocumentKind::CACHE_INFO {
            (
                "nix-cache-info".to_owned(),
                CT_CACHE_INFO,
                "nix-cache-info".to_owned(),
            )
        } else if kind == DocumentKind::REALISATION {
            batlehub_core::services::local_registry::validate_package_name(package)?;
            (
                format!("realisations/{package}.doi"),
                "application/json",
                format!("realisation '{package}'"),
            )
        } else if kind == DocumentKind::BUILD_LOG {
            batlehub_core::services::local_registry::validate_package_name(package)?;
            (
                format!("log/{package}"),
                "text/plain; charset=utf-8",
                format!("build log '{package}'"),
            )
        } else {
            return Err(CoreError::NotSupported(format!(
                "a Nix binary cache has no '{kind}' document: the protocol is nix-cache-info, \
                 one narinfo per store path, and the NARs"
            )));
        };

        let url = self.url(&path);
        let resp = self.get(&url).send().await.map_err(to_registry_error)?;
        // Same mapping as `narinfo`: `getFile` treats all three as absence, and
        // absence on a mass-query cache is the common case, not an error.
        if matches!(resp.status().as_u16(), 403 | 404 | 410) {
            return Err(CoreError::NotFound(format!(
                "{what} is not in this cache ({})",
                resp.status()
            )));
        }
        let text = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .text()
            .await
            .map_err(to_registry_error)?;
        Ok(VersionDocument::text(content_type, text))
    }

    /// Stream a NAR, or a `.ls` listing, under its coordinate.
    ///
    /// Costs one narinfo read to learn the upstream URL. `fetch_artifact_resolved`
    /// below is the path that avoids it when the caller has already resolved —
    /// which, on the proxy read path, it always has, because the block is
    /// enforced on the coordinate the narinfo produced.
    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let artifact = pkg.artifact.as_deref().unwrap_or_default();
        let url = if let Some((hash, _)) = Self::split_nar_artifact(artifact) {
            let info = self.narinfo(hash).await?;
            self.nar_url(&info)?
        } else if let Some(hash) = artifact.strip_suffix(".ls") {
            if !nix::is_store_hash(hash) {
                return Err(CoreError::InvalidInput(format!(
                    "nix artifact '{artifact}' names no store hash"
                )));
            }
            self.url(&format!("{hash}.ls"))
        } else {
            return Err(CoreError::InvalidInput(format!(
                "nix artifact '{artifact}' is neither '{{hash}}/{{file}}' nor '{{hash}}.ls'"
            )));
        };
        self.stream(&url).await
    }

    /// The same fetch for a caller that already resolved.
    ///
    /// **`download_url` is re-checked rather than trusted**, per the trait's own
    /// obligation: it came from this registry's own `resolve_metadata`, but the
    /// document it came out of is the *upstream's*, and a `URL:` in it can be
    /// absolute and point anywhere.
    async fn fetch_artifact_resolved(
        &self,
        pkg: &PackageId,
        resolved: &PackageMetadata,
    ) -> Result<Option<FetchedArtifact>, CoreError> {
        let Some(url) = resolved.download_url.as_deref() else {
            return Ok(None);
        };
        let _ = pkg;
        ensure_url_under_base(url, &self.base_url)?;
        self.stream(url).await.map(Some)
    }

    /// There is no listing to read: a cache answers a store path or it does
    /// not, and the path is computed by an evaluation this instance cannot
    /// perform.
    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        Err(CoreError::NotSupported(format!(
            "a Nix binary cache has no version list to enumerate for '{package}': it answers one \
             store path at a time, and the path is computed by the client"
        )))
    }
}

impl NixBinaryCacheClient {
    async fn stream(&self, url: &str) -> Result<FetchedArtifact, CoreError> {
        tracing::debug!(url = %url, "fetching NAR");
        let resp = self.get(url).send().await.map_err(to_registry_error)?;
        if matches!(resp.status().as_u16(), 403 | 404 | 410) {
            return Err(CoreError::NotFound(format!(
                "{url} is not in this cache ({})",
                resp.status()
            )));
        }
        let resp = resp.error_for_status().map_err(to_registry_error)?;
        let cache_control = cache_control(&resp);
        Ok(FetchedArtifact {
            stream: Box::pin(resp.bytes_stream().map_err(to_registry_error)),
            cache_control,
        })
    }
}

/// `nix-cache-info` for a registry that has no upstream to relay one from.
///
/// `StoreDir` has to be `/nix/store` or every stock client refuses the cache
/// outright — *"binary cache '…' is for Nix stores with prefix '…', not '…'"* —
/// so it is not a knob. `WantMassQuery: 1` invites `nix-env -qa`-style tools to
/// ask about everything, which is what this proxy wants: a miss is cheap and
/// counted, and a path nobody asks about is never cached.
///
/// `Priority: 30` sorts this cache *before* `cache.nixos.org` (40; lower wins)
/// when a client lists both, which is the useful default for a local cache of
/// things built here. A client that disagrees sets `?priority=` on its own
/// `substituters` line, which is the one value an operator would change and the
/// reason proxy mode relays the upstream's document rather than composing one.
pub fn local_cache_info() -> String {
    "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 30\n".to_owned()
}

/// The content type each protocol document is served as.
///
/// Not a `const fn`: `DocumentKind::Secondary` wraps a `&'static str` and
/// string comparison is not const-stable, so this matches on
/// [`DocumentKind::as_str`] — the same discriminant the cache key uses, which
/// is what keeps the two from drifting.
pub fn content_type_for(kind: DocumentKind) -> &'static str {
    match kind.as_str() {
        "narinfo" => CT_NARINFO,
        "cache-info" => CT_CACHE_INFO,
        "realisation" => "application/json",
        _ => "text/plain; charset=utf-8",
    }
}

/// The content type a NAR is served as.
pub const fn nar_content_type() -> &'static str {
    CT_NAR
}
