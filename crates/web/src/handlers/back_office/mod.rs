pub mod access_check;
pub mod audit;
pub mod authz_explain;
pub mod authz_shadow;
pub mod bulk;
pub mod config;
pub mod explore;
pub mod exposure;
pub mod flags;
pub mod governance;
pub mod health;
pub mod notification;
pub mod ops;
pub mod packages;
pub mod retention;
pub mod sbom;
pub mod stats;
pub mod stats_history;
pub mod tombstones;
pub mod visibility;

use std::time::{SystemTime, UNIX_EPOCH};

use crate::{error::AppError, extractors::AuthIdentity};
use batlehub_core::entities::Role;

// `require_admin` is **deleted**, not deprecated.
//
// It guarded 98 call sites across 28 files — the whole of RFC 0015 §4.2's
// deferred *"control surfaces stay `role:admin`"* — and every one of them now
// asks [`require_verb`] instead. Leaving the helper behind would leave the
// second authorization model behind with it, one `use` away from the next
// handler that needs a gate and reaches for the familiar name.
//
// A role still decides all of this. It decides it **inside** the engine, where
// `role:admin` is one of §4.3's five subject forms and §10 rule 5 grants it every
// control verb — which is why an administrator's reach is unchanged and the
// verbs are now delegable one at a time.

/// A control-surface verb, resolved by the engine (RFC 0015 §4.2).
///
/// This is what replaces [`require_admin`] on the endpoints §4.2 deferred. The
/// difference is not the answer — §10 rule 5 grants every control verb to
/// `role:admin`, so an administrator reaches exactly what they reached before —
/// it is **who decides**. A handler asserting `identity.role != Admin` is a
/// second authorization model beside the engine, and one that silently overrides
/// it: a grant naming somebody else resolved to *allow* and was then refused by
/// a role check the operator never wrote.
///
/// `registry` is `Some` for a control endpoint scoped to one registry and `None`
/// for the dozen that name none — config, health, the notification wiring, the
/// block lists, the diagnostics. Those resolve against the instance tier, which
/// exists for exactly this reason (§4.1).
///
/// Fails closed on a registry with no configured hierarchy; see
/// [`authorize_control`](batlehub_core::services::authz::authorize_control) for
/// why that differs from the read path.
pub(crate) async fn require_verb(
    identity: &AuthIdentity,
    action: batlehub_core::entities::Action,
    registry: Option<&str>,
    hot: &batlehub_core::services::hot_config::HotConfigLock,
) -> Result<(), AppError> {
    batlehub_core::services::authz::authorize_control(hot, registry, &identity.0, action)
        .await
        .map_err(|_| {
            AppError::forbidden(format!("this endpoint requires the '{action}' permission"))
        })
}

/// Reject anonymous callers, without requiring the stronger `require_admin`
/// threshold. Shared by handlers that only need "authenticated at all" —
/// e.g. deb/rpm repo publish, per-artifact SBOM reads.
pub(crate) fn require_authenticated(identity: &AuthIdentity) -> Result<(), AppError> {
    if identity.role == Role::Anonymous {
        Err(AppError::forbidden("authentication required"))
    } else {
        Ok(())
    }
}

pub(super) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
