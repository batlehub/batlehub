//! `ansible-galaxy collection publish`, and the import task it polls
//! afterwards (RFC 0031 §5.4).
//!
//! **The work is finished before the task exists.** The tarball is read,
//! validated and stored before the `POST` answers, and the task it names is
//! already `completed` with a `finished_at`. galaxy_ng answers `waiting` and
//! does the work in a worker; nothing the client does depends on seeing that
//! state — `wait_import_task` loops until `finished_at` is set, treats a `404`
//! on the task as "not started yet", and prints every `messages[]` entry at its
//! own level. So there is no state a client can observe between "the POST
//! returned" and "the version is installable", and a publish followed
//! immediately by an install cannot race.

use std::sync::Arc;

use actix_multipart::Multipart;
use actix_web::{get, post, web, HttpRequest, HttpResponse, Responder};
use bytes::BytesMut;
use futures::StreamExt;

use batlehub_adapters::registry::galaxy::{publish::digest, read_collection_tarball};
use batlehub_core::{
    entities::PublishedPackage,
    services::galaxy::{artifact_filename, version_url},
    services::local_registry::PublishRequest,
    services::LocalRegistryService,
};
use serde::Serialize;
use utoipa::ToSchema;

use super::super::common::{
    extract_signature_headers, publish_and_respond, require_local_mode, require_registry_type,
    MAX_UPLOAD_BYTES,
};
use super::JSON;
use crate::{
    error::AppError, extractors::AuthIdentity, services::NotificationService, RegistryMap,
    RegistryModeMap,
};

/// The publish response: a **bare task id**, not a URL and not a path.
///
/// RFC 0031 §4.4 specified an absolute path here, reasoning about
/// `urljoin(self.api_server, resp['task'])`. That is not what the v3 client
/// does. `wait_import_task` ignores the value as a URL and interpolates it as a
/// single **path segment**:
///
/// ```text
/// _urljoin(api_server, v3, 'imports/collections', task_id, '/')
/// ```
///
/// so a path or a URL here lands in the middle of the polled path and the
/// client spends its whole timeout asking for a route that cannot exist — it
/// treats a `404` as "the import has not started yet" and retries with backoff,
/// so the symptom is a hang, not an error. Measured against ansible-core 2.19.3
/// (RFC 0031 §13).
#[derive(Debug, Serialize, ToSchema)]
pub struct GalaxyImportTask {
    pub task: String,
}

/// The import-task poll. `finished_at` set is what `wait_import_task` waits
/// for.
#[derive(Debug, Serialize, ToSchema)]
pub struct GalaxyTaskStatus {
    pub state: String,
    pub finished_at: Option<String>,
    pub started_at: Option<String>,
    pub messages: Vec<serde_json::Value>,
    pub error: Option<serde_json::Value>,
}

/// Publish a collection — synchronous, so the import task it answers with is already finished.
///
/// `POST …/api/v3/artifacts/collections/`
#[utoipa::path(
    post,
    path = "/proxy/{registry}/galaxy/api/v3/artifacts/collections/",
    tag = "proxy/galaxy",
    params(("registry" = String, Path, description = "Registry name")),
    request_body(content = String, description = "multipart/form-data with `sha256` and `file`", content_type = "multipart/form-data"),
    responses(
        (status = 202, description = "Stored; the task named is already finished", body = GalaxyImportTask),
        (status = 400, description = "Not a collection tarball, or the sha256 field disagrees with the bytes"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry, or mode = proxy"),
        (status = 409, description = "That version is already published"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/proxy/{registry}/galaxy/api/v3/artifacts/collections/")]
#[allow(clippy::too_many_arguments)]
pub async fn galaxy_publish(
    req: HttpRequest,
    path: web::Path<String>,
    mut multipart: Multipart,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    notifications: web::Data<Option<Arc<NotificationService>>>,
    map: web::Data<RegistryMap>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "galaxy", &map)?;
    require_local_mode(&registry, &modes)?;

    let form = collect_publish_form(&mut multipart).await?;
    let bytes = form
        .file
        .ok_or_else(|| AppError::bad_request("no 'file' field in the multipart body"))?;
    let checksum = digest(&bytes);
    // Checked before anything is stored: the client hashes what it downloads
    // and compares it with `artifact.sha256`, so bytes that do not match the
    // digest published beside them fail every install (§7).
    if let Some(claimed) = form.sha256.as_deref() {
        if !claimed.eq_ignore_ascii_case(&checksum) {
            return Err(AppError::bad_request(format!(
                "the 'sha256' form field ({claimed}) is not the digest of the uploaded bytes \
                 ({checksum})"
            )));
        }
    }

    let read = read_collection_tarball(&bytes).map_err(AppError::from)?;
    let info = &read.manifest.collection_info;
    let package = read.manifest.package().map_err(AppError::from)?;
    batlehub_core::services::galaxy::validate_version(&info.version).map_err(AppError::from)?;

    // The filename has to agree with what is inside: a tarball called
    // `acme-util-1.0.0.tar.gz` whose manifest says `2.0.0` would be stored
    // under one coordinate and served under another.
    if let Some(name) = form.filename.as_deref() {
        let expected = artifact_filename(&info.namespace, &info.name, &info.version);
        if name != expected {
            return Err(AppError::bad_request(format!(
                "the uploaded file is called '{name}' and its MANIFEST.json says '{expected}'"
            )));
        }
    }

    let signature = extract_signature_headers(&req)?;
    // The id is the coordinate, which makes the poll route readable in a log
    // and needs no state to answer: the work is already done when the task is
    // named, so nothing has to be looked up to say so.
    let task = format!("{}-{}-{}", info.namespace, info.name, info.version);

    let publish = PublishRequest {
        registry: registry.clone(),
        name: package.clone(),
        version: info.version.clone(),
        artifact: bytes,
        checksum,
        // `manifest` and `files` are stored beside the artifact so a read is
        // never an archive open (§4.4). `FILES.json` travels as it arrived and
        // is never re-derived.
        index_metadata: serde_json::json!({
            "manifest": read.manifest_json,
            "files": read.files_json,
            "requires_ansible": info.dependencies.get("ansible"),
            "dependencies": info.dependencies,
            "tags": info.tags,
            "license": info.license,
            "readme": info.readme,
        }),
        unlisted: false,
        publisher: identity.0.clone(),
        signature_bytes: signature.as_ref().map(|s| s.bytes.clone()),
        signature_type: signature.as_ref().map(|s| s.signature_type.clone()),
    };

    publish_and_respond(
        &local_svc,
        &notifications,
        publish,
        actix_web::http::StatusCode::ACCEPTED,
        GalaxyImportTask { task },
    )
    .await
}

/// The import task a publish returned — always `completed`, because the work is done before the task exists.
///
/// `GET …/api/v3/imports/collections/{task}/`
///
/// Always `completed`: the publish that named this task finished before it
/// answered. A task id nobody published under is still `completed` rather than
/// a `404`, because a `404` is what the client reads as *"not started yet"* and
/// it would poll forever.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/galaxy/api/v3/imports/collections/{task}/",
    tag = "proxy/galaxy",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("task" = String, Path, description = "The bare task id the publish returned"),
    ),
    responses(
        (status = 200, description = "The import task, already finished", body = GalaxyTaskStatus),
        (status = 404, description = "Unknown registry, or mode = proxy"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/galaxy/api/v3/imports/collections/{task}/")]
pub async fn galaxy_import_task(
    path: web::Path<(String, String)>,
    map: web::Data<RegistryMap>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, task) = path.into_inner();
    require_registry_type(&registry, "galaxy", &map)?;
    require_local_mode(&registry, &modes)?;
    batlehub_core::services::validate_path_safe("task", &task).map_err(AppError::from)?;

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Ok(HttpResponse::Ok()
        .content_type(JSON)
        .json(GalaxyTaskStatus {
            state: "completed".to_owned(),
            finished_at: Some(now.clone()),
            started_at: Some(now),
            messages: Vec::new(),
            error: None,
        }))
}

/// The version document for a locally published collection.
///
/// Composed from the publish row, never from the archive: `manifest` and
/// `files` were read once at publish time and stored beside the bytes.
pub(super) fn local_version_document(
    base: &str,
    namespace: &str,
    name: &str,
    row: &PublishedPackage,
) -> serde_json::Value {
    let meta = &row.index_metadata;
    serde_json::json!({
        "version": row.version,
        "href": version_url(base, namespace, name, &row.version),
        "created_at": row.published_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "updated_at": row.published_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "requires_ansible": meta.get("requires_ansible").cloned().unwrap_or(serde_json::Value::Null),
        "artifact": {
            "filename": artifact_filename(namespace, name, &row.version),
            "sha256": row.checksum,
            "size": serde_json::Value::Null,
        },
        "collection": { "name": name, "href": null },
        "namespace": { "name": namespace },
        "metadata": {
            "dependencies": meta.get("dependencies").cloned().unwrap_or(serde_json::json!({})),
            "tags": meta.get("tags").cloned().unwrap_or(serde_json::json!([])),
            "license": meta.get("license").cloned().unwrap_or(serde_json::json!([])),
        },
        "manifest": meta.get("manifest").cloned().unwrap_or(serde_json::Value::Null),
        "files": meta.get("files").cloned().unwrap_or(serde_json::Value::Null),
        // Nothing is minted here. A locally published collection carries no
        // signature, and saying so is honest where an empty list read as
        // "verified" would not be (§3).
        "signatures": [],
        "download_url": null,
    })
}

/// Decode a `Content-Transfer-Encoding: base64` multipart part.
///
/// The encoder that produced it wraps at 76 columns with CRLF, so the
/// whitespace is stripped before decoding rather than fed to a decoder that
/// would refuse it.
fn decode_base64_part(raw: &[u8], field: &str) -> Result<BytesMut, AppError> {
    use base64::Engine as _;
    let packed: Vec<u8> = raw
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&packed)
        .map_err(|e| {
            AppError::bad_request(format!(
                "the '{field}' part declares Content-Transfer-Encoding: base64 and is not \
                 base64: {e}"
            ))
        })?;
    Ok(BytesMut::from(&decoded[..]))
}

/// What the publish multipart body carries.
struct PublishForm {
    sha256: Option<String>,
    file: Option<bytes::Bytes>,
    filename: Option<String>,
}

/// Walk the multipart body, bounded.
///
/// Raw `Multipart` is not covered by `PayloadConfig`, so the cumulative size is
/// bounded here — the same ceiling `collect_payload` applies, and the same
/// reason: otherwise a client could stream an unbounded body into memory.
async fn collect_publish_form(multipart: &mut Multipart) -> Result<PublishForm, AppError> {
    let mut form = PublishForm {
        sha256: None,
        file: None,
        filename: None,
    };
    let mut total: u64 = 0;
    while let Some(field) = multipart.next().await {
        let mut field =
            field.map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?;
        let disposition = field.content_disposition().cloned();
        let field_name = disposition
            .as_ref()
            .and_then(|cd| cd.get_name())
            .unwrap_or("")
            .to_owned();
        let file_name = disposition
            .as_ref()
            .and_then(|cd| cd.get_filename())
            .map(str::to_owned);

        // **The part may be base64.** `ansible-galaxy` builds its body with
        // `prepare_multipart`, whose default encoder for a part read from a
        // *file* is `email.encoders.encode_base64` — so the tarball arrives
        // base64-encoded under `Content-Transfer-Encoding: base64`, and
        // `actix_multipart` hands the raw bytes through without decoding it.
        //
        // Found by the real client: a plain-multipart `curl` publishes fine
        // and `ansible-galaxy collection publish` answered `400`, because the
        // tar reader met base64 text where it expected gzip. Read the header
        // rather than sniffing the body: a `Content-Transfer-Encoding` a
        // client declares is a fact, and guessing would eventually decode a
        // tarball that happened to look like base64.
        let base64_encoded = field
            .headers()
            .get("content-transfer-encoding")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("base64"));

        let mut buf = BytesMut::new();
        while let Some(chunk) = field.next().await {
            let chunk = chunk.map_err(|e| AppError::bad_request(format!("chunk error: {e}")))?;
            total += chunk.len() as u64;
            if total > MAX_UPLOAD_BYTES {
                return Err(AppError::from(
                    batlehub_core::error::CoreError::PayloadTooLarge(format!(
                        "upload exceeds the {MAX_UPLOAD_BYTES}-byte limit"
                    )),
                ));
            }
            buf.extend_from_slice(&chunk);
        }
        let buf = if base64_encoded {
            decode_base64_part(&buf, &field_name)?
        } else {
            buf
        };
        match field_name.as_str() {
            "sha256" => {
                form.sha256 = Some(String::from_utf8_lossy(&buf).trim().to_owned());
            }
            "file" => {
                form.file = Some(buf.freeze());
                form.filename = file_name;
            }
            // An unnamed part is the file: some clients label neither, and the
            // first binary part is the only thing a publish can be.
            _ if form.file.is_none() && !buf.is_empty() => {
                form.file = Some(buf.freeze());
                form.filename = file_name;
            }
            _ => {}
        }
    }
    Ok(form)
}
