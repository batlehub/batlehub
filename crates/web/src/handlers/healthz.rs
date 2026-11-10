use std::sync::Arc;

use actix_web::{get, http::StatusCode, web, HttpResponse, Responder};
use serde::Serialize;
use sqlx::PgPool;

use batlehub_core::services::ProxyService;

const STATUS_OK: &str = "ok";
const STATUS_ERROR: &str = "error";
const STATUS_UNCONFIGURED: &str = "unconfigured";
/// The key the storage probe looks up; it never exists.
const HEALTHZ_PROBE_KEY: &str = "staging:healthz/__healthz__";

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    db: &'static str,
    storage: &'static str,
    /// The running server's version (`CARGO_PKG_VERSION`), so operators and
    /// the UI can surface what's deployed without a separate authenticated call.
    version: &'static str,
}

/// Liveness probe — answers `200` as long as the process is running and the
/// HTTP server is accepting requests. Unauthenticated, no I/O.
///
/// Deliberately checks **nothing** beyond that. A liveness probe's only correct
/// remedy is "restart the pod", and restarting fixes neither an unreachable
/// database nor an unreachable object store — pointing liveness at [`healthz`]
/// would turn a Postgres blip into a CrashLoopBackOff across every replica.
/// Readiness is what should react to dependency health, so that is where
/// [`healthz`] belongs.
#[get("/livez")]
pub async fn livez() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Infrastructure health check — verifies DB and storage connectivity, and
/// reports the running version. Unauthenticated; intended for the Kubernetes
/// **readiness** probe (see [`livez`] for liveness) as well as UI/CLI version
/// display.
///
/// Returns `503` when a dependency is unreachable, which drops the pod out of
/// the Service endpoints without restarting it.
#[get("/healthz")]
pub async fn healthz(
    pool: Option<web::Data<PgPool>>,
    proxy_svc: web::Data<Arc<ProxyService>>,
) -> impl Responder {
    let db: &'static str = match pool {
        Some(p) => {
            let result = batlehub_adapters::db::timed_query(
                "healthz_ping",
                sqlx::query("SELECT 1").execute(p.get_ref()),
            )
            .await;
            match result {
                Ok(_) => STATUS_OK,
                Err(e) => {
                    tracing::warn!(error = %e, "healthz: database check failed");
                    STATUS_ERROR
                }
            }
        }
        None => STATUS_UNCONFIGURED,
    };

    // A staging key goes straight to the backend: through a storage router a
    // plain key would first be resolved in the dedup tables, two more
    // statements that probe the database again rather than the object store
    // (TD-0004 §4).
    let storage: &'static str = match proxy_svc.storage.exists(HEALTHZ_PROBE_KEY).await {
        Ok(_) => STATUS_OK,
        Err(e) => {
            tracing::warn!(error = %e, "healthz: storage check failed");
            STATUS_ERROR
        }
    };

    let ok = db != STATUS_ERROR && storage != STATUS_ERROR;
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    HttpResponse::build(status).json(HealthResponse {
        ok,
        db,
        storage,
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test, App};

    #[actix_web::test]
    async fn livez_is_unauthenticated_and_always_ok() {
        let app = test::init_service(App::new().service(livez)).await;
        let req = test::TestRequest::get().uri("/livez").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["ok"], serde_json::json!(true));
        assert_eq!(
            body["version"],
            serde_json::json!(env!("CARGO_PKG_VERSION"))
        );
    }

    /// `livez` takes no `web::Data`, so it cannot 500 on a half-wired app the
    /// way a dependency-checking handler would. Guards against someone later
    /// adding a DB check here and reintroducing restart-on-outage.
    #[actix_web::test]
    async fn livez_needs_no_app_data() {
        let app = test::init_service(App::new().service(livez)).await;
        for _ in 0..3 {
            let req = test::TestRequest::get().uri("/livez").to_request();
            assert_eq!(test::call_service(&app, req).await.status(), StatusCode::OK);
        }
    }
}
