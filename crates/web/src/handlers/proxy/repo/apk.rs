//! Alpine `apk`: a path tree whose `.apk` files carry a coordinate.
//!
//! Separate from [`super::repo_get`] — which serves `deb`, `rpm` and `pacman` —
//! because of one difference that runs through everything: those three address
//! every file as the synthetic `repo`/`_` package, and `apk` does not. A request
//! ending in `.apk` is split into a real `name` and `version` before the
//! `PackageId` is built, which is what gives this kind a block list, an age
//! gate, a row in explore and a coordinate in the statistics — and what the
//! other three cannot have, because a `.deb` file name is not a reliable
//! coordinate and an `.rpm`'s is not one at all (RFC 0026 §4.3).
//!
//! **The path is still the cache key.** `apk` stays a path-addressed kind:
//! `path_allow`, `warm_paths` and `local:{registry}/{path}` storage all work as
//! they do for the siblings. The identity is what the *admin* acts on; the path
//! is what the client asks for. Three URLs naming one file are three cache
//! entries with one dedup reference and one row per coordinate.

use std::sync::Arc;

use actix_web::{get, web, HttpResponse, Responder};

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{Action, PackageId},
    services::{
        apk::apk_coordinate, proxy::proxy_artifact_key, LocalRegistryService, ProxyService,
    },
};

use crate::handlers::proxy::common::{
    collect_storage_stream, proxy_stream, require_registry_type, LISTING_HEADER,
    LISTING_HELD_HEADER,
};
use crate::handlers::schemas::ArtifactBytes;
use crate::{
    error::AppError, extractors::AuthIdentity, ApkSignerMap, RegistryMap, RegistryModeMap,
};

use super::repo_storage_key;

/// Everything an Alpine mirror serves is `application/octet-stream` — the
/// index and the packages alike. Matching the upstream exactly matters here:
/// apk reads the bytes and never the type, but a client-side proxy or a
/// corporate middlebox that sees `application/gzip` on an `APKINDEX.tar.gz` may
/// transparently decompress it, which breaks the signature over the compressed
/// bytes.
const APK_CONTENT_TYPE: &str = "application/octet-stream";

/// The file name every apk resolves a repository through.
const INDEX_FILE: &str = "APKINDEX.tar.gz";

/// The reserved prefix under `…/apk/` that serves the signing key.
///
/// A real Alpine tree's content always begins with a branch name, and
/// `dl-cdn.alpinelinux.org/alpine/keys/` is a `404` — its root holds only
/// branches, `MIRRORS.txt` and `last-updated` — so one reserved segment costs
/// nothing there. It is still a trade, and the registry page states it: in
/// `hybrid` mode this shadows any upstream path spelled `keys/…`.
const KEYS_PREFIX: &str = "keys/";

/// The `PackageId` a request under `…/apk/{path}` resolves to.
///
/// A `.apk` file name yields a real coordinate; anything else — the index, a
/// directory, the key route — is the synthetic `repo`/`_` the path family uses,
/// because nothing blocks a listing.
///
/// A `.apk` whose name carries no `-r<digits>` release token is an error at the
/// edge rather than a guess: the alternatives are inventing a version (which
/// would let a block be bypassed by misspelling a file name) or treating it as
/// a listing (which would let it bypass the block list entirely).
pub(super) fn apk_package_id(registry: &str, path: &str) -> Result<PackageId, AppError> {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    if !file_name.ends_with(".apk") {
        return Ok(PackageId::new(registry, "repo", "_").with_artifact(path));
    }

    let (name, version) = apk_coordinate(file_name).ok_or_else(|| {
        AppError::bad_request(format!(
            "'{file_name}' is not a valid apk file name: expected \
             {{name}}-{{pkgver}}-r{{N}}.apk"
        ))
    })?;

    // The coordinate reaches a storage key and a rule lookup, so it is
    // validated at the edge for a clean 400 — the deeper funnels
    // (`validate_coordinate` in the proxy read, `ensure_safe_key` in the
    // backends) are defence in depth, not the first line.
    batlehub_core::services::validate_package_name(name).map_err(AppError::from)?;
    batlehub_core::services::validate_path_safe("version", version).map_err(AppError::from)?;

    Ok(PackageId::new(registry, name, version).with_artifact(path))
}

/// Serve a file from an Alpine repository (`/proxy/{registry}/apk/{path}`).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/apk/{path}",
    tag = "proxy/apk",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("path" = String, Path, description = "Repository file path (e.g. v3.22/main/x86_64/APKINDEX.tar.gz, v3.22/main/x86_64/busybox-1.37.0-r20.apk)"),
    ),
    responses(
        (status = 200, description = "Repository file, or the signing key under keys/{name}", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 400, description = "Malformed .apk file name"),
        (status = 403, description = "Access denied, or the version is blocked"),
        (status = 404, description = "Not found or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/apk/{path:.*}")]
pub async fn apk_get(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    signers: web::Data<ApkSignerMap>,
) -> Result<impl Responder, AppError> {
    let (registry, file_path) = path.into_inner();
    require_registry_type(&registry, "apk", &map)?;
    // Edge validation on the whole path, before a storage key is built from it.
    batlehub_core::services::validate_path_safe("path", &file_path).map_err(AppError::from)?;

    // The public key, served live — before anything has been published, so a
    // client can install it and configure the repository first. Compared for
    // equality, never matched: the name is the one thing apk looks the key up
    // by, and a near miss is an untrusted index rather than an error the user
    // sees. Any other name under `keys/` is a 404, so a typo is caught at
    // `curl` and not at `apk update`.
    if let Some(requested) = file_path.strip_prefix(KEYS_PREFIX) {
        let Some(signer) = signers.get(&registry) else {
            return Err(AppError::not_found(
                "this registry has no signing key".to_owned(),
            ));
        };
        // Current *or* retired: a machine that has not been given the new key
        // file yet is exactly the machine that needs to fetch the old one, and
        // refusing it here would make rotation the thing that breaks a fleet
        // (RFC 0026 §11 decision 9).
        let Some(served) = signer.by_name(requested) else {
            return Err(AppError::not_found(format!(
                "no key named '{requested}' — this registry signs with {}",
                signer
                    .all()
                    .map(|s| format!("'{}'", s.key_name()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        };
        return Ok(HttpResponse::Ok()
            .content_type("application/x-pem-file")
            .body(served.public_key_pem().map_err(AppError::from)?));
    }

    let pkg = apk_package_id(&registry, &file_path)?;
    let mode = mode_map.get(&registry);

    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        // The same authorization the proxy fall-through runs through the rule
        // chain, so a local read and a proxied read agree. Keyed on the real
        // coordinate when there is one, which is what makes a grant on an
        // `apk` package mean something.
        svc.authorize_read(&pkg, &identity.0, Action::ReleasesRead)
            .await
            .map_err(AppError::from)?;

        let key = repo_storage_key(&registry, &file_path);
        match local_svc.storage.retrieve(&key).await {
            Ok(Some(artifact)) => {
                let buf = collect_storage_stream(artifact.stream).await?;
                return Ok(HttpResponse::Ok().content_type(APK_CONTENT_TYPE).body(buf));
            }
            Ok(None) if mode == RegistryMode::Local => {
                return Err(AppError::not_found(format!(
                    "{file_path} not found in registry"
                )));
            }
            Ok(None) => { /* hybrid: fall through to the upstream mirror */ }
            Err(e) if mode == RegistryMode::Hybrid => {
                tracing::warn!("local storage error, falling back to proxy: {e}");
            }
            Err(e) => return Err(AppError::from(e)),
        }
    }

    // RFC 0026 §6.10 — the air gap. An `apk` repository resolves through
    // `APKINDEX.tar.gz` and has no second way to ask: with no index an
    // `apk update` fails before any package is named. This kind is the one OS
    // member that can answer anyway, because a *composed* index is this
    // instance's own document to sign (decision 8).
    //
    // The order is the synthesis rule of RFC 0008-bis §5.1, not a preference:
    // a real index this instance holds is served as it always was, and the
    // composition fires only on the miss that would otherwise be the `503`.
    if svc.synthesises_listings().await
        && file_path
            .rsplit('/')
            .next()
            .is_some_and(|f| f == INDEX_FILE)
        && !svc
            .storage
            .exists(&proxy_artifact_key(&pkg))
            .await
            .unwrap_or(false)
    {
        // The same authorization the fall-through below runs; a composed
        // listing is not a way around the rule chain.
        svc.authorize_read(&pkg, &identity.0, Action::ReleasesRead)
            .await
            .map_err(AppError::from)?;
        if let Some((body, held)) = compose_held_index(
            &svc,
            &registry,
            &file_path,
            signers.get(&registry).as_deref(),
        )
        .await?
        {
            tracing::info!(
                registry = %registry,
                path = %file_path,
                held,
                "air gap: APKINDEX composed from the held set and signed"
            );
            return Ok(HttpResponse::Ok()
                .content_type(APK_CONTENT_TYPE)
                .insert_header((LISTING_HEADER, "synthesised"))
                .insert_header((LISTING_HELD_HEADER, held.to_string()))
                .body(body));
        }
    }

    proxy_stream(
        svc,
        pkg,
        identity,
        Action::ReleasesRead,
        Some(APK_CONTENT_TYPE),
    )
    .await
}

/// The `APKINDEX.tar.gz` an air-gapped instance composes over the `.apk` files
/// it holds under the requested directory, signed with this registry's key.
///
/// `None` — and so the `503` of RFC 0008 — when nothing is held there, or when
/// the registry has no key and has not said `apk_unsigned = true`: an index no
/// client can verify is not an answer, it is a different failure with a
/// friendlier status.
///
/// The bytes are read and parsed rather than taken from a stored entry on
/// purpose. `C:` is a digest of the package's own control member, so an index
/// composed from the archives *this instance holds* cannot name a version it
/// cannot serve or an identity that does not match the bytes — which is the
/// property RFC 0008-bis §5.2 wants and a snapshot taken on the connected side
/// does not have.
async fn compose_held_index(
    svc: &Arc<ProxyService>,
    registry: &str,
    index_path: &str,
    signer: Option<&batlehub_adapters::repo::ApkSigner>,
) -> Result<Option<(Vec<u8>, usize)>, AppError> {
    let Some((dir, _)) = index_path.rsplit_once('/') else {
        // An index at the root of the tree names no architecture; a real
        // Alpine repository has none there either.
        return Ok(None);
    };

    // No key, no composition. `regenerate_apk` may emit an unsigned index
    // because config validation refused the silent case for a registry that
    // hosts locally; a *proxy* registry is not asked for a key at all, so
    // silence here is silence and not a choice on the record. An index no
    // client can verify would turn a `503` into an `apk update` that fails
    // further along, which is a worse answer and not a truer one.
    let Some(signer) = signer else {
        tracing::warn!(
            registry,
            "air gap: no APKINDEX composed — this registry has no [registries.apk_signing] key, \
             and every shipping apk refuses an index it cannot verify"
        );
        return Ok(None);
    };

    let blocked: std::collections::HashSet<(String, String)> = svc
        .repo
        .blocked_in_registry(registry)
        .await
        .map_err(AppError::from)?
        .into_iter()
        .collect();

    let mut rows: Vec<(String, String, String)> = svc
        .held_artifacts(registry)
        .await
        .into_iter()
        .filter_map(|(name, held)| {
            let artifact = held.artifact?;
            let (held_dir, file) = artifact.rsplit_once('/')?;
            (held_dir == dir && file.ends_with(".apk")).then_some((name, held.version, artifact))
        })
        .filter(|(name, version, _)| !blocked.contains(&(name.clone(), version.clone())))
        .collect();
    // Deterministic, for the same reason `regenerate_apk` sorts: the same held
    // set has to produce the same bytes and so the same signature.
    rows.sort();
    rows.dedup();
    if rows.is_empty() {
        return Ok(None);
    }

    let mut entries = Vec::with_capacity(rows.len());
    for (name, version, artifact) in &rows {
        let id = PackageId::new(registry, name, version).with_artifact(artifact);
        let key = proxy_artifact_key(&id);
        let bytes = match svc.storage.retrieve(&key).await {
            Ok(Some(artifact)) => collect_storage_stream(artifact.stream).await?,
            Ok(None) => {
                // The row and the bytes disagree — the artifact-meta join said
                // it is held and the store does not have it. Dropping it is the
                // only honest move: a listing must not name what the next
                // request cannot serve.
                tracing::warn!(registry, key = %key, "air gap: held row with no bytes; not listed");
                continue;
            }
            Err(e) => {
                tracing::warn!(registry, key = %key, error = %e,
                    "air gap: could not read a held package; not listed");
                continue;
            }
        };
        match batlehub_adapters::repo::apk::parse_apk(&bytes) {
            Ok(parsed) => entries.push(batlehub_core::services::apk::index_entry(
                &parsed.info,
                &parsed.identity,
                parsed.size,
            )),
            Err(e) => tracing::warn!(registry, key = %key, error = %e,
                "air gap: a held .apk did not parse; not listed"),
        }
    }
    if entries.is_empty() {
        return Ok(None);
    }

    let held = entries.len();
    let body = batlehub_core::services::apk::render_index(&entries);
    let description = format!("{registry} {dir} (composed from the held set)\n");
    let data_member = batlehub_adapters::repo::apk::generate_index(&body, &description)
        .map_err(|e| AppError::internal(e.to_string()))?;
    let file = batlehub_adapters::repo::apk::sign_index(&data_member, signer)
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Some((file, held)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(path: &str) -> PackageId {
        apk_package_id("alpine", path).expect("valid")
    }

    #[test]
    fn a_package_gets_its_real_coordinate() {
        let pkg = id("v3.22/main/x86_64/busybox-1.37.0-r20.apk");
        assert_eq!(pkg.name, "busybox");
        assert_eq!(pkg.version, "1.37.0-r20");
        assert_eq!(
            pkg.artifact.as_deref(),
            Some("v3.22/main/x86_64/busybox-1.37.0-r20.apk"),
            "the path stays the cache key"
        );
    }

    #[test]
    fn a_name_with_dashes_splits_from_the_right() {
        let pkg = id("v3.22/main/x86_64/py3-requests-2.32.4-r0.apk");
        assert_eq!(pkg.name, "py3-requests");
        assert_eq!(pkg.version, "2.32.4-r0");
    }

    /// The real adversarial name in `v3.22/main`: its own name ends in
    /// `-r<digits>`.
    #[test]
    fn a_name_ending_in_a_release_suffix_still_splits() {
        let pkg = id("v3.22/main/x86_64/linux-firmware-r128-20250613-r0.apk");
        assert_eq!(pkg.name, "linux-firmware-r128");
        assert_eq!(pkg.version, "20250613-r0");
    }

    /// Nothing blocks a listing, so the index is the synthetic package — the
    /// same coordinate `deb`, `rpm` and `pacman` use for every file.
    #[test]
    fn the_index_is_the_synthetic_package() {
        let pkg = id("v3.22/main/x86_64/APKINDEX.tar.gz");
        assert_eq!(pkg.name, "repo");
        assert_eq!(pkg.version, "_");
        assert_eq!(
            pkg.artifact.as_deref(),
            Some("v3.22/main/x86_64/APKINDEX.tar.gz")
        );
    }

    /// A malformed `.apk` name is a 400, never an invented version: guessing
    /// would make a block bypassable by misspelling a file name.
    #[test]
    fn a_malformed_package_name_is_rejected() {
        for path in [
            "v3.22/main/x86_64/x-1.0.apk",
            "v3.22/main/x86_64/noversion.apk",
            "v3.22/main/x86_64/thing-1.0-rc1.apk",
        ] {
            assert!(
                apk_package_id("alpine", path).is_err(),
                "expected a 400 for {path}"
            );
        }
    }

    #[test]
    fn a_traversal_version_is_rejected() {
        assert!(apk_package_id("alpine", "v3.22/main/x86_64/x-..%2f..-r0.apk").is_err());
    }
}
