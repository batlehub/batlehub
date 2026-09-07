//! What a VSIX names itself, and what happens after it is published
//! (RFC 0021 §5.2).
//!
//! Both halves of importing an extension that `crates/core` cannot do alone:
//! the coordinate lives inside the archive, and the signature is written into
//! the gallery's own storage layout. Rather than move either into core, the
//! import runs the same code the `PUT …/vsix` route runs — so an entry an
//! import created is indistinguishable from one a publish created, which is
//! the whole claim §5.1 makes.

use std::sync::Arc;

use batlehub_core::services::{CoordinateReader, LocalRegistryService, PostPublish};

/// The coordinate a VSIX carries in `extension/package.json`.
///
/// The same read `POST /api/-/publish` performs, and for the same reason: that
/// route carries no coordinate in its URL either, so the manifest is the only
/// thing that names what is being published. An archive whose manifest will not
/// parse has no coordinate, and the import reports it by name rather than
/// inventing one from the file name — a `.vsix` built by CI is named after the
/// extension often enough to be wrong occasionally, which is the worst kind of
/// guess.
pub struct VsixCoordinates;

impl CoordinateReader for VsixCoordinates {
    fn read(&self, _filename: &str, bytes: &[u8]) -> Option<(String, String)> {
        let manifest = super::archive::parse_manifest(bytes)?;
        Some((manifest.extension_id(), manifest.version.clone()))
    }

    fn index_metadata(&self, name: &str, version: &str, bytes: &[u8]) -> serde_json::Value {
        match super::archive::parse_manifest(bytes) {
            Some(manifest) => manifest.index_metadata(name, version),
            None => serde_json::json!({ "id": name, "version": version }),
        }
    }
}

/// Sign an imported extension exactly as a published one is signed.
pub struct SignAfterPublish {
    pub local: Arc<LocalRegistryService>,
}

#[async_trait::async_trait]
impl PostPublish for SignAfterPublish {
    async fn after_publish(&self, registry: &str, name: &str, version: &str, artifact: &[u8]) {
        super::signing::sign_after_publish(
            &self.local,
            registry,
            name,
            version,
            &bytes::Bytes::copy_from_slice(artifact),
        )
        .await;
    }
}
