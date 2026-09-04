//! The push side of RFC 0002 (§4.3, recast by §13): a `[[flag_sources]]`
//! entry sends its assertions here, signed.
//!
//! # Authentication
//!
//! No `Identity`: the credential is `X-Hub-Signature-256` over the raw body
//! with the source's configured secret, verified before the body is parsed —
//! the same scheme `[[notifications.inbound]]` uses, so an operator who has
//! wired one has wired both. An unknown source name and a bad signature
//! answer the same `404`, so the endpoint does not enumerate the sources.
//!
//! # What an item can say
//!
//! One exact `version` or `version_range = "*"`. A range other than `*` is
//! refused per item with the reason: ranges need the registry kind's
//! version ordering, which RFC 0002 §13 sends to its own RFC. The push as a
//! whole is refused only for size (over [`MAX_FLAG_BATCH`] items) or rate
//! (`max_flags_per_minute`, `429` with `Retry-After`); every other problem
//! is an item outcome, so a batch with one bad line lands the other 999.

use std::sync::Arc;

use actix_web::{delete, post, web, HttpRequest, HttpResponse, Responder};
use bytes::BytesMut;
use chrono::Utc;
use futures::StreamExt;
use serde::Serialize;
use utoipa::ToSchema;

use batlehub_config::schema::FlagSourceConfig;
use batlehub_core::{
    entities::{FlagPush, FlagPushResponse},
    services::{FlagPushError, FlagService, MAX_FLAG_BATCH},
};

use crate::{error::AppError, services::verify_inbound_hmac};

/// The configured `[[flag_sources]]`, as app data.
#[derive(Clone, Default)]
pub struct FlagSources(pub Vec<FlagSourceConfig>);

/// A push: the items, and nothing else — the source is the path, the
/// credential the header.
#[derive(Debug, serde::Deserialize, ToSchema)]
pub struct FlagPushBody {
    pub flags: Vec<FlagPush>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FlagRevokeResponse {
    pub revoked: bool,
}

/// Bounded like the inbound webhook: a batch of a thousand flags is well
/// under this, and an unauthenticated caller must not stream more into
/// memory than the check needs.
const MAX_PUSH_BYTES: u64 = 5 * 1024 * 1024;

/// Find the source, read the body within bounds, check the signature.
async fn authenticate(
    req: &HttpRequest,
    name: &str,
    payload: Option<&mut web::Payload>,
    sources: &FlagSources,
) -> Result<(FlagSourceConfig, bytes::Bytes), AppError> {
    let Some(source) = sources.0.iter().find(|s| s.name == name).cloned() else {
        return Err(AppError::not_found(format!("unknown flag source: {name}")));
    };
    let mut raw = BytesMut::new();
    if let Some(payload) = payload {
        let mut total: u64 = 0;
        while let Some(chunk) = payload.next().await {
            let chunk = chunk.map_err(|e| AppError::bad_request(e.to_string()))?;
            total += chunk.len() as u64;
            if total > MAX_PUSH_BYTES {
                return Err(AppError::bad_request(format!(
                    "flag push exceeds the {MAX_PUSH_BYTES}-byte limit"
                )));
            }
            raw.extend_from_slice(&chunk);
        }
    }
    let body = raw.freeze();
    let header = req
        .headers()
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !verify_inbound_hmac(&source.secret, &body, header) {
        // The same answer as an unknown name: a bad signature must not
        // confirm the name was right.
        return Err(AppError::not_found(format!("unknown flag source: {name}")));
    }
    Ok((source, body))
}

/// Push a batch of flags from a configured source.
#[utoipa::path(
    post,
    path = "/api/v1/flags/{source}",
    tag = "security",
    params(("source" = String, Path, description = "The `[[flag_sources]]` name")),
    request_body = FlagPushBody,
    responses(
        (status = 200, description = "One outcome per item, in order", body = FlagPushResponse),
        (status = 400, description = "Not JSON, or over the batch size"),
        (status = 404, description = "Unknown source or bad signature"),
        (status = 429, description = "Over the source's per-minute budget; `Retry-After` set"),
    ),
)]
#[post("/api/v1/flags/{source}")]
pub async fn push_flags(
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
    sources: web::Data<FlagSources>,
    svc: web::Data<Arc<FlagService>>,
) -> Result<impl Responder, AppError> {
    let name = path.into_inner();
    let (source, body) = authenticate(&req, &name, Some(&mut payload), &sources).await?;
    let parsed: FlagPushBody = serde_json::from_slice(&body)
        .map_err(|e| AppError::bad_request(format!("request body is not a flag push: {e}")))?;
    let now = Utc::now();
    let n = parsed.flags.len();
    match svc.push(&source.limits(), parsed.flags, now).await {
        Ok(out) => {
            tracing::info!(
                source = %name,
                items = n,
                accepted = out.accepted,
                rejected = out.rejected,
                "flags: push received"
            );
            Ok(HttpResponse::Ok().json(out))
        }
        Err(FlagPushError::TooMany { .. }) => Err(AppError::bad_request(format!(
            "a push carries at most {MAX_FLAG_BATCH} flags"
        ))),
        Err(FlagPushError::RateLimited { retry_after_secs }) => Ok(HttpResponse::TooManyRequests()
            .insert_header(("Retry-After", retry_after_secs.to_string()))
            .json(serde_json::json!({
                "error": "over the source's per-minute budget",
                "retry_after_secs": retry_after_secs,
            }))),
        // Never reached: an item's problem is an item outcome, not a batch
        // error. Answered as one anyway rather than panicked on.
        Err(FlagPushError::Item(e)) => Err(AppError::bad_request(e)),
        Err(FlagPushError::Core(e)) => Err(AppError::from(e)),
    }
}

/// Revoke one flag. The row stays as a tombstone for the exposure report.
#[utoipa::path(
    delete,
    path = "/api/v1/flags/{source}/{external_id}",
    tag = "security",
    params(
        ("source" = String, Path, description = "The `[[flag_sources]]` name"),
        ("external_id" = String, Path, description = "The source's own id for the flag"),
    ),
    responses(
        (status = 200, description = "Whether a live flag was revoked", body = FlagRevokeResponse),
        (status = 404, description = "Unknown source or bad signature"),
    ),
)]
#[delete("/api/v1/flags/{source}/{external_id}")]
pub async fn revoke_flag(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    sources: web::Data<FlagSources>,
    svc: web::Data<Arc<FlagService>>,
) -> Result<impl Responder, AppError> {
    let (name, external_id) = path.into_inner();
    // A DELETE has no body: the signature is over the empty string.
    let (_, _) = authenticate(&req, &name, None, &sources).await?;
    let revoked = svc
        .revoke(&name, &external_id, Utc::now())
        .await
        .map_err(AppError::from)?;
    tracing::info!(source = %name, external_id = %external_id, revoked, "flags: revoke");
    Ok(HttpResponse::Ok().json(FlagRevokeResponse { revoked }))
}
