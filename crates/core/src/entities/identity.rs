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
