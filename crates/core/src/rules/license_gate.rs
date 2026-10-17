use std::sync::Arc;

use async_trait::async_trait;

use crate::entities::Role;
use crate::ports::SbomRepository;
use crate::rules::{Rule, RuleContext, RuleDecision};

/// Denies (or, in warn-only mode, permits) a package version whose declared
/// licence is refused by policy (RFC 0004-bis §13.1).
///
/// The licence is what the package's **own manifest** declares — `Cargo.toml`'s
/// `license`, `package.json`'s `license`, `pom.xml`'s `<licenses>`, `METADATA`'s
/// `License-Expression`, `.nuspec`'s `<license>`. It is *not* the licence of
/// anything in the dependency tree: BatleHub does not resolve a graph, so a
/// rule that claimed to gate on transitive licences would be claiming more than
/// it can see.
///
/// # The unknown case is the whole design
///
/// The manifest lives inside the archive, which means the licence is recorded
/// on the way *through* the proxy — after the first fetch, not before it. So on
/// a first request the answer is genuinely unknown, and `allow_unknown` decides
/// what that means. Sixteen of the twenty-one registry types have no manifest
/// parser at all, and those are permanently unknown.
///
/// `allow_unknown = true` (the default) is eventually-consistent denial: the
/// first fetch proceeds and every later one is gated. `allow_unknown = false`
/// refuses anything not already known, which is conservative and costs a fetch
/// per package — the same trade `integrity.require_metadata` makes.
///
/// What the rule must never do is treat "unknown" as "permissively licensed".
/// That is the RFC 0004-bis §2.4 defect — answering `allow` over a condition
/// that was never observed.
pub struct LicenseGateRule {
    pub repo: Arc<dyn SbomRepository>,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub allow_unknown: bool,
    pub bypass_roles: Vec<Role>,
    pub block: bool,
}

impl LicenseGateRule {
    pub fn new(
        repo: Arc<dyn SbomRepository>,
        allow: Vec<String>,
        deny: Vec<String>,
        allow_unknown: bool,
        bypass_roles: Vec<Role>,
        block: bool,
    ) -> Self {
        Self {
            repo,
            allow,
            deny,
            allow_unknown,
            bypass_roles,
            block,
        }
    }
}

/// Manifests are hand-written, so `  MIT ` and `mit` are the same declaration.
/// Nothing beyond case and whitespace is normalised — see `LicenseGateConfig`.
fn matches(candidate: &str, list: &[String]) -> bool {
    list.iter()
        .any(|entry| entry.trim().eq_ignore_ascii_case(candidate.trim()))
}

#[async_trait]
impl Rule for LicenseGateRule {
    fn name(&self) -> &str {
        "license_gate"
    }

    async fn evaluate(&self, ctx: &RuleContext<'_>) -> RuleDecision {
        // Warn-only mode never blocks; skip the lookup entirely.
        if !self.block {
            return RuleDecision::Allow;
        }
        if ctx.identity.role.clears_bypass(&self.bypass_roles) {
            return RuleDecision::Allow;
        }

        let id = &ctx.package.id;
        let license = match self
            .repo
            .get_license_for_coordinate(&id.registry, &id.name, &id.version)
            .await
        {
            Ok(l) => l,
            Err(e) => {
                // SECURITY: fail-open, consistent with BlockListRule and
                // CveGateRule — a DB outage must not turn the proxy into a
                // brick wall. Note this is deliberately *not* routed through
                // the `allow_unknown = false` path: an unreachable database is
                // an outage, not a package that failed to declare a licence,
                // and conflating them would make every request fail closed the
                // moment Postgres blinked.
                tracing::warn!(
                    package = %id,
                    error = %e,
                    "LicenseGateRule: failed to query the recorded licence, failing open"
                );
                return RuleDecision::Allow;
            }
        };

        let Some(license) = license else {
            return if self.allow_unknown {
                RuleDecision::Allow
            } else {
                RuleDecision::Deny {
                    reason: "blocked: no declared licence recorded for this version, and \
                             license_gate.allow_unknown is false"
                        .to_owned(),
                }
            };
        };

        // Deny is checked first so an entry in both lists is a refusal: a
        // policy that contradicts itself should fail safe.
        if matches(&license, &self.deny) {
            return RuleDecision::Deny {
                reason: format!("blocked: licence '{license}' is on the deny list"),
            };
        }

        if !self.allow.is_empty() && !matches(&license, &self.allow) {
            return RuleDecision::Deny {
                reason: format!(
                    "blocked: licence '{license}' is not on the allow list ({})",
                    self.allow.join(", ")
                ),
            };
        }

        RuleDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use crate::entities::Action;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use chrono::{DateTime, Utc};

    use super::*;
    use crate::entities::{ArtifactSbom, Identity, PackageId, PackageMetadata, Role, SbomFormat};
    use crate::error::CoreError;

    #[derive(Default)]
    struct MemSbomRepo {
        /// keyed by "registry/name/version"
        licenses: Mutex<HashMap<String, String>>,
    }

    impl MemSbomRepo {
        fn with(reg: &str, name: &str, ver: &str, license: &str) -> Arc<Self> {
            let r = Arc::new(Self::default());
            r.licenses
                .lock()
                .unwrap()
                .insert(format!("{reg}/{name}/{ver}"), license.to_owned());
            r
        }
    }

    #[async_trait]
    impl SbomRepository for MemSbomRepo {
        async fn upsert_sbom(&self, _sbom: ArtifactSbom) -> Result<(), CoreError> {
            Ok(())
        }
        async fn get_sbom(
            &self,
            _artifact_key: &str,
            _format: &SbomFormat,
        ) -> Result<Option<ArtifactSbom>, CoreError> {
            Ok(None)
        }
        async fn get_sbom_by_coordinates(
            &self,
            _registry: &str,
            _package_name: &str,
            _version: &str,
            _format: &SbomFormat,
        ) -> Result<Option<ArtifactSbom>, CoreError> {
            Ok(None)
        }
        async fn get_license_for_coordinate(
            &self,
            registry: &str,
            package_name: &str,
            version: &str,
        ) -> Result<Option<String>, CoreError> {
            Ok(self
                .licenses
                .lock()
                .unwrap()
                .get(&format!("{registry}/{package_name}/{version}"))
                .cloned())
        }
        async fn list_sboms_for_export(
            &self,
            _registry: Option<&str>,
            _from: Option<DateTime<Utc>>,
            _to: Option<DateTime<Utc>>,
            _limit: u64,
            _offset: u64,
        ) -> Result<Vec<ArtifactSbom>, CoreError> {
            Ok(vec![])
        }
    }

    struct ErrSbomRepo;

    #[async_trait]
    impl SbomRepository for ErrSbomRepo {
        async fn upsert_sbom(&self, _sbom: ArtifactSbom) -> Result<(), CoreError> {
            Ok(())
        }
        async fn get_sbom(
            &self,
            _artifact_key: &str,
            _format: &SbomFormat,
        ) -> Result<Option<ArtifactSbom>, CoreError> {
            Ok(None)
        }
        async fn get_sbom_by_coordinates(
            &self,
            _registry: &str,
            _package_name: &str,
            _version: &str,
            _format: &SbomFormat,
        ) -> Result<Option<ArtifactSbom>, CoreError> {
            Ok(None)
        }
        async fn get_license_for_coordinate(
            &self,
            _registry: &str,
            _package_name: &str,
            _version: &str,
        ) -> Result<Option<String>, CoreError> {
            Err(CoreError::Database("db down".into()))
        }
        async fn list_sboms_for_export(
            &self,
            _registry: Option<&str>,
            _from: Option<DateTime<Utc>>,
            _to: Option<DateTime<Utc>>,
            _limit: u64,
            _offset: u64,
        ) -> Result<Vec<ArtifactSbom>, CoreError> {
            Ok(vec![])
        }
    }

    fn meta() -> PackageMetadata {
        PackageMetadata {
            id: PackageId::new("npm", "left-pad", "1.3.0"),
            published_at: Some(Utc::now()),
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control: None,
        }
    }

    fn identity(role: Role) -> Identity {
        Identity {
            user_id: None,
            role,
            auth_provider: None,
            groups: vec![],
        }
    }

    async fn eval(rule: &LicenseGateRule, id: &Identity) -> RuleDecision {
        let m = meta();
        let ctx = RuleContext {
            identity: id,
            package: &m,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: None,
        };
        rule.evaluate(&ctx).await
    }

    fn rule(repo: Arc<dyn SbomRepository>, allow: &[&str], deny: &[&str]) -> LicenseGateRule {
        LicenseGateRule::new(
            repo,
            allow.iter().map(|s| (*s).to_owned()).collect(),
            deny.iter().map(|s| (*s).to_owned()).collect(),
            true,
            vec![Role::Admin],
            true,
        )
    }

    #[tokio::test]
    async fn warn_only_allows_even_with_a_denied_license() {
        let repo = MemSbomRepo::with("npm", "left-pad", "1.3.0", "AGPL-3.0");
        let mut r = rule(repo, &[], &["AGPL-3.0"]);
        r.block = false;
        assert!(matches!(
            eval(&r, &identity(Role::Anonymous)).await,
            RuleDecision::Allow
        ));
    }

    #[tokio::test]
    async fn deny_list_refuses() {
        let repo = MemSbomRepo::with("npm", "left-pad", "1.3.0", "AGPL-3.0");
        let r = rule(repo, &[], &["AGPL-3.0"]);
        match eval(&r, &identity(Role::Anonymous)).await {
            RuleDecision::Deny { reason } => assert!(reason.contains("AGPL-3.0"), "{reason}"),
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn allow_list_refuses_anything_not_on_it() {
        let repo = MemSbomRepo::with("npm", "left-pad", "1.3.0", "GPL-3.0");
        let r = rule(repo, &["MIT", "Apache-2.0"], &[]);
        assert!(matches!(
            eval(&r, &identity(Role::Anonymous)).await,
            RuleDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn allow_list_permits_a_listed_license() {
        let repo = MemSbomRepo::with("npm", "left-pad", "1.3.0", "MIT");
        let r = rule(repo, &["MIT", "Apache-2.0"], &[]);
        assert!(matches!(
            eval(&r, &identity(Role::Anonymous)).await,
            RuleDecision::Allow
        ));
    }

    /// An empty allow list is "no allowlist", not "allow nothing" — otherwise a
    /// deny-only policy would refuse every package in the registry.
    #[tokio::test]
    async fn empty_allow_list_is_not_an_empty_allowlist() {
        let repo = MemSbomRepo::with("npm", "left-pad", "1.3.0", "MIT");
        let r = rule(repo, &[], &["AGPL-3.0"]);
        assert!(matches!(
            eval(&r, &identity(Role::Anonymous)).await,
            RuleDecision::Allow
        ));
    }

    #[tokio::test]
    async fn comparison_ignores_case_and_surrounding_space() {
        let repo = MemSbomRepo::with("npm", "left-pad", "1.3.0", "  mit ");
        let r = rule(repo, &["MIT"], &[]);
        assert!(matches!(
            eval(&r, &identity(Role::Anonymous)).await,
            RuleDecision::Allow
        ));
    }

    /// A compound expression is a different declaration from a bare id.
    /// Matching it against `allow = ["MIT"]` would let any package opt out of
    /// the gate by adding an alternative.
    #[tokio::test]
    async fn compound_expression_does_not_match_a_bare_identifier() {
        let repo = MemSbomRepo::with("npm", "left-pad", "1.3.0", "MIT OR AGPL-3.0");
        let r = rule(repo, &["MIT"], &[]);
        assert!(matches!(
            eval(&r, &identity(Role::Anonymous)).await,
            RuleDecision::Deny { .. }
        ));
    }

    /// A licence in both lists is a policy that contradicts itself; it fails safe.
    #[tokio::test]
    async fn deny_wins_over_allow() {
        let repo = MemSbomRepo::with("npm", "left-pad", "1.3.0", "MIT");
        let r = rule(repo, &["MIT"], &["MIT"]);
        assert!(matches!(
            eval(&r, &identity(Role::Anonymous)).await,
            RuleDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn bypass_role_allowed_despite_denied_license() {
        let repo = MemSbomRepo::with("npm", "left-pad", "1.3.0", "AGPL-3.0");
        let r = rule(repo, &[], &["AGPL-3.0"]);
        assert!(matches!(
            eval(&r, &identity(Role::Admin)).await,
            RuleDecision::Allow
        ));
    }

    /// The first fetch of an uncached package has nothing recorded yet, and by
    /// default that proceeds — denial is eventually consistent.
    #[tokio::test]
    async fn unknown_license_allowed_by_default() {
        let repo = Arc::new(MemSbomRepo::default());
        let r = rule(repo, &["MIT"], &[]);
        assert!(matches!(
            eval(&r, &identity(Role::Anonymous)).await,
            RuleDecision::Allow
        ));
    }

    #[tokio::test]
    async fn unknown_license_refused_when_allow_unknown_is_false() {
        let repo = Arc::new(MemSbomRepo::default());
        let mut r = rule(repo, &["MIT"], &[]);
        r.allow_unknown = false;
        match eval(&r, &identity(Role::Anonymous)).await {
            RuleDecision::Deny { reason } => {
                assert!(reason.contains("allow_unknown"), "{reason}")
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    /// SECURITY: a database outage is not a package that failed to declare a
    /// licence. It fails open even under `allow_unknown = false`, or every
    /// request would fail closed the moment Postgres blinked.
    #[tokio::test]
    async fn db_error_fails_open_even_when_unknown_is_refused() {
        let mut r = rule(Arc::new(ErrSbomRepo), &["MIT"], &[]);
        r.allow_unknown = false;
        assert!(matches!(
            eval(&r, &identity(Role::Anonymous)).await,
            RuleDecision::Allow
        ));
    }

    #[test]
    fn rule_name_is_license_gate() {
        let r = rule(Arc::new(MemSbomRepo::default()), &[], &[]);
        assert_eq!(r.name(), "license_gate");
    }
}
