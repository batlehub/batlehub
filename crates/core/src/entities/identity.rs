use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Anonymous,
    User,
    Admin,
}

impl Role {
    /// Whether this role clears a gate's `bypass_roles`: the least-privileged
    /// listed role is the floor, so listing `user` lets an admin through too.
    /// An empty list bypasses nobody. Every gate reads its list this way.
    pub fn clears_bypass(&self, bypass_roles: &[Role]) -> bool {
        bypass_roles.iter().min().is_some_and(|floor| self >= floor)
    }
}

/// Whether a static token's configured value is an argon2 PHC hash. The one
/// rule both the verifier (which compares anything else in clear) and the
/// config's plain-token warning read, so they cannot disagree.
pub fn is_argon2_hash(s: &str) -> bool {
    ["$argon2id$", "$argon2i$", "$argon2d$"]
        .iter()
        .any(|p| s.starts_with(p))
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Role::Anonymous => write!(f, "anonymous"),
            Role::User => write!(f, "user"),
            Role::Admin => write!(f, "admin"),
        }
    }
}

impl std::str::FromStr for Role {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "anonymous" => Ok(Role::Anonymous),
            "user" => Ok(Role::User),
            "admin" => Ok(Role::Admin),
            other => Err(format!("unknown role: '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub user_id: Option<String>,
    pub role: Role,
    pub auth_provider: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
}

impl Identity {
    /// The `user_id` a scheduled task acts under.
    ///
    /// A reserved name rather than a `None`: an audit row with no subject reads
    /// as "we do not know who did this", which is the wrong thing to say about
    /// an action the server took on its own schedule. `"system"` is the name the
    /// banner already uses for the same idea.
    pub const SYSTEM_USER_ID: &str = "system";

    pub fn anonymous() -> Self {
        Self {
            user_id: None,
            role: Role::Anonymous,
            auth_provider: None,
            groups: vec![],
        }
    }

    /// The identity a scheduled background task runs as.
    ///
    /// `Role::Admin` because a periodic sweep answers to the operator who
    /// enabled it in the config, not to a request — there is no principal to
    /// narrow it to and nothing to ask. It is deliberately **not reachable from
    /// a request**: nothing parses `"system"` out of a token, so this is only
    /// ever constructed by the process itself.
    ///
    /// Its whole purpose is the trail: `user_id = "system"` is what separates
    /// "the schedule collected this" from "an operator did", which no other
    /// field of an `AccessEvent` could say.
    pub fn system() -> Self {
        Self {
            user_id: Some(Self::SYSTEM_USER_ID.to_owned()),
            role: Role::Admin,
            auth_provider: None,
            groups: vec![],
        }
    }

    /// `auth_provider` of a personal access token: `user-token:<minting provider>`.
    ///
    /// The prefix keeps a PAT from passing for an interactive session (the token
    /// endpoints refuse anything that is not a configured OIDC name), and the
    /// suffix keeps the owner's provider, which `user:` grants are scoped by.
    pub const PAT_PROVIDER_PREFIX: &str = "user-token:";

    /// `auth_provider` of a signed-URL bearer: `signed-url:<signer's provider>`.
    pub const SIGNED_URL_PROVIDER_PREFIX: &str = "signed-url:";

    /// The provider that vouches for `user_id`: the minting provider of a PAT
    /// or a signed URL, `auth_provider` otherwise.
    pub fn principal_provider(&self) -> Option<&str> {
        let p = self.auth_provider.as_deref()?;
        Some(
            p.strip_prefix(Self::PAT_PROVIDER_PREFIX)
                .or_else(|| p.strip_prefix(Self::SIGNED_URL_PROVIDER_PREFIX))
                .unwrap_or(p),
        )
    }

    /// `<provider>:<user_id>` — the key a `user:` grant and a user owner row
    /// name. A user id is a string each provider picks for itself, so the id
    /// alone would let `alice` on one provider act as `alice` on another.
    ///
    /// An identity no provider vouches for keeps its bare id. Every
    /// `AuthProvider` sets `auth_provider`, so that is only ever the process's
    /// own [`Self::system`] identity or a test fixture — never a caller — and a
    /// bare grant (the legacy, ambiguous form) therefore matches no request.
    pub fn user_principal(&self) -> Option<String> {
        let id = self.user_id.as_deref()?;
        Some(match self.principal_provider() {
            Some(p) => format!("{p}:{id}"),
            None => id.to_owned(),
        })
    }

    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }

    pub fn has_role_at_least(&self, minimum: &Role) -> bool {
        &self.role >= minimum
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_identity_is_named_and_admin() {
        let sys = Identity::system();
        assert_eq!(sys.user_id.as_deref(), Some("system"));
        assert!(sys.is_admin());
        assert!(
            sys.groups.is_empty(),
            "a scheduled task belongs to no team, and a group grant must not \
             widen what it can reach"
        );
    }

    /// An anonymous caller and the scheduler must not be the same subject in the
    /// audit trail.
    #[test]
    fn system_is_not_anonymous() {
        assert_ne!(Identity::system().user_id, Identity::anonymous().user_id);
    }

    #[test]
    fn role_from_str_round_trips_every_variant() {
        for role in [Role::Anonymous, Role::User, Role::Admin] {
            let s = role.to_string();
            assert_eq!(s.parse::<Role>().unwrap(), role);
        }
    }

    #[test]
    fn role_from_str_rejects_unknown() {
        assert!("not-a-real-role".parse::<Role>().is_err());
    }

    #[test]
    fn is_admin_true_only_for_admin_role() {
        assert!(Identity {
            user_id: None,
            role: Role::Admin,
            auth_provider: None,
            groups: vec![]
        }
        .is_admin());
        assert!(!Identity {
            user_id: None,
            role: Role::User,
            auth_provider: None,
            groups: vec![]
        }
        .is_admin());
        assert!(!Identity::anonymous().is_admin());
    }

    #[test]
    fn the_least_privileged_bypass_role_is_the_floor() {
        assert!(!Role::Admin.clears_bypass(&[]));
        assert!(Role::Admin.clears_bypass(&[Role::User]));
        assert!(Role::User.clears_bypass(&[Role::Admin, Role::User]));
        assert!(!Role::Anonymous.clears_bypass(&[Role::User]));
        assert!(!Role::User.clears_bypass(&[Role::Admin]));
    }

    #[test]
    fn has_role_at_least_respects_ordering() {
        let admin = Identity {
            user_id: None,
            role: Role::Admin,
            auth_provider: None,
            groups: vec![],
        };
        let user = Identity {
            user_id: None,
            role: Role::User,
            auth_provider: None,
            groups: vec![],
        };
        let anon = Identity::anonymous();

        assert!(admin.has_role_at_least(&Role::Admin));
        assert!(admin.has_role_at_least(&Role::User));
        assert!(admin.has_role_at_least(&Role::Anonymous));

        assert!(!user.has_role_at_least(&Role::Admin));
        assert!(user.has_role_at_least(&Role::User));
        assert!(user.has_role_at_least(&Role::Anonymous));

        assert!(!anon.has_role_at_least(&Role::Admin));
        assert!(!anon.has_role_at_least(&Role::User));
        assert!(anon.has_role_at_least(&Role::Anonymous));
    }

    #[test]
    fn anonymous_returns_anonymous_role_and_no_user_id() {
        let id = Identity::anonymous();
        assert_eq!(id.role, Role::Anonymous);
        assert!(id.user_id.is_none());
        assert!(id.auth_provider.is_none());
    }
}
