//! Publishing a store path (RFC 0028 §4.4, §13).
//!
//! # Why this kind needs two steps and no other does
//!
//! Every other publish in this tree arrives as one request that names its own
//! coordinate: an npm tarball comes with a packument, a `.nupkg` carries its
//! `.nuspec`. A `nix copy --to` does not. Measured from
//! `BinaryCacheStore::addToStoreCommon` and `uploadNarInfo`, it sends:
//!
//! ```text
//! HEAD nar/{fileHash}.nar.xz     ← is it already here?
//! PUT  nar/{fileHash}.nar.xz     ← the bytes, naming no package
//! GET  {ref}.narinfo  × N        ← queryPathInfo over the closure
//! PUT  {storeHash}.narinfo       ← *now* the coordinate exists
//! ```
//!
//! So when the bytes arrive nothing is known about them: not the package, not
//! the version, not even which store path they belong to. The NAR is parked
//! under a **pending** key owned by whoever uploaded it, and the narinfo is
//! what claims it — at which point the coordinate is known, the hashes can be
//! checked against the bytes, and the registry can sign what it verified.
//!
//! Refusing the NAR until a narinfo arrives is not open to us: the client's
//! order is fixed, and it is the right order for the client — uploading
//! megabytes before learning whether the metadata is acceptable would be worse.
//!
//! # Where the verification happens, and why not here
//!
//! `check_nar` lives in `adapters`, because decompressing xz and zstd is
//! infrastructure. This module takes the [`NarFacts`] it produced. The
//! dependency direction (`core ← adapters`) is why, and it is the right split
//! anyway: the *facts* are what the registry's signature attests, and the
//! *codecs* are how they were obtained.

use bytes::Bytes;

use crate::entities::Identity;
use crate::error::CoreError;
use crate::services::nix::{self, NarFacts, NarInfo, NixSigningKey};

use super::{
    nix_nar_storage_key, nix_narinfo_storage_key, validate_package_name, validate_path_safe,
    LocalRegistryService, PublishPolicyRequest,
};

/// Where an unclaimed NAR waits.
///
/// Scoped by publisher as well as registry, and that is a security property
/// rather than tidiness: a narinfo may only claim a NAR **its own publisher**
/// uploaded. Without the scope, a caller holding `releases:write` could wait
/// for someone else's upload and wrap those bytes in a coordinate of their own
/// choosing — the registry would then sign, under its own key, a store path
/// whose contents it attributes to the wrong publisher.
pub fn pending_nar_key(registry: &str, publisher: &str, file: &str) -> String {
    format!("nix-pending:{registry}/{publisher}/{file}")
}

/// The identity a pending upload is filed under.
///
/// Anonymous publishers share one bucket. That is acceptable because an
/// anonymous `releases:write` is already a decision the operator made
/// deliberately; what this protects is a *named* publisher's uploads.
fn publisher_key(publisher: &Identity) -> String {
    publisher
        .user_id
        .clone()
        .unwrap_or_else(|| "anonymous".to_owned())
}

/// Everything [`LocalRegistryService::publish_nix_narinfo`] needs.
///
/// A struct rather than eight parameters, for the reason `PublishRequest` and
/// `PublishPolicyRequest` beside it are: at this arity the call site stops
/// being readable and two arguments of the same type become swappable without
/// the compiler minding. `registry` and `store_hash` are both `&str`, and
/// swapping them would publish under a coordinate nobody asked for.
pub struct NixPublishRequest<'a> {
    pub registry: &'a str,
    /// The hash the narinfo was `PUT` to — checked against the document's own
    /// `StorePath:`, because the two disagreeing means the bytes would land
    /// under a coordinate the document does not name.
    pub store_hash: &'a str,
    pub info: NarInfo,
    /// The NAR itself, already verified against `facts`.
    pub bytes: Bytes,
    /// What `adapters`' `check_nar` computed from `bytes`. Passed in rather
    /// than recomputed so the signature attests something this server checked.
    pub facts: &'a NarFacts,
    pub publisher: &'a Identity,
    /// `None` publishes unsigned, which every stock client refuses — warned
    /// about at reload rather than refused here.
    pub signing_key: Option<&'a NixSigningKey>,
}

/// What a claimed narinfo produced, for the handler to serve and record.
pub struct PublishedStorePath {
    /// The narinfo exactly as this instance will serve it.
    pub narinfo: String,
    /// `{package}` as `DrvName` split it.
    pub package: String,
    /// `{version}`, or `-`.
    pub version: String,
    /// The artifact sub-coordinate, `{storeHash}/{file}`.
    pub artifact: String,
}

impl LocalRegistryService {
    /// Step 1 — park an uploaded NAR under its file name, unclaimed.
    ///
    /// Nothing about the bytes is verified here, because the claim needs a
    /// narinfo to check them *against*. Hashing now to fail a few seconds
    /// earlier would cost a full pass over every upload to catch a case the
    /// claim catches anyway.
    ///
    /// The size limit *is* applied now: it is the one check that does not need
    /// the narinfo, and it is the one that protects the disk.
    pub async fn publish_nix_nar(
        &self,
        registry: &str,
        file: &str,
        bytes: Bytes,
        publisher: &Identity,
    ) -> Result<(), CoreError> {
        validate_path_safe("NAR file name", file)?;
        if let Some(max) = self.hot.read().await.max_artifact_size_bytes {
            if bytes.len() as u64 > max {
                return Err(CoreError::PayloadTooLarge(format!(
                    "the uploaded NAR is {} bytes and limits.max_artifact_size_bytes is {max}",
                    bytes.len()
                )));
            }
        }
        let key = pending_nar_key(registry, &publisher_key(publisher), file);
        self.storage
            .store(&key, bytes, Default::default())
            .await
            .map_err(|e| CoreError::Storage(format!("parking the uploaded NAR: {e}")))?;
        tracing::debug!(registry = %registry, file = %file, "parked an unclaimed NAR");
        Ok(())
    }

    /// Whether a NAR of this name is already here — the `HEAD` that precedes
    /// every upload.
    ///
    /// **Answering this wrong is expensive in one direction only.** A false
    /// "yes" makes `addToStoreCommon` skip the upload, and the narinfo then
    /// claims bytes that are not there; a false "no" costs one re-upload. So
    /// this answers `true` only for a NAR *this publisher* actually parked.
    pub async fn nix_nar_exists(
        &self,
        registry: &str,
        file: &str,
        publisher: &Identity,
    ) -> Result<bool, CoreError> {
        if validate_path_safe("NAR file name", file).is_err() {
            return Ok(false);
        }
        let key = pending_nar_key(registry, &publisher_key(publisher), file);
        Ok(self.storage.exists(&key).await.unwrap_or(false))
    }

    /// Read back the NAR this publisher parked under `file`, for the narinfo
    /// that is about to claim it.
    ///
    /// Separate from [`Self::publish_nix_narinfo`] because the bytes have to be
    /// **verified before** the publish begins, and the verifier lives one crate
    /// out: the handler reads them here, checks them with `adapters`'
    /// `check_nar`, and hands both back.
    pub async fn take_pending_nix_nar(
        &self,
        registry: &str,
        file: &str,
        publisher: &Identity,
    ) -> Result<Bytes, CoreError> {
        validate_path_safe("NAR file name", file)?;
        let key = pending_nar_key(registry, &publisher_key(publisher), file);
        let stored = self
            .storage
            .retrieve(&key)
            .await
            .map_err(|e| CoreError::Storage(format!("reading the pending NAR: {e}")))?
            .ok_or_else(|| {
                CoreError::InvalidInput(format!(
                    "no NAR named '{file}' was uploaded by this publisher. A narinfo can only \
                     claim a NAR its own publisher PUT first — `nix copy --to` sends the NAR \
                     before the narinfo, so this usually means the upload failed or the \
                     credential changed between the two requests"
                ))
            })?;
        crate::ports::collect_byte_stream(stored.stream).await
    }

    /// Step 2 — the narinfo claims the NAR, and the registry signs what was
    /// verified.
    ///
    /// `facts` came from `adapters`' `check_nar` over the very bytes this
    /// method then stores, which is what makes the signature honest: every
    /// field the fingerprint covers was recomputed, not read out of the
    /// document (RFC 0028 decision 3).
    pub async fn publish_nix_narinfo(
        &self,
        req: NixPublishRequest<'_>,
    ) -> Result<PublishedStorePath, CoreError> {
        let NixPublishRequest {
            registry,
            store_hash,
            mut info,
            bytes,
            facts,
            publisher,
            signing_key,
        } = req;
        let path = info.store_path()?;
        if path.hash != store_hash {
            return Err(CoreError::InvalidInput(format!(
                "this narinfo was PUT to {store_hash}.narinfo and its StorePath is {} — the \
                 address and the document have to name the same path",
                path.to_full_path()
            )));
        }
        validate_package_name(&path.package)?;
        validate_path_safe("version", &path.version)?;

        let url = info.get("URL").ok_or_else(|| {
            CoreError::InvalidInput("corrupt NAR info file: missing 'URL'".into())
        })?;
        let file = url.rsplit('/').next().unwrap_or(url).to_owned();
        validate_path_safe("NAR file name", &file)?;

        // Maven's route for a multi-file kind: the shared `publish()` writes one
        // key per version, and a version here holds many store paths. So the
        // policy gate is called directly and the bytes are stored under a key
        // that carries the hash.
        let artifact = format!("{}/{}", path.hash, file);
        let nar_key =
            nix_nar_storage_key(registry, &path.package, &path.version, &path.hash, &file);
        self.enforce_publish_policy(
            &PublishPolicyRequest {
                registry,
                name: &path.package,
                version: &path.version,
                artifact_len: facts.file_size,
                signature_bytes: None,
                signature_type: None,
                // Maven's reason, and a sharper one. A version here holds
                // *many* store paths — two builds of `hello-1.0` differ only in
                // their hash and are both legitimate — so the version row
                // exists from the first store path onward and every later one
                // would read as a replacement. Under `immutable = "always"`
                // that would make a second build of an existing version
                // impossible rather than making the first permanent. Deciding
                // on the key asks the right question: are *these bytes* being
                // replaced.
                artifact_key: Some(&nar_key),
            },
            publisher,
        )
        .await?;

        // `URL:` becomes this instance's layout. Before signing only for
        // tidiness — `URL` is not in the fingerprint, which is the property the
        // entire design rests on.
        info.set("URL", format!("nar/{artifact}"));

        // A publisher cannot mint our signature. Every other `Sig:` is somebody
        // else's provenance and is kept: the publisher may legitimately have
        // signed client-side (`narInfo->sign(*this, signers)`).
        if let Some(key) = signing_key {
            let forged = info.drop_signatures_by(key.key_name());
            if forged > 0 {
                tracing::warn!(
                    registry = %registry,
                    store_path = %path.to_full_path(),
                    dropped = forged,
                    "a publish carried Sig: lines under this registry's own key name; dropped"
                );
            }
            let fingerprint = nix::fingerprint(&info)?;
            info.push_signature(key.sign_fingerprint(&fingerprint));
        }
        let narinfo = info.to_wire();

        self.storage
            .store(&nar_key, bytes, Default::default())
            .await
            .map_err(|e| CoreError::Storage(format!("storing the NAR: {e}")))?;

        let doc_key = nix_narinfo_storage_key(registry, &path.package, &path.version, &path.hash);
        self.storage
            .store(
                &doc_key,
                Bytes::from(narinfo.clone().into_bytes()),
                Default::default(),
            )
            .await
            .map_err(|e| CoreError::Storage(format!("storing the narinfo: {e}")))?;

        // The pending copy has served its purpose. A failure here leaks one
        // object until the publish window reaps it, which is why it is not
        // allowed to fail the publish.
        let pending = pending_nar_key(registry, &publisher_key(publisher), &file);
        if let Err(e) = self.storage.delete(&pending).await {
            tracing::debug!(key = %pending, error = %e, "could not reap a claimed pending NAR");
        }

        tracing::info!(
            registry = %registry,
            store_path = %path.to_full_path(),
            package = %path.package,
            version = %path.version,
            signed = signing_key.is_some(),
            "published a store path"
        );

        Ok(PublishedStorePath {
            narinfo,
            package: path.package,
            version: path.version,
            artifact,
        })
    }

    /// The narinfo this instance holds for one store hash, if any.
    ///
    /// A scan rather than a lookup, for the reason the air-gap synthesis is
    /// registry-wide: a store hash names no package on its own, and the mapping
    /// lives only in the keys.
    pub async fn get_nix_narinfo(
        &self,
        registry: &str,
        store_hash: &str,
    ) -> Result<Option<String>, CoreError> {
        if !nix::is_store_hash(store_hash) {
            return Ok(None);
        }
        let suffix = format!("/{store_hash}.narinfo");
        let keys = self
            .storage
            .list_keys(&format!("local:{registry}/"))
            .await
            .unwrap_or_default();
        let Some(key) = keys.into_iter().find(|k| k.ends_with(&suffix)) else {
            return Ok(None);
        };
        let Some(stored) = self.storage.retrieve(&key).await? else {
            return Ok(None);
        };
        let bytes = crate::ports::collect_byte_stream(stored.stream).await?;
        Ok(String::from_utf8(bytes.to_vec()).ok())
    }
}
