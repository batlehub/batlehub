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
pub fn pending_nar_prefix(registry: &str, publisher: &str) -> String {
    format!("nix-pending:{registry}/{publisher}/")
}

/// One parked upload, under the second it arrived.
///
/// **The timestamp is in the key because storage has no clock.** The port
/// offers `list_keys`, `retrieve`, `delete` and no modification time, so the
/// only backend-agnostic place to record when a NAR was parked is its own name
/// — and an expiry that depended on a `StorageMeta` field would have to be
/// added to every backend, including the deduplicating router where a logical
/// key's age is not a well-defined question.
///
/// `{secs}/{file}` as two segments rather than one joined string: a NAR file
/// name may contain a `/` (only traversal and leading/trailing slashes are
/// refused), so the split has to be from the left, on the first separator, and
/// everything after it is the name.
pub fn pending_nar_key(registry: &str, publisher: &str, uploaded_at: i64, file: &str) -> String {
    format!(
        "{}{uploaded_at}/{file}",
        pending_nar_prefix(registry, publisher)
    )
}

/// `(uploaded_at, file)` read back out of a key under `prefix`.
fn parse_pending_nar_key<'a>(prefix: &str, key: &'a str) -> Option<(i64, &'a str)> {
    let rest = key.strip_prefix(prefix)?;
    let (secs, file) = rest.split_once('/')?;
    Some((secs.parse().ok()?, file))
}

/// What one registry's staging area will hold (RFC 0028 §4.4).
///
/// Per registry because both halves are deployment facts rather than protocol
/// ones: a link where the NAR and its narinfo are far apart wants a longer
/// window, and an instance whose publishers are not all trusted equally wants a
/// shorter one. Absent from the config means [`Default`], which is the pair
/// below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NixStagingLimits {
    /// Seconds an unclaimed NAR may wait before a later upload sweeps it.
    pub ttl_secs: i64,
    /// Unclaimed NARs one publisher may hold at once.
    pub max_pending: usize,
}

impl Default for NixStagingLimits {
    fn default() -> Self {
        Self {
            ttl_secs: PENDING_NAR_TTL_SECS,
            max_pending: MAX_PENDING_NARS,
        }
    }
}

/// Seconds since the epoch, for the key an upload is parked under.
fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// How long an unclaimed NAR is kept before a later upload sweeps it away.
///
/// `nix copy --to` sends a path's NAR and its narinfo back to back — the window
/// between them is seconds even on a slow link, because the closure is walked
/// path by path — so an hour is not a deadline any honest publish can miss. It
/// is short enough that a client which died mid-copy, or a caller probing what
/// the staging area will hold, does not leave bytes behind for a week.
pub const PENDING_NAR_TTL_SECS: i64 = 3600;

/// How many unclaimed NARs one publisher may hold at once.
///
/// Sized off the client rather than guessed: `nix copy` parallelises over
/// `http-connections`, which defaults to **25**, and each in-flight path holds
/// at most one unclaimed NAR. 64 is comfortably above that for a legitimate
/// copy of any size, and a ceiling for anything else.
pub const MAX_PENDING_NARS: usize = 64;

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
        // RFC 0015 §5.1 on a request that has no coordinate to resolve against.
        // The narinfo's own `enforce_publish_policy` is the coordinate-scoped
        // check and still runs; this one decides who may consume the staging
        // area at all.
        if !crate::services::authz::holds_anywhere_in_registry(
            &self.hot,
            registry,
            publisher,
            crate::entities::Action::ReleasesPublish,
        )
        .await
        {
            return Err(CoreError::AccessDenied(format!(
                "no grant for 'releases:publish' on registry '{registry}'. A NAR arrives before \
                 the narinfo that names it, so this upload is authorized against the registry \
                 and its namespaces — a grant scoped to one package cannot reach it, because at \
                 this point in `nix copy --to` no package is named yet"
            )));
        }

        let limits = self.nix_staging_limits(registry).await;
        let publisher_key = publisher_key(publisher);
        let live = self
            .sweep_pending_nars(registry, &publisher_key, file, limits)
            .await?;
        if live >= limits.max_pending {
            return Err(CoreError::QuotaExceeded(format!(
                "this publisher already holds {live} unclaimed NAR uploads on registry \
                 '{registry}' and max_pending_nars is {}. An unclaimed NAR is bytes no narinfo \
                 has named; they are swept {}s after they arrive (pending_nar_ttl_secs), or \
                 immediately when their narinfo claims them",
                limits.max_pending, limits.ttl_secs
            )));
        }

        let key = pending_nar_key(registry, &publisher_key, now_secs(), file);
        self.storage
            .store(&key, bytes, Default::default())
            .await
            .map_err(|e| CoreError::Storage(format!("parking the uploaded NAR: {e}")))?;
        tracing::debug!(registry = %registry, file = %file, "parked an unclaimed NAR");
        Ok(())
    }

    /// This registry's staging limits, or the defaults.
    async fn nix_staging_limits(&self, registry: &str) -> NixStagingLimits {
        self.hot
            .read()
            .await
            .nix_staging
            .get(registry)
            .copied()
            .unwrap_or_default()
    }

    /// Delete this publisher's expired uploads — and any earlier upload of
    /// `superseded`, when a file is being re-parked — and count what is left.
    ///
    /// Swept on the write path rather than by a background task: the only
    /// caller that cares is the one about to add to the pile, and a sweeper
    /// thread would be a second place for this rule to live. A publisher who
    /// stops uploading leaves at most [`MAX_PENDING_NARS`] entries behind,
    /// which the next upload — or the registry's deletion — clears.
    async fn sweep_pending_nars(
        &self,
        registry: &str,
        publisher_key: &str,
        superseded: &str,
        limits: NixStagingLimits,
    ) -> Result<usize, CoreError> {
        let prefix = pending_nar_prefix(registry, publisher_key);
        let keys = self
            .storage
            .list_keys(&prefix)
            .await
            .map_err(|e| CoreError::Storage(format!("listing the pending NARs: {e}")))?;
        let cutoff = now_secs() - limits.ttl_secs;
        let mut live = 0usize;
        for key in keys {
            let Some((uploaded_at, file)) = parse_pending_nar_key(&prefix, &key) else {
                // A key this function did not write. Left alone rather than
                // deleted: guessing at bytes whose shape is unknown is how a
                // sweeper becomes a data-loss bug.
                continue;
            };
            if uploaded_at > cutoff && file != superseded {
                live += 1;
                continue;
            }
            if let Err(e) = self.storage.delete(&key).await {
                tracing::warn!(registry = %registry, key = %key, error = %e,
                    "could not sweep a pending NAR");
                live += 1;
            }
        }
        Ok(live)
    }

    /// This publisher's live parked upload of `file`, newest first.
    ///
    /// Newest because a re-upload supersedes: [`Self::publish_nix_nar`] deletes
    /// the earlier entry as it parks the new one, so a second entry can only
    /// exist if that delete failed — and the bytes the client last sent are the
    /// ones its narinfo is about to describe.
    async fn pending_nar_keys(
        &self,
        registry: &str,
        publisher_key: &str,
        file: &str,
    ) -> Result<Vec<String>, CoreError> {
        let prefix = pending_nar_prefix(registry, publisher_key);
        let keys = self
            .storage
            .list_keys(&prefix)
            .await
            .map_err(|e| CoreError::Storage(format!("listing the pending NARs: {e}")))?;
        let cutoff = now_secs() - self.nix_staging_limits(registry).await.ttl_secs;
        let mut found: Vec<(i64, String)> = keys
            .into_iter()
            .filter_map(|key| {
                let (uploaded_at, name) = parse_pending_nar_key(&prefix, &key)?;
                (name == file && uploaded_at > cutoff).then_some((uploaded_at, key.clone()))
            })
            .collect();
        found.sort_by_key(|(uploaded_at, _)| std::cmp::Reverse(*uploaded_at));
        Ok(found.into_iter().map(|(_, key)| key).collect())
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
        // An expired upload answers "no", which is the cheap direction: the
        // client re-uploads. Answering "yes" for bytes the next sweep removes
        // would make the narinfo claim something that is no longer there.
        Ok(self
            .pending_nar_keys(registry, &publisher_key(publisher), file)
            .await
            .map(|keys| !keys.is_empty())
            .unwrap_or(false))
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
        let key = self
            .pending_nar_keys(registry, &publisher_key(publisher), file)
            .await?
            .into_iter()
            .next()
            .unwrap_or_else(|| pending_nar_key(registry, &publisher_key(publisher), 0, file));
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

        // **The two fields the fingerprint does cover are rewritten from what
        // was recomputed, not kept as the publisher spelled them.** This is
        // what makes the doc comment above true. `check_nar` compares
        // `NarHash` with `digests_match`, which deliberately accepts base16
        // and base64 as well as Nix32 — so a correct upload can carry
        // `sha256:<64 hex>`, pass verification, and then be signed over a
        // fingerprint no client will ever reconstruct: Nix prints the parsed
        // hash in Nix32 when it builds the fingerprint to verify. The publish
        // succeeded, a `Sig:` was stored, and every client refused the path
        // with "lacks a signature by a trusted key" and nothing in the log.
        // `NarSize` is canonicalised for the same reason — `0226848` parses
        // equal and is a different string in the fingerprint.
        info.set("NarHash", facts.nar_hash.clone());
        info.set("NarSize", facts.nar_size.to_string());

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
        // Every entry for this file, not one key: the upload time is part of
        // the key now, so a re-upload whose supersede-delete failed would leave
        // a second one behind and the TTL alone would keep it for an hour.
        for pending in self
            .pending_nar_keys(registry, &publisher_key(publisher), &file)
            .await
            .unwrap_or_default()
        {
            if let Err(e) = self.storage.delete(&pending).await {
                tracing::debug!(key = %pending, error = %e, "could not reap a claimed pending NAR");
            }
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
