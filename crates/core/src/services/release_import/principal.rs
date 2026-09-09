//! Who an import publishes as (RFC 0021 §4.3).

use crate::entities::{Identity, Role};

/// The reserved provider prefix on a principal's groups.
///
/// `Identity.groups` are provider-namespaced strings, and a grant matches them
/// by named provider, by `*` for any provider, or as a bare string carrying
/// none. A principal whose groups were written bare would be indistinguishable
/// from a group an identity provider minted, so a config file could name `eng`
/// and collect whatever `eng` holds. This prefix is what makes a grant for an
/// import say out loud where its group came from.
///
/// One consequence, written down rather than discovered: a grant spelled
/// `group:*:eng` means *any provider's* `eng`, and that includes `config:eng`.
pub const CONFIG_GROUP_PREFIX: &str = "config:";

/// The identity a configured import publishes under.
///
/// A principal, not a credential: no token is minted, stored or sent, because
/// the server is not authenticating to itself — it is deciding what one of its
/// own actions may do. There is nothing to leak and nothing to rotate, and the
/// revocation is removing the block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPrincipal {
    user_id: String,
    groups: Vec<String>,
}

impl ImportPrincipal {
    /// The `user_id` reserved for the schedule's own identity.
    ///
    /// [`Identity::system`] is an admin by construction and says *the schedule
    /// did this* — true of a retention pass, misleading about a version that
    /// now sits in a team's namespace. An import may not borrow it.
    pub const RESERVED_USER_ID: &str = Identity::SYSTEM_USER_ID;

    /// `Err` with the reason when the principal is one the config may not name.
    /// The same checks run in `AppConfig::validate()`, so an operator meets them
    /// at load; this is the constructor that makes them unavoidable.
    pub fn new(user_id: impl Into<String>, groups: Vec<String>) -> Result<Self, String> {
        let user_id = user_id.into();
        if user_id.trim().is_empty() {
            return Err(
                "a publish with no publisher is a row the audit cannot answer for, \
                        and a quota nothing is charged against"
                    .to_owned(),
            );
        }
        if user_id == Self::RESERVED_USER_ID {
            return Err(format!(
                "'{}' is reserved for the schedule's own identity, which is an admin and \
                 means something else",
                Self::RESERVED_USER_ID
            ));
        }
        if let Some(bad) = groups
            .iter()
            .find(|g| !g.starts_with(CONFIG_GROUP_PREFIX) || g.len() == CONFIG_GROUP_PREFIX.len())
        {
            return Err(format!(
                "group '{bad}' must be written '{CONFIG_GROUP_PREFIX}<name>': a config file \
                 that could mint an identity provider's group string would collect that \
                 group's grants"
            ));
        }
        Ok(Self { user_id, groups })
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    pub fn groups(&self) -> &[String] {
        &self.groups
    }

    /// The identity every gate on the publish path reads.
    ///
    /// **Always [`Role::User`]**, and this is the one place that is decided, so
    /// it is a property of the type rather than a rule each call site
    /// re-applies. An admin skips `check_namespace_membership` outright, so an
    /// import that could be configured as one would publish into any namespace
    /// on the target — including one a team owns and did not offer — and
    /// nothing at request time would say so.
    pub fn identity(&self) -> Identity {
        Identity {
            user_id: Some(self.user_id.clone()),
            role: Role::User,
            auth_provider: Some("release-import".to_owned()),
            groups: self.groups.clone(),
        }
    }
}
