use async_trait::async_trait;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use bytes::{Bytes, BytesMut};
use futures::{stream, StreamExt};
use sha2::{Digest, Sha256};

use batlehub_core::{
    error::CoreError,
    ports::{ByteStream, StorageBackend, StorageMeta, StoreOutcome, StoredArtifact},
};

use super::{super::read_chunked, S3StorageBackend};

/// Multipart part size. Parts (except the last) must be ≥ 5 MiB per the S3 API;
/// 8 MiB keeps a comfortable margin while bounding peak memory to one part.
const PART_SIZE: usize = 8 * 1024 * 1024;

#[async_trait]
impl StorageBackend for S3StorageBackend {
    async fn store(&self, key: &str, data: Bytes, meta: StorageMeta) -> Result<(), CoreError> {
        let obj_key = self.object_key(key)?;
        let len = data.len() as i64;

        let mut req = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(&obj_key)
            .body(data.into())
            .content_length(len);

        if let Some(ref ct) = meta.content_type {
            req = req.content_type(ct);
        }

        req.send()
            .await
            .map_err(|e| CoreError::Storage(format!("S3 put_object {obj_key}: {e}")))?;

        tracing::debug!(key = %key, bytes = %len, "stored artifact in S3");
        Ok(())
    }

    /// Stream bytes to S3 while hashing them. Small objects go through a single
    /// `put_object`; once the buffered data crosses one part we switch to a
    /// multipart upload, flushing full parts as they accumulate so peak memory
    /// stays bounded to ~`PART_SIZE` regardless of artifact size.
    async fn store_streaming(
        &self,
        key: &str,
        mut stream: ByteStream,
        meta: StorageMeta,
    ) -> Result<StoreOutcome, CoreError> {
        let obj_key = self.object_key(key)?;
        let mut hasher = Sha256::new();
        let mut total: u64 = 0;
        let mut buf = BytesMut::with_capacity(PART_SIZE);

        let mut upload_id: Option<String> = None;
        let mut completed: Vec<CompletedPart> = Vec::new();
        let mut part_number = 1;

        // The per-S3-call bookkeeping is factored into the inherent helpers below
        // (an `async` closure can't borrow `self` across awaits cleanly).
        let result: Result<(), CoreError> = async {
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                hasher.update(&chunk);
                total += chunk.len() as u64;
                buf.extend_from_slice(&chunk);

                while buf.len() >= PART_SIZE {
                    if upload_id.is_none() {
                        upload_id = Some(self.create_multipart(&obj_key, &meta).await?);
                    }
                    let id = upload_id.as_deref().unwrap();
                    let part = buf.split_to(PART_SIZE).freeze();
                    completed.push(
                        self.upload_one_part(&obj_key, id, part_number, part)
                            .await?,
                    );
                    part_number += 1;
                }
            }

            match upload_id.as_deref() {
                // Whole object fit under one part: a single put is cheaper.
                None => self.put_single(&obj_key, buf.freeze(), &meta).await?,
                // Multipart in progress: flush the trailing remainder as the last
                // part (it may be < 5 MiB, which is allowed for the final part).
                Some(id) => {
                    if !buf.is_empty() {
                        let part = buf.split().freeze();
                        completed.push(
                            self.upload_one_part(&obj_key, id, part_number, part)
                                .await?,
                        );
                    }
                    self.complete_multipart(&obj_key, id, completed).await?;
                }
            }
            Ok(())
        }
        .await;

        if let Err(e) = result {
            // Abort the multipart upload so we don't leak storage-billed parts.
            if let Some(id) = upload_id {
                let _ = self
                    .client
                    .abort_multipart_upload()
                    .bucket(&self.bucket)
                    .key(&obj_key)
                    .upload_id(id)
                    .send()
                    .await;
            }
            return Err(e);
        }

        tracing::debug!(key = %key, bytes = %total, "streamed artifact to S3");
        Ok(StoreOutcome {
            content_hash: hex::encode(hasher.finalize()),
            size: total,
        })
    }

    /// Server-side copy + delete — the object bytes never pass through us.
    async fn move_key(&self, from: &str, to: &str) -> Result<(), CoreError> {
        let from_key = self.object_key(from)?;
        let to_key = self.object_key(to)?;
        self.client
            .copy_object()
            .bucket(&self.bucket)
            .key(&to_key)
            .copy_source(format!("{}/{}", self.bucket, from_key))
            .send()
            .await
            .map_err(|e| {
                CoreError::Storage(format!("S3 copy_object {from_key} -> {to_key}: {e}"))
            })?;
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(&from_key)
            .send()
            .await
            .map_err(|e| CoreError::Storage(format!("S3 delete_object {from_key}: {e}")))?;
        Ok(())
    }

    async fn retrieve(&self, key: &str) -> Result<Option<StoredArtifact>, CoreError> {
        let obj_key = self.object_key(key)?;

        let resp = match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&obj_key)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let sdk_err = e.into_service_error();
                let err_str = sdk_err.to_string();
                if Self::is_not_found(&sdk_err.into()) {
                    return Ok(None);
                }
                return Err(CoreError::Storage(format!(
                    "S3 get_object {obj_key}: {err_str}"
                )));
            }
        };

        let size = resp.content_length().map(|n| n as u64);
        let content_type = resp.content_type().map(str::to_owned);

        // Stream the body back in fixed chunks instead of buffering it whole. A
        // zero-length object still yields exactly one (empty) chunk so consumers
        // that expect at least one item behave as they did before streaming.
        let stream: ByteStream = if size == Some(0) {
            Box::pin(stream::once(async { Ok(Bytes::new()) }))
        } else {
            read_chunked(resp.body.into_async_read(), obj_key.clone())
        };

        Ok(Some(StoredArtifact {
            stream,
            meta: StorageMeta {
                size,
                content_type,
                ..Default::default()
            },
        }))
    }

    async fn exists(&self, key: &str) -> Result<bool, CoreError> {
        let obj_key = self.object_key(key)?;

        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&obj_key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => {
                let sdk_err = e.into_service_error();
                let err_str = sdk_err.to_string();
                if Self::is_not_found(&sdk_err.into()) {
                    return Ok(false);
                }
                Err(CoreError::Storage(format!(
                    "S3 head_object {obj_key}: {err_str}"
                )))
            }
        }
    }

    async fn delete(&self, key: &str) -> Result<bool, CoreError> {
        let obj_key = self.object_key(key)?;

        match self
            .client
            .delete_object()
            .bucket(&self.bucket)
            .key(&obj_key)
            .send()
            .await
        {
            // S3 returns 204 regardless of whether the object existed; treat as present.
            Ok(_) => Ok(true),
            Err(e) => {
                let sdk_err = e.into_service_error();
                let err_str = sdk_err.to_string();
                // NoSuchKey on delete is fine (some S3-compatible stores do return it)
                if Self::is_not_found(&sdk_err.into()) {
                    return Ok(false);
                }
                Err(CoreError::Storage(format!(
                    "S3 delete_object {obj_key}: {err_str}"
                )))
            }
        }
    }

    async fn stat_by_prefix(&self, prefix: &str) -> Result<(u64, u64), CoreError> {
        let s3_prefix = self.object_key(prefix)?;
        let mut count = 0u64;
        let mut total_bytes = 0u64;
        let mut continuation_token: Option<String> = None;

        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&s3_prefix);
            if let Some(ref token) = continuation_token {
                req = req.continuation_token(token);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| CoreError::Storage(format!("S3 list_objects {s3_prefix}: {e}")))?;

            for obj in resp.contents() {
                count += 1;
                total_bytes += obj.size().unwrap_or(0) as u64;
            }

            let is_truncated = resp.is_truncated().unwrap_or(false);
            continuation_token = resp.next_continuation_token().map(str::to_owned);
            if !is_truncated {
                break;
            }
        }

        Ok((count, total_bytes))
    }

    async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, CoreError> {
        let s3_prefix = self.object_key(prefix)?;
        let prefix_strip_len = self.prefix.len();
        let mut keys = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&s3_prefix);
            if let Some(ref token) = continuation_token {
                req = req.continuation_token(token);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| CoreError::Storage(format!("S3 list_objects {s3_prefix}: {e}")))?;

            for obj in resp.contents() {
                if let Some(k) = obj.key() {
                    // Strip the backend prefix to get the logical key.
                    keys.push(k[prefix_strip_len..].to_owned());
                }
            }

            let is_truncated = resp.is_truncated().unwrap_or(false);
            continuation_token = resp.next_continuation_token().map(str::to_owned);
            if !is_truncated {
                break;
            }
        }
        Ok(keys)
    }

    async fn delete_by_prefix(&self, prefix: &str) -> Result<usize, CoreError> {
        use aws_sdk_s3::types::{Delete, ObjectIdentifier};

        let s3_prefix = self.object_key(prefix)?;
        let mut total = 0usize;
        let mut continuation_token: Option<String> = None;

        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&s3_prefix);
            if let Some(ref token) = continuation_token {
                req = req.continuation_token(token);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| CoreError::Storage(format!("S3 list_objects {s3_prefix}: {e}")))?;

            // `DeleteObjects` addresses objects by their *full* key, exactly as
            // `list_objects_v2` returned them. The configured backend prefix is
            // already part of that key and must not be stripped here — unlike
            // `list_keys`, which strips it because it returns logical keys to a
            // caller that never sees the prefix. Stripping it here would ask S3
            // to delete keys that do not exist, leaving the bucket untouched
            // while reporting a successful purge.
            let object_keys: Vec<ObjectIdentifier> = resp
                .contents()
                .iter()
                .filter_map(|o| o.key())
                .filter_map(|k| ObjectIdentifier::builder().key(k.to_owned()).build().ok())
                .collect();

            let batch_len = object_keys.len();
            if batch_len > 0 {
                let delete = Delete::builder()
                    .set_objects(Some(object_keys))
                    .build()
                    .map_err(|e| CoreError::Storage(format!("S3 delete build: {e}")))?;
                let resp = self
                    .client
                    .delete_objects()
                    .bucket(&self.bucket)
                    .delete(delete)
                    .send()
                    .await
                    .map_err(|e| CoreError::Storage(format!("S3 delete_objects: {e}")))?;

                // Per-object failures come back in the body of a `200`, not as a
                // request error, so a batch can partially fail silently. Count
                // only what S3 confirmed and surface the rest.
                for err in resp.errors() {
                    tracing::warn!(
                        key = err.key().unwrap_or("<unknown>"),
                        code = err.code().unwrap_or("<none>"),
                        "S3 delete_objects: object not deleted"
                    );
                }
                total += batch_len.saturating_sub(resp.errors().len());
            }

            let is_truncated = resp.is_truncated().unwrap_or(false);
            continuation_token = resp.next_continuation_token().map(str::to_owned);
            if !is_truncated {
                break;
            }
        }

        Ok(total)
    }
}

/// Single-S3-call helpers for the multipart streaming upload in `store_streaming`.
/// Each wraps one SDK call and maps its error, keeping the orchestrator readable.
impl S3StorageBackend {
    /// Begin a multipart upload, returning its upload id.
    async fn create_multipart(
        &self,
        obj_key: &str,
        meta: &StorageMeta,
    ) -> Result<String, CoreError> {
        let create = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(obj_key)
            .set_content_type(meta.content_type.clone())
            .send()
            .await
            .map_err(|e| CoreError::Storage(format!("S3 create_multipart {obj_key}: {e}")))?;
        create.upload_id().map(str::to_owned).ok_or_else(|| {
            CoreError::Storage(format!(
                "S3 create_multipart {obj_key}: no upload id returned"
            ))
        })
    }

    /// Upload one part, returning the `CompletedPart` to record for completion.
    async fn upload_one_part(
        &self,
        obj_key: &str,
        upload_id: &str,
        part_number: i32,
        body: Bytes,
    ) -> Result<CompletedPart, CoreError> {
        let resp = self
            .client
            .upload_part()
            .bucket(&self.bucket)
            .key(obj_key)
            .upload_id(upload_id)
            .part_number(part_number)
            .body(body.into())
            .send()
            .await
            .map_err(|e| {
                CoreError::Storage(format!("S3 upload_part {obj_key} #{part_number}: {e}"))
            })?;
        Ok(CompletedPart::builder()
            .set_e_tag(resp.e_tag().map(str::to_owned))
            .part_number(part_number)
            .build())
    }

    /// `put_object` for an object that fit entirely under one part.
    async fn put_single(
        &self,
        obj_key: &str,
        data: Bytes,
        meta: &StorageMeta,
    ) -> Result<(), CoreError> {
        let len = data.len() as i64;
        let mut req = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(obj_key)
            .body(data.into())
            .content_length(len);
        if let Some(ref ct) = meta.content_type {
            req = req.content_type(ct);
        }
        req.send()
            .await
            .map_err(|e| CoreError::Storage(format!("S3 put_object {obj_key}: {e}")))?;
        Ok(())
    }

    /// Finalize a multipart upload from its recorded parts.
    async fn complete_multipart(
        &self,
        obj_key: &str,
        upload_id: &str,
        completed: Vec<CompletedPart>,
    ) -> Result<(), CoreError> {
        self.client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(obj_key)
            .upload_id(upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(completed))
                    .build(),
            )
            .send()
            .await
            .map_err(|e| CoreError::Storage(format!("S3 complete_multipart {obj_key}: {e}")))?;
        Ok(())
    }
}
