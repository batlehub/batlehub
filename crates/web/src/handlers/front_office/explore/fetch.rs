//! `POST /api/v1/explore/packages/{registry}/{name}/{version}/fetch`
//! (RFC 0007-bis §4.4).
//!
//! **This runs the download.** Not a warming task, not a special path — the same
//! `ProxyService::handle` a package manager's request runs, with the same
//! `ProxyRequest`, under the caller's own identity. That is the whole design,
//! and §5.3 is the section of the RFC most worth reading before changing
//! anything here.
//!
//! The single most important sentence: **this must not reuse the warming
//! service.** `WarmingService::warm_one_version_inner` calls
//! `client.fetch_artifact` directly, bypassing the rule engine, the release-age
//! gate, the block list, the licence gate, quota and the access audit — which is
//! fine, because its only caller is `require_admin`. Wiring a non-admin button
//! to it would hand every console user a way to pull bytes past every gate the
//! proxy has, and it would not look like a hole. It would look like reuse.
//!
//! What follows from being the download path, and is the point: the rules run,
//! integrity verification runs, quota is consumed, the access event is recorded
//! **with the caller as the actor**, and SBOM and README extraction run because
//! the artifact lands in storage through the ordinary path. The handler is short
//! by construction, and its length is the argument for the design.

use std::time::Instant;

use actix_web::post;
use futures::StreamExt;

use super::{web, AppError, Arc, AuthIdentity, Deserialize, IntoParams, Serialize, ToSchema};
use batlehub_core::{
    entities::{RegistryKind, Role},
    services::{LocalRegistryService, ProxyRequest, ProxyService},
};

use crate::RegistryMap;
use batlehub_core::entities::Action;

/// A rule refused the download. The body carries the rule's own reason.
pub const FETCH_DENIED: &str = "fetch.denied";

/// This instance already holds the version.
pub const FETCH_ALREADY_HELD: &str = "fetch.already-held";

/// The registry, or this build, does not offer the button.
pub const FETCH_UNSUPPORTED: &str = "fetch.unsupported";

/// The caller has no session. Pulling is an authenticated act (§4.1 revisited).
pub const FETCH_UNAUTHENTICATED: &str = "fetch.unauthenticated";

#[derive(Deserialize, IntoParams)]
pub struct FetchPath {
    pub registry: String,
    pub name: String,
    pub version: String,
}

#[derive(Serialize, ToSchema)]
pub struct FetchResponse {
    pub fetched: bool,
    /// What arrived, so the row can say what the wait bought.
    ///
    /// Measured against real upstreams, the median version is 0.57 MB and the
    /// largest in the sample was 41.7 MB (RFC 0007-bis §13.4) — a range wide
    /// enough that "done" on its own tells a reader nothing.
    pub size_bytes: u64,
    pub duration_ms: u64,
    /// The coordinate that was fetched, echoed so a client that fired several
    /// can match responses to rows.
    pub registry: String,
    pub name: String,
    pub version: String,
}

/// Fetch one version from upstream, as the caller.
#[utoipa::path(
    post,
    path = "/api/v1/explore/packages/{registry}/{name}/{version}/fetch",
    tag = "explore",
    params(FetchPath),
    responses(
        (status = 200, description = "The version was fetched and is now held", body = FetchResponse),
        (status = 401, description = "No session; pulling a version requires one"),
        (status = 403, description = "A rule refused it; the body carries the rule's own reason"),
        (status = 404, description = "No such registry, package or version upstream"),
        (status = 409, description = "This instance already holds the version"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/explore/packages/{registry}/{name}/{version}/fetch")]
pub async fn explore_fetch_version(
    path: web::Path<FetchPath>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    proxy_svc: web::Data<Arc<ProxyService>>,
    registry_map: web::Data<RegistryMap>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<web::Json<FetchResponse>, AppError> {
    let FetchPath {
        registry,
        name,
        version,
    } = path.into_inner();

    // **A session is required to pull.**
    //
    // This endpoint used to admit an anonymous caller, on the reasoning above:
    // the fetch downloads exactly what that caller could already pull through
    // the proxy with `curl`, so it grants no read they did not have. That is
    // true about *reading* and it is not the whole question. A fetch is a write
    // to this instance — it fills the cache, spends the upstream's bandwidth and
    // ours, extracts an SBOM, and lands a row in the audit log — and it is the
    // one button in the console that does so on a page an unauthenticated reader
    // can open. Measured against the running instance before this check existed,
    // an anonymous `POST …/strip-ansi/7.2.0/fetch` came back `409 already-held`:
    // it had passed visibility, the operator's switch and the kind check, and
    // was stopped only by the artifact happening to be there already.
    //
    // The proxy path is unchanged and still serves whoever the operator's
    // `anonymous` policy allows. What is refused here is *causing this instance
    // to go and get something* without saying who you are — the audit row for
    // which would read `anonymous`.
    //
    // **Before** the visibility check, not after it. Ordered the other way this
    // status pair is an existence oracle: `check_visibility` answers `Public`
    // for any name with no `local_packages` row, so an unauthenticated
    // `POST …/{registry}/{name}/1.0.0/fetch` came back `404` exactly when
    // `name` is a published `internal`/`team` package and `401` otherwise —
    // which enumerates private package names to a caller with no session at
    // all, the disclosure the `404`-not-`403` rule exists to prevent. Refusing
    // first leaks nothing: an anonymous caller gets `401` for every coordinate,
    // existing or not, and a signed-in one still meets the visibility check
    // below and its uniform `404`.
    if identity.0.role == Role::Anonymous {
        return Err(AppError::unauthorized(
            "fetching a version requires a signed-in session".to_owned(),
        )
        .coded(FETCH_UNAUTHENTICATED));
    }

    // `404` rather than `403` on a visibility refusal, exactly as the detail and
    // README endpoints do: a `403` confirms the package exists, which is the
    // fact a non-public package is trying not to disclose.
    if local_svc
        .check_visibility(&registry, &name, &identity)
        .await
        .is_err()
    {
        return Err(AppError::not_found(format!(
            "package '{name}' not found in registry '{registry}'"
        )));
    }

    // Gate: `rbac.explore` on the registry — the same gate `explore_detail`,
    // `explore_readme` and the image endpoint apply, and the one this endpoint
    // was missing. Without it the console's own API is a door around it: a
    // signed-in non-admin who gets `404` from
    // `GET …/explore/packages/{registry}/{name}` still learns from this
    // endpoint's `409 fetch.already-held` exactly which versions the instance
    // holds, and can drive it into fetching the ones it does not — into a
    // registry they are not allowed to browse at all.
    //
    // `404` and not `403`, exactly as the visibility check above: denied and
    // absent look identical from outside. **After** the anonymous refusal, not
    // RFC 0015 §4.2 — `catalogue:browse`, resolved from grants, in place of
    // `AccessConfig`'s explore sets. §10 rule 2's conjunction reproduces those
    // sets exactly (§13.5 measured the naive reading at 19 disagreements before
    // it was corrected), so this is a substitution between two computations
    // already known to agree — not a new policy.
    //
    // before it: `explore.anonymous` is commonly off, and a `404` here would
    // otherwise answer an unauthenticated caller who should be told to sign in.
    // Neither answer leaks anything — an anonymous caller gets `401` whatever
    // the registry, and a signed-in one gets a `404` that says nothing about
    // whether the package exists.
    if !batlehub_core::services::authz::browsable_registries(&hot, &identity)
        .await
        .contains(&registry)
    {
        tracing::debug!(
            registry = %registry, package = %name,
            "explore fetch: registry not browsable by this caller"
        );
        return Err(AppError::not_found(format!(
            "package '{name}' not found in registry '{registry}'"
        )));
    }

    // The operator's switch. It admits nothing — the fetch is a download the
    // caller could already run with `curl` — so it exists for the operator who
    // wants the console strictly read-only, which is a legitimate posture and
    // not one the software should have to guess at (§4.1).
    let (console_fetch, kind) = {
        let hot = local_svc.hot.read().await;
        let enabled = hot
            .console_fetch
            .get(&registry)
            .copied()
            .unwrap_or(batlehub_core::services::DEFAULT_CONSOLE_FETCH);
        let kind = registry_map
            .type_of(&registry)
            .and_then(|t| t.parse::<RegistryKind>().ok());
        (enabled, kind)
    };
    if !console_fetch {
        return Err(AppError::forbidden(format!(
            "fetching from the console is disabled for registry '{registry}'"
        ))
        .coded(FETCH_UNSUPPORTED));
    }

    // "Fetch this version" has to name one thing. Maven's artifact is a set of
    // files and a Terraform provider needs an OS and an architecture, so those
    // answer with the reason rather than a disabled button with no explanation.
    // No type means no such registry here. `404` rather than a `400` about an
    // unknown kind: the caller named something that does not exist, and saying
    // so in the vocabulary of registry *types* would be an answer to a question
    // they did not ask.
    let Some(kind) = kind else {
        return Err(AppError::not_found(format!(
            "registry '{registry}' not found"
        )));
    };
    // Validated at the edge, before anything is built from it. The two deeper
    // funnels do run — `ProxyService::handle` calls `validate_coordinate` and
    // `ensure_safe_key` guards the storage backends — but the `storage.exists()`
    // below builds a key from these bytes and asks the backend about it *first*,
    // which is the handler-builds-a-key case CLAUDE.md says must validate here
    // for a clean `400` rather than lean on the guards behind it.
    batlehub_core::services::validate_coordinate(&name, &version, None).map_err(AppError::from)?;

    // The coordinate comes from the kind's own table rather than being built
    // here, because it has to be *byte-identical* to the one the package
    // manager's download builds — sub-coordinate and normalisation included.
    // Built here, it was `{registry}/{name}/{version}` for every kind: the
    // fetch stored `artifact:npm1/lodash/4.17.21` while `npm install` reads
    // `artifact:npm1/lodash/4.17.21/tarball`, so the button reported a success
    // that filled a slot nothing reads and left the next install going upstream
    // — and the `409` check below could never match a genuinely cached version.
    let Some(package_id) = kind.fetch_coordinate(&registry, &name, &version) else {
        // `fetch_coordinate` is `None` exactly when the kind cannot name one
        // artifact for a version, which is when `fetchable_by_version` carries
        // the reason to show.
        let reason = kind
            .fetchable_by_version()
            .reason()
            .unwrap_or("this registry type cannot fetch a version by coordinate");
        return Err(AppError::bad_request(reason.to_owned()).coded(FETCH_UNSUPPORTED));
    };

    // Already here? Then this would be a cache read dressed as a fetch, and the
    // row the reader is looking at is out of date rather than upstream-only.
    // `409` says which, so the console can refresh instead of reporting a
    // download that did not happen.
    //
    // The **proxy** key, not the local-publish one: they describe different
    // halves of the same catalogue, and asking the wrong one is a question
    // always answered "no".
    if proxy_svc
        .storage
        .exists(&batlehub_core::services::proxy::proxy_artifact_key(
            &package_id,
        ))
        .await
        .unwrap_or(false)
    {
        return Err(
            AppError::conflict(format!("this instance already holds {name} {version}"))
                .coded(FETCH_ALREADY_HELD),
        );
    }

    // From here it is the download. `source:read` rather than `releases:read`:
    // the button pulls **bytes**, and the permission it needs is the one a
    // package manager's download needs, not the one a metadata read needs.
    let started = Instant::now();
    let response = proxy_svc
        .handle(ProxyRequest {
            package_id,
            identity: identity.0.clone(),
            action: Action::SourceRead,
            ip_address: None,
            user_agent: None,
        })
        .await
        .map_err(AppError::from)?;

    // The rule's own reason, which is the same string the download would
    // have given — so the console shows the operator *why*, and the
    // `/tools/access-check` page it already links to explains the same
    // verdict (§4.4). A security verdict's headers are not needed here: the
    // console reads the verdict endpoint for those.
    let stream = response
        .into_stream()
        .map_err(|(reason, _)| AppError::forbidden(reason).coded(FETCH_DENIED))?;

    // Drained, not forwarded: this wants the side effect, not the bytes.
    // Everything that makes a download safe has already applied because it *is*
    // a download; the only difference is that nothing writes the response to a
    // socket.
    let mut stream = stream;
    let mut size_bytes: u64 = 0;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => size_bytes += bytes.len() as u64,
            // A break mid-stream means the artifact did not land. Reported as an
            // error rather than as a short success, because the row would
            // otherwise claim to be held when it is not.
            Err(e) => {
                return Err(AppError::from(batlehub_core::error::CoreError::Registry(
                    format!("fetching {name} {version}: {e}"),
                )))
            }
        }
    }

    Ok(web::Json(FetchResponse {
        fetched: true,
        size_bytes,
        duration_ms: started.elapsed().as_millis() as u64,
        registry,
        name,
        version,
    }))
}
