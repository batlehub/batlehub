use actix_web::http::StatusCode;
use actix_web::HttpResponse;
use serde::Serialize;

use batlehub_core::error::CoreError;

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
    pub message: String,
    /// Stable slug identifying *which* refusal this is, when one status covers
    /// more than one and a client has to tell them apart.
    ///
    /// Safe to match on; `message` is not — it is prose and may be reworded.
    /// The same reasoning `ConfigWarning::code` already carries. Absent on the
    /// endpoints that have only one way to fail with a given status, so no
    /// existing response shape changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

pub struct AppError {
    pub status: StatusCode,
    pub message: String,
    pub code: Option<String>,
    /// Extra fields merged into the JSON body, for the one refusal that has
    /// to carry machine-readable facts rather than a sentence: RFC 0008's
    /// air-gapped miss names the registry, the coordinate and what to do
    /// about it, because the thing reading it is a script building the next
    /// bundle.
    pub details: Option<serde_json::Value>,
}

impl AppError {
    /// One refusal, spelled once.
    ///
    /// Every named constructor below is this with its status filled in — a
    /// private funnel rather than eleven copies of the same three fields, so a
    /// field added to `AppError` is added in one place and cannot be forgotten
    /// on the eleventh.
    fn with_status(status: StatusCode, msg: impl Into<String>) -> Self {
        Self {
            status,
            message: msg.into(),
            code: None,
            details: None,
        }
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::with_status(StatusCode::NOT_FOUND, msg)
    }

    /// The same refusal, carrying a slug a client can branch on.
    ///
    /// For the endpoints where one status covers genuinely different situations
    /// — a README that is absent because the package has none stored, and one
    /// that is absent because the registry type has none to give — and a panel
    /// has to render a statement for one and an error for the other.
    pub fn coded(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    /// Merge machine-readable facts into the body beside `message`.
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    /// The caller could not be identified at all — distinct from
    /// [`forbidden`](Self::forbidden), which means "we know who you are and the
    /// answer is no".
    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self::with_status(StatusCode::UNAUTHORIZED, msg)
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::with_status(StatusCode::FORBIDDEN, msg)
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::with_status(StatusCode::BAD_REQUEST, msg)
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::with_status(StatusCode::INTERNAL_SERVER_ERROR, msg)
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::with_status(StatusCode::CONFLICT, msg)
    }

    /// The route exists and the request is well-formed, but this server does
    /// not implement the operation — as against `404`, which says the route is
    /// not here at all. The distinction matters to a client deciding whether to
    /// retry elsewhere or to stop.
    pub fn not_implemented(msg: impl Into<String>) -> Self {
        Self::with_status(StatusCode::NOT_IMPLEMENTED, msg)
    }

    pub fn unprocessable(msg: impl Into<String>) -> Self {
        Self::with_status(StatusCode::UNPROCESSABLE_ENTITY, msg)
    }

    pub fn too_many_requests(msg: impl Into<String>) -> Self {
        Self::with_status(StatusCode::TOO_MANY_REQUESTS, msg)
    }

    pub fn service_unavailable(msg: impl Into<String>) -> Self {
        Self::with_status(StatusCode::SERVICE_UNAVAILABLE, msg)
    }

    pub fn bad_gateway(msg: impl Into<String>) -> Self {
        Self::with_status(StatusCode::BAD_GATEWAY, msg)
    }
}

impl actix_web::ResponseError for AppError {
    fn status_code(&self) -> StatusCode {
        self.status
    }

    fn error_response(&self) -> HttpResponse {
        let body = ErrorBody {
            error: self.status.canonical_reason().unwrap_or("error").to_owned(),
            message: self.message.clone(),
            code: self.code.clone(),
        };
        // The extra fields are merged into the same object rather than
        // nested, so a client that reads `message` is unaffected and one
        // that reads `registry` does not have to know which refusal it is.
        match (&self.details, serde_json::to_value(&body)) {
            (Some(serde_json::Value::Object(extra)), Ok(serde_json::Value::Object(mut base))) => {
                for (k, v) in extra {
                    base.insert(k.clone(), v.clone());
                }
                HttpResponse::build(self.status).json(serde_json::Value::Object(base))
            }
            _ => HttpResponse::build(self.status).json(body),
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status, self.message)
    }
}

impl std::fmt::Debug for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AppError({} {})", self.status, self.message)
    }
}

impl From<CoreError> for AppError {
    fn from(e: CoreError) -> Self {
        match e {
            CoreError::NotFound(msg) => Self::not_found(msg),
            // Same status and same body as `NotFound`: the distinction is for
            // the Hybrid fall-through, never for the client. See the variant.
            CoreError::NotFoundWithheld(msg) => Self::not_found(msg),
            // RFC 0008 §4.4 — `503`, not `404`: the artifact exists, it is
            // simply not in this instance, and "try later" is the truth
            // ("later" being after the next bundle). The body carries the
            // facts a script needs to build that bundle.
            CoreError::ContentUnavailable { registry, key } => Self::with_status(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "{registry} does not hold '{key}' and this instance will not fetch it: \
                     it is air-gapped. The next bundle needs this coordinate."
                ),
            )
            .coded("content_unavailable")
            .with_details(serde_json::json!({
                "registry": registry,
                "coordinate": key,
                "bundle_hint": "batlehub-cli admin air-gap missing",
            })),
            CoreError::AccessDenied(msg) => Self::forbidden(msg),
            CoreError::UnknownRegistry(name) => {
                Self::bad_request(format!("unknown registry: {name}"))
            }
            CoreError::Conflict(msg) => Self::conflict(msg),
            CoreError::PayloadTooLarge(msg) => {
                Self::with_status(StatusCode::PAYLOAD_TOO_LARGE, msg)
            }
            CoreError::QuotaExceeded(msg) => Self::with_status(StatusCode::TOO_MANY_REQUESTS, msg),
            CoreError::InvalidVersion(msg) => Self::unprocessable(msg),
            CoreError::InvalidInput(msg) => Self::bad_request(msg),
            CoreError::Registry(msg) => Self::with_status(StatusCode::BAD_GATEWAY, msg),
            // Upstream served bytes that fail their advertised checksum (or
            // provided none when one is required) → 502, never the bad artifact.
            CoreError::IntegrityFailure(msg) => Self::with_status(StatusCode::BAD_GATEWAY, msg),
            // Dependency unavailability → 503 so load-balancers can retry elsewhere.
            CoreError::Storage(msg) | CoreError::Cache(msg) => {
                Self::with_status(StatusCode::SERVICE_UNAVAILABLE, msg)
            }
            CoreError::Database(msg) => Self::with_status(StatusCode::SERVICE_UNAVAILABLE, msg),
            // Reaching HTTP at all means a handler asked a registry type for
            // something its protocol has no answer for; 501 says that plainly
            // rather than dressing a capability gap up as a server fault.
            CoreError::NotSupported(msg) => Self::with_status(StatusCode::NOT_IMPLEMENTED, msg),
            other => {
                tracing::error!(error = %other, "internal error");
                Self::internal("internal server error")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_format() {
        let e = AppError::not_found("pkg missing");
        assert!(format!("{e}").contains("pkg missing"));
    }

    #[test]
    fn debug_format() {
        let e = AppError::forbidden("denied");
        assert!(format!("{e:?}").contains("403"));
    }

    #[test]
    fn internal_method() {
        let e = AppError::internal("oops");
        assert_eq!(e.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(e.message, "oops");
    }

    #[test]
    fn from_core_payload_too_large() {
        let e = AppError::from(CoreError::PayloadTooLarge("too big".into()));
        assert_eq!(e.status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn from_core_integrity_failure_maps_to_502() {
        let e = AppError::from(CoreError::IntegrityFailure("checksum mismatch".into()));
        assert_eq!(e.status, StatusCode::BAD_GATEWAY);
        assert_eq!(e.message, "checksum mismatch");
    }

    #[test]
    fn from_core_database_error_maps_to_503() {
        let e = AppError::from(CoreError::Database("db error".into()));
        assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn from_core_storage_error_maps_to_503() {
        let e = AppError::from(CoreError::Storage("backend down".into()));
        assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn from_core_cache_error_maps_to_503() {
        let e = AppError::from(CoreError::Cache("cache unavailable".into()));
        assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn from_core_not_found() {
        let e = AppError::from(CoreError::NotFound("missing".into()));
        assert_eq!(e.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn from_core_conflict() {
        let e = AppError::from(CoreError::Conflict("dup".into()));
        assert_eq!(e.status, StatusCode::CONFLICT);
    }

    #[test]
    fn not_found_status_is_404() {
        let e = AppError::not_found("missing resource");
        assert_eq!(e.status, StatusCode::NOT_FOUND);
        assert_eq!(e.message, "missing resource");
    }

    #[test]
    fn forbidden_status_is_403() {
        let e = AppError::forbidden("access denied");
        assert_eq!(e.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn bad_request_status_is_400() {
        let e = AppError::bad_request("invalid input");
        assert_eq!(e.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn conflict_status_is_409() {
        let e = AppError::conflict("already exists");
        assert_eq!(e.status, StatusCode::CONFLICT);
    }

    #[test]
    fn unprocessable_status_is_422() {
        let e = AppError::unprocessable("invalid version");
        assert_eq!(e.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn service_unavailable_status_is_503() {
        let e = AppError::service_unavailable("backend down");
        assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn error_response_uses_status_code() {
        use actix_web::ResponseError;
        let e = AppError::forbidden("you shall not pass");
        let resp = e.error_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn from_core_quota_exceeded_maps_to_429() {
        let e = AppError::from(CoreError::QuotaExceeded("over limit".into()));
        assert_eq!(e.status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn from_core_invalid_version_maps_to_422() {
        let e = AppError::from(CoreError::InvalidVersion("bad semver".into()));
        assert_eq!(e.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
}
