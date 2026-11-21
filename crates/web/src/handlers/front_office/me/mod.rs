//! `/api/v1/me/*` — what this API can tell a caller about themselves.
//!
//! Every endpoint here is scoped to the token that made the request and takes
//! no `user_id` parameter of any kind. That is deliberate and load-bearing: an
//! endpoint that accepts a subject is an endpoint one missing authorisation
//! check away from serving someone else's data, which is why RFC 0004 §7 keeps
//! these paths and the admin ones separate rather than widening the admin
//! handlers to two authorisation rules.
//!
//! The scoping itself lives further down — in `PackageRepository::list_own_downloads`
//! and `OwnershipPort::list_owned_by` — so a future caller cannot assemble the
//! query without it (RFC 0004 §6.2).

pub mod advisories;
pub mod downloads;
pub mod identity;
pub mod limits;
pub mod quota;

pub use advisories::my_advisories;
pub use downloads::my_downloads;
pub use identity::{me, MeResponse};
pub use limits::my_limits;
pub use quota::my_quota;

use crate::{error::AppError, extractors::AuthIdentity};

/// The caller's user id, or `401` if the request is anonymous.
///
/// Every `me` endpoint needs a subject to scope by, and an anonymous caller has
/// none — there is no history, quota or ownership attached to "nobody". Failing
/// here rather than returning an empty body keeps that distinction visible: an
/// empty result means "you have none", not "we could not tell who you are".
pub(crate) fn require_user_id(identity: &AuthIdentity) -> Result<&str, AppError> {
    identity
        .0
        .user_id
        .as_deref()
        .ok_or_else(|| AppError::unauthorized("authentication required"))
}
