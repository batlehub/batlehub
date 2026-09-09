//! The registry's own VSIX signature (RFC 0020 §4.2, §5.2): built at publish,
//! read back — or built on first request — when the gallery's
//! `VsixSignature` asset is asked for, and kept beside the artifact under
//! `artifact_storage_key + ".sigzip"`.
//!
//! No key id is persisted per version: a stored archive is checked against
//! the **current** key over the VSIX bytes the asset route has already read,
//! and rebuilt when it does not verify. A rotated key therefore re-signs on
//! the next request, and the `PublicKey` asset always names the key that
//! verifies what is served — the invariant §5.1 asks for, held by
//! construction rather than by a column.

use std::sync::Arc;

use bytes::Bytes;

use batlehub_core::{
    ports::{collect_byte_stream, StorageMeta},
    services::{
        artifact_storage_key,
        signature::VsxSigningKey,
        vsx_signature::{read_signature_archive, signature_archive, signature_manifest},
        LocalRegistryService,
    },
};

use crate::error::AppError;

/// The sibling key the archive is kept under.
pub(crate) fn signature_storage_key(registry: &str, extension_id: &str, version: &str) -> String {
    format!(
        "{}.sigzip",
        artifact_storage_key(registry, extension_id, version)
    )
}

/// This registry's signing key, when `[registries.vsx_signing]` names one.
pub(crate) async fn registry_key(
    local_svc: &LocalRegistryService,
    registry: &str,
) -> Option<Arc<VsxSigningKey>> {
    local_svc
        .hot
        .read()
        .await
        .vsx_signing
        .get(registry)
        .cloned()
}

/// The signature archive for a locally held version: the stored sibling when
/// it verifies under `key`, a fresh one otherwise (a version published before
/// the key existed, or under a key since rotated).
pub(crate) async fn ensure_signature_archive(
    local_svc: &LocalRegistryService,
    key: &VsxSigningKey,
    registry: &str,
    extension_id: &str,
    version: &str,
    vsix: &Bytes,
) -> Result<Bytes, AppError> {
    let storage_key = signature_storage_key(registry, extension_id, version);
    if let Some(stored) = local_svc.storage.retrieve(&storage_key).await? {
        let bytes = collect_byte_stream(stored.stream).await?;
        match read_signature_archive(&bytes) {
            Ok(archive) if key.verify(&archive.signature, vsix) => return Ok(bytes),
            Ok(_) => tracing::info!(
                registry,
                extension = extension_id,
                version,
                key_id = key.key_id(),
                "stored VSIX signature does not verify under the current key; re-signing"
            ),
            Err(e) => tracing::warn!(
                registry,
                extension = extension_id,
                version,
                error = %e,
                "stored VSIX signature archive is unreadable; re-signing"
            ),
        }
    }

    let manifest = signature_manifest(vsix)?;
    let signature = key.sign(vsix);
    let archive = Bytes::from(signature_archive(&manifest, &signature)?);
    local_svc
        .storage
        .store(
            &storage_key,
            archive.clone(),
            StorageMeta {
                content_type: Some("application/zip".to_owned()),
                size: Some(archive.len() as u64),
                checksum: None,
            },
        )
        .await?;
    tracing::debug!(
        registry,
        extension = extension_id,
        version,
        key_id = key.key_id(),
        "signed VSIX"
    );
    Ok(archive)
}

/// Sign a version that was just published, when this registry holds a key.
/// Best effort: the publish has succeeded, and a signature that could not be
/// written now is built on the first request for it (§4.2).
pub(crate) async fn sign_after_publish(
    local_svc: &LocalRegistryService,
    registry: &str,
    extension_id: &str,
    version: &str,
    vsix: &Bytes,
) {
    let Some(key) = registry_key(local_svc, registry).await else {
        return;
    };
    if let Err(e) =
        ensure_signature_archive(local_svc, &key, registry, extension_id, version, vsix).await
    {
        tracing::warn!(
            registry,
            extension = extension_id,
            version,
            error = %e,
            "could not sign the published VSIX now; it will be signed on first request"
        );
    }
}
