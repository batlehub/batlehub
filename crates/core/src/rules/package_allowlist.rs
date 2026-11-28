//! `package_allowlist` (RFC 0051): a registry serves only the packages an
//! operator listed, each optionally restricted to a version range.
//!
//! One config block builds two rules over one shared [`Allowlist`]:
//!
//! - [`PackageAllowlistRule`] (`package_allowlist`) judges the name. It also
//!   answers [`Rule::judge_name`], the pre-resolve check the funnels call before
//!   anything is fetched (§5.1).
//! - [`PackageAllowlistVersionRule`] (`package_allowlist_version`) judges the
//!   resolved version of a listed name.
//!
//! Two names because `why`, `reason_code_for` and the access log key on the
//! refusing rule's name, and one name cannot carry two reason codes (§15
//! decision 11).
//!
//! Neither rule reads a placeholder name or version to decide what a
//! coordinate is: the handler says so through [`CoordinateRole`] (§15
//! decision 8). A whole-registry document is never judged; a listing of one
//! package has its name judged and its range skipped.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::entities::RegistryKind;
use crate::entities::{namespace_matches, Action, CoordinateRole, Identity, PackageId, Role};
use crate::rules::version_match::VersionMatcher;
use crate::rules::{Rule, RuleContext, RuleDecision};

/// The name rule's name: what `why`, the access log and the simulator record.
pub const PACKAGE_ALLOWLIST_GATE: &str = "package_allowlist";
/// The version rule's name.
pub const PACKAGE_ALLOWLIST_VERSION_GATE: &str = "package_allowlist_version";

/// The reason code a name refusal is recorded under.
pub const PACKAGE_NOT_ALLOWED: &str = "PACKAGE_NOT_ALLOWED";
/// The reason code a version refusal is recorded under.
pub const VERSION_NOT_ALLOWED: &str = "VERSION_NOT_ALLOWED";

/// One configured entry, as the server hands it over: a name in the
/// ecosystem's own spelling (possibly `<prefix><separator>*`), and an
/// optional version list. Validation has already run (`crates/config`).
#[derive(Debug, Clone)]
pub struct AllowlistEntrySpec {
    pub name: String,
    pub versions: Option<Vec<String>>,
}

/// `None` = any version; `Some` = one of these.
type Versions = Option<Vec<VersionMatcher>>;

/// The list itself, built once per config load and shared by both rules.
#[derive(Debug)]
pub struct Allowlist {
    kind: RegistryKind,
    exact: HashMap<String, Versions>,
    /// Canonical prefix (without the trailing `<separator>*`), longest first,
    /// so the most specific prefix entry wins whatever the config order.
    prefixes: Vec<(String, Versions)>,
    bypass_roles: Vec<Role>,
}

/// What the list says about a name.
enum Listing<'a> {
    Unlisted,
    Listed(&'a Versions),
}

impl Allowlist {
    pub fn new(
        kind: RegistryKind,
        entries: &[AllowlistEntrySpec],
        bypass_roles: Vec<Role>,
    ) -> Self {
        let sep = crate::entities::namespace_separator(kind);
        let mut exact = HashMap::new();
        let mut prefixes = Vec::new();
        for entry in entries {
            let versions = entry
                .versions
                .as_ref()
                .map(|vs| vs.iter().map(|v| VersionMatcher::parse(v)).collect());
            match entry
                .name
                .strip_suffix('*')
                .and_then(|n| n.strip_suffix(sep))
            {
                Some(prefix) => {
                    let canonical = kind.canonical_package_name(prefix).into_owned();
                    prefixes.push((canonical, versions));
                }
                None => {
                    let canonical = kind.canonical_package_name(&entry.name).into_owned();
                    exact.insert(canonical, versions);
                }
            }
        }
        prefixes.sort_by_key(|(prefix, _): &(String, Versions)| std::cmp::Reverse(prefix.len()));
        Self {
            kind,
            exact,
            prefixes,
            bypass_roles,
        }
    }

    /// An exact entry wins over any prefix entry; among prefixes, the longest.
    /// A prefix entry matches the names *under* it and not the bare prefix:
    /// on NuGet the id `Acme` may belong to someone other than the owner of
    /// `Acme.*` (§15 decision 18).
    fn listing(&self, name: &str) -> Listing<'_> {
        let canonical = self.kind.canonical_package_name(name);
        if let Some(v) = self.exact.get(canonical.as_ref()) {
            return Listing::Listed(v);
        }
        for (prefix, v) in &self.prefixes {
            if canonical.as_ref() != prefix
                && namespace_matches(self.kind, prefix, canonical.as_ref())
            {
                return Listing::Listed(v);
            }
        }
        Listing::Unlisted
    }

    fn is_listed(&self, name: &str) -> bool {
        matches!(self.listing(name), Listing::Listed(_))
    }

    fn name_refusal(&self, id: &PackageId, identity: &Identity) -> RuleDecision {
        crate::rules::gate(
            &self.bypass_roles,
            identity,
            format!(
                "package allowlist: '{}' is not listed for registry '{}'",
                printable(&id.name),
                printable(&id.registry)
            ),
        )
    }
}

/// The actions the rule judges: reads, and the console's package page.
/// Publishing is a grant (`releases:publish`), not this rule's business.
pub fn is_judged_action(action: Action) -> bool {
    matches!(
        action,
        Action::ReleasesRead | Action::ReleasesList | Action::SourceRead | Action::CatalogueBrowse
    )
}

/// The requested name is the client's text and ends up in a `403` body and a
/// terminal: control characters (ANSI escapes) are dropped first (§7.3).
fn printable(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// `package_allowlist`: refuses a read of a package the list does not name.
pub struct PackageAllowlistRule {
    list: Arc<Allowlist>,
}

impl PackageAllowlistRule {
    pub fn new(list: Arc<Allowlist>) -> Self {
        Self { list }
    }

    fn judge(&self, id: &PackageId, identity: &Identity) -> RuleDecision {
        if id.role == CoordinateRole::RegistryDocument || self.list.is_listed(&id.name) {
            return RuleDecision::Allow;
        }
        self.list.name_refusal(id, identity)
    }
}

#[async_trait]
impl Rule for PackageAllowlistRule {
    fn name(&self) -> &str {
        PACKAGE_ALLOWLIST_GATE
    }

    async fn evaluate(&self, ctx: &RuleContext<'_>) -> RuleDecision {
        if !is_judged_action(ctx.action) {
            return RuleDecision::Allow;
        }
        self.judge(&ctx.package.id, ctx.identity)
    }

    fn judge_name(&self, id: &PackageId, identity: &Identity) -> Option<RuleDecision> {
        Some(self.judge(id, identity))
    }
}

/// `package_allowlist_version`: refuses a listed package at a version outside
/// its entry's range. An unlisted name passes here — the name rule owns it.
pub struct PackageAllowlistVersionRule {
    list: Arc<Allowlist>,
}

impl PackageAllowlistVersionRule {
    pub fn new(list: Arc<Allowlist>) -> Self {
        Self { list }
    }
}

#[async_trait]
impl Rule for PackageAllowlistVersionRule {
    fn name(&self) -> &str {
        PACKAGE_ALLOWLIST_VERSION_GATE
    }

    async fn evaluate(&self, ctx: &RuleContext<'_>) -> RuleDecision {
        let id = &ctx.package.id;
        // Only a coordinate that names one version is judged on its version:
        // a listing carries a placeholder (`__index__`, `info`, `latest`), and
        // the role — not the string — says so (§15 decision 8).
        if !is_judged_action(ctx.action) || id.role != CoordinateRole::Package {
            return RuleDecision::Allow;
        }
        let Listing::Listed(Some(versions)) = self.list.listing(&id.name) else {
            return RuleDecision::Allow;
        };
        if versions.iter().any(|m| m.matches(&id.version)) {
            return RuleDecision::Allow;
        }
        crate::rules::gate(
            &self.list.bypass_roles,
            ctx.identity,
            format!(
                "package allowlist: version '{}' of '{}' is not allowed for registry '{}'",
                printable(&id.version),
                printable(&id.name),
                printable(&id.registry)
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::PackageMetadata;

    fn spec(name: &str, versions: Option<&[&str]>) -> AllowlistEntrySpec {
        AllowlistEntrySpec {
            name: name.to_owned(),
            versions: versions.map(|vs| vs.iter().map(|v| (*v).to_owned()).collect()),
        }
    }

    fn list(
        kind: RegistryKind,
        entries: &[AllowlistEntrySpec],
        bypass: Vec<Role>,
    ) -> Arc<Allowlist> {
        Arc::new(Allowlist::new(kind, entries, bypass))
    }

    fn meta(id: PackageId) -> PackageMetadata {
        PackageMetadata {
            id,
            published_at: None,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control: None,
        }
    }

    fn identity(role: Role) -> Identity {
        Identity {
            role,
            ..Identity::anonymous()
        }
    }

    async fn eval(rule: &dyn Rule, id: PackageId, role: Role, action: Action) -> RuleDecision {
        let who = identity(role);
        let m = meta(id);
        let ctx = RuleContext {
            identity: &who,
            package: &m,
            action,
            cache_entry: None,
            requested_version: None,
        };
        rule.evaluate(&ctx).await
    }

    fn npm(name: &str, version: &str) -> PackageId {
        PackageId::new("npm", name, version)
    }

    #[test]
    fn rule_names_are_the_gate_constants() {
        let l = list(RegistryKind::Npm, &[spec("react", None)], vec![]);
        assert_eq!(
            PackageAllowlistRule::new(l.clone()).name(),
            "package_allowlist"
        );
        assert_eq!(
            PackageAllowlistVersionRule::new(l).name(),
            "package_allowlist_version"
        );
    }

    #[tokio::test]
    async fn a_listed_name_is_allowed_and_an_unlisted_one_refused() {
        let rule =
            PackageAllowlistRule::new(list(RegistryKind::Npm, &[spec("react", None)], vec![]));
        assert!(!eval(
            &rule,
            npm("react", "18.3.1"),
            Role::User,
            Action::ReleasesRead
        )
        .await
        .is_deny());
        let d = eval(
            &rule,
            npm("left-pad", "1.3.0"),
            Role::User,
            Action::ReleasesRead,
        )
        .await;
        let RuleDecision::Deny { reason } = d else {
            panic!("expected a refusal")
        };
        assert_eq!(
            reason,
            "package allowlist: 'left-pad' is not listed for registry 'npm'"
        );
    }

    #[tokio::test]
    async fn non_read_actions_pass() {
        let rule =
            PackageAllowlistRule::new(list(RegistryKind::Npm, &[spec("react", None)], vec![]));
        assert!(!eval(
            &rule,
            npm("left-pad", "1.0.0"),
            Role::User,
            Action::ReleasesPublish
        )
        .await
        .is_deny());
    }

    #[tokio::test]
    async fn allowlist_spelling_variants_match_one_entry() {
        let pypi = PackageAllowlistRule::new(list(
            RegistryKind::Pypi,
            &[spec("python-dateutil", None)],
            vec![],
        ));
        for name in [
            "python-dateutil",
            "Python.Dateutil",
            "python_dateutil",
            "PYTHON--dateutil",
        ] {
            let id = PackageId::new("pypi", name, "2.9.0");
            assert!(
                !eval(&pypi, id, Role::User, Action::ReleasesRead)
                    .await
                    .is_deny(),
                "{name}"
            );
        }
        let nuget = PackageAllowlistRule::new(list(
            RegistryKind::Nuget,
            &[spec("Newtonsoft.Json", None)],
            vec![],
        ));
        let id = PackageId::new("nuget", "newtonsoft.json", "13.0.3");
        assert!(!eval(&nuget, id, Role::User, Action::ReleasesRead)
            .await
            .is_deny());
        let go = PackageAllowlistRule::new(list(
            RegistryKind::Goproxy,
            &[spec("github.com/BurntSushi/toml", None)],
            vec![],
        ));
        let id = PackageId::new("go", "github.com/!burnt!sushi/toml", "v1.3.2");
        assert!(!eval(&go, id, Role::User, Action::ReleasesRead)
            .await
            .is_deny());
    }

    #[tokio::test]
    async fn allowlist_prefix_needs_separator() {
        let rule =
            PackageAllowlistRule::new(list(RegistryKind::Npm, &[spec("@acme/*", None)], vec![]));
        assert!(!eval(
            &rule,
            npm("@acme/widget", "1.0.0"),
            Role::User,
            Action::ReleasesRead
        )
        .await
        .is_deny());
        assert!(eval(
            &rule,
            npm("@acme-evil/x", "1.0.0"),
            Role::User,
            Action::ReleasesRead
        )
        .await
        .is_deny());
    }

    #[tokio::test]
    async fn allowlist_prefix_excludes_bare_prefix() {
        let rule =
            PackageAllowlistRule::new(list(RegistryKind::Nuget, &[spec("Acme.*", None)], vec![]));
        let under = PackageId::new("nuget", "acme.billing", "1.0.0");
        let bare = PackageId::new("nuget", "acme", "1.0.0");
        assert!(!eval(&rule, under, Role::User, Action::ReleasesRead)
            .await
            .is_deny());
        assert!(eval(&rule, bare, Role::User, Action::ReleasesRead)
            .await
            .is_deny());
    }

    #[tokio::test]
    async fn exact_entry_wins_over_prefix_and_longest_prefix_wins() {
        let l = list(
            RegistryKind::Npm,
            &[
                spec("@acme/*", None),
                spec("@acme/billing", Some(&["^2"])),
                spec("@acme/tools/*", Some(&["1.0.0"])),
            ],
            vec![],
        );
        let rule = PackageAllowlistVersionRule::new(l);
        assert!(eval(
            &rule,
            npm("@acme/billing", "1.9.0"),
            Role::User,
            Action::ReleasesRead
        )
        .await
        .is_deny());
        assert!(!eval(
            &rule,
            npm("@acme/billing", "2.1.0"),
            Role::User,
            Action::ReleasesRead
        )
        .await
        .is_deny());
        assert!(!eval(
            &rule,
            npm("@acme/other", "9.9.9"),
            Role::User,
            Action::ReleasesRead
        )
        .await
        .is_deny());
        assert!(eval(
            &rule,
            npm("@acme/tools/cli", "1.0.1"),
            Role::User,
            Action::ReleasesRead
        )
        .await
        .is_deny());
    }

    #[tokio::test]
    async fn no_versions_allows_every_version() {
        let rule = PackageAllowlistVersionRule::new(list(
            RegistryKind::Npm,
            &[spec("react", None)],
            vec![],
        ));
        assert!(!eval(
            &rule,
            npm("react", "0.0.1-anything"),
            Role::User,
            Action::ReleasesRead
        )
        .await
        .is_deny());
    }

    #[tokio::test]
    async fn version_rule_passes_an_unlisted_name() {
        let rule = PackageAllowlistVersionRule::new(list(
            RegistryKind::Npm,
            &[spec("react", Some(&["^18"]))],
            vec![],
        ));
        assert!(!eval(
            &rule,
            npm("left-pad", "1.0.0"),
            Role::User,
            Action::ReleasesRead
        )
        .await
        .is_deny());
    }

    #[tokio::test]
    async fn allowlist_latest_resolving_out_of_range_is_denied() {
        // The resolve turned `latest` into 5.0.0; the rule sees the resolved id.
        let rule = PackageAllowlistVersionRule::new(list(
            RegistryKind::Npm,
            &[spec("lodash", Some(&[">=4.17.21, <5"]))],
            vec![],
        ));
        let d = eval(
            &rule,
            npm("lodash", "5.0.0"),
            Role::User,
            Action::ReleasesRead,
        )
        .await;
        let RuleDecision::Deny { reason } = d else {
            panic!("expected a refusal")
        };
        assert_eq!(
            reason,
            "package allowlist: version '5.0.0' of 'lodash' is not allowed for registry 'npm'"
        );
    }

    #[tokio::test]
    async fn allowlist_literal_latest_on_a_package_coordinate_is_judged() {
        let rule = PackageAllowlistVersionRule::new(list(
            RegistryKind::Github,
            &[spec("acme/tool", Some(&["v1.0.0"]))],
            vec![],
        ));
        let id = PackageId::new("github", "acme/tool", "latest");
        assert!(eval(&rule, id, Role::User, Action::ReleasesRead)
            .await
            .is_deny());
    }

    #[tokio::test]
    async fn a_listing_has_its_name_judged_and_its_range_skipped() {
        let l = list(
            RegistryKind::Nuget,
            &[spec("Newtonsoft.Json", Some(&["13.0.3"]))],
            vec![],
        );
        let listed = PackageId::listing("nuget", "newtonsoft.json", "__index__");
        let unlisted = PackageId::listing("nuget", "left.pad", "__index__");
        let name = PackageAllowlistRule::new(l.clone());
        let range = PackageAllowlistVersionRule::new(l);
        assert!(
            !eval(&range, listed.clone(), Role::User, Action::ReleasesList)
                .await
                .is_deny()
        );
        assert!(!eval(&name, listed, Role::User, Action::ReleasesList)
            .await
            .is_deny());
        assert!(eval(&name, unlisted, Role::User, Action::ReleasesList)
            .await
            .is_deny());
    }

    #[tokio::test]
    async fn a_registry_document_is_never_judged() {
        let l = list(
            RegistryKind::Pypi,
            &[spec("requests", Some(&["2.0.0"]))],
            vec![],
        );
        let root = PackageId::registry_document("pypi", "__simple__", "__root__");
        assert!(!eval(
            &PackageAllowlistRule::new(l.clone()),
            root.clone(),
            Role::User,
            Action::ReleasesList
        )
        .await
        .is_deny());
        assert!(!eval(
            &PackageAllowlistVersionRule::new(l.clone()),
            root.clone(),
            Role::User,
            Action::ReleasesList
        )
        .await
        .is_deny());
        assert!(matches!(
            PackageAllowlistRule::new(l).judge_name(&root, &identity(Role::User)),
            Some(RuleDecision::Allow)
        ));
    }

    #[tokio::test]
    async fn unmarked_document_is_refused() {
        // A handler that forgot its role builds a `Package`: fail closed.
        let rule =
            PackageAllowlistRule::new(list(RegistryKind::Pypi, &[spec("requests", None)], vec![]));
        let root = PackageId::new("pypi", "__simple__", "__root__");
        assert!(eval(&rule, root, Role::User, Action::ReleasesList)
            .await
            .is_deny());
    }

    #[tokio::test]
    async fn allowlist_unparseable_version_is_refused() {
        let rule = PackageAllowlistVersionRule::new(list(
            RegistryKind::Pypi,
            &[spec("django", Some(&[">=4.0.0"]))],
            vec![],
        ));
        let id = PackageId::new("pypi", "django", "4.2");
        assert!(eval(&rule, id, Role::User, Action::ReleasesRead)
            .await
            .is_deny());
    }

    #[tokio::test]
    async fn allowlist_refusal_body_names_no_range() {
        let rule = PackageAllowlistVersionRule::new(list(
            RegistryKind::Npm,
            &[spec("lodash", Some(&[">=4.17.21, <5"]))],
            vec![],
        ));
        let RuleDecision::Deny { reason } = eval(
            &rule,
            npm("lodash", "4.17.20"),
            Role::User,
            Action::ReleasesRead,
        )
        .await
        else {
            panic!("expected a refusal")
        };
        assert!(
            !reason.contains("4.17.21") && !reason.contains('<'),
            "{reason}"
        );
    }

    #[tokio::test]
    async fn control_characters_are_stripped_from_the_reason() {
        let rule =
            PackageAllowlistRule::new(list(RegistryKind::Npm, &[spec("react", None)], vec![]));
        let RuleDecision::Deny { reason } = eval(
            &rule,
            npm("evil\u{1b}[31m", "1.0.0"),
            Role::User,
            Action::ReleasesRead,
        )
        .await
        else {
            panic!("expected a refusal")
        };
        assert!(!reason.contains('\u{1b}'), "{reason}");
    }

    #[tokio::test]
    async fn bypass_role_resolves_to_allow_for_admin() {
        let rule = PackageAllowlistRule::new(list(
            RegistryKind::Npm,
            &[spec("react", None)],
            vec![Role::Admin],
        ));
        assert!(!eval(
            &rule,
            npm("left-pad", "1.0.0"),
            Role::Admin,
            Action::ReleasesRead
        )
        .await
        .is_deny());
    }

    #[tokio::test]
    async fn bypass_role_resolves_to_deny_for_a_user_with_the_hint() {
        let rule = PackageAllowlistRule::new(list(
            RegistryKind::Npm,
            &[spec("react", None)],
            vec![Role::Admin],
        ));
        let RuleDecision::Deny { reason } = eval(
            &rule,
            npm("left-pad", "1.0.0"),
            Role::User,
            Action::ReleasesRead,
        )
        .await
        else {
            panic!("expected a refusal")
        };
        assert!(
            reason.ends_with("(bypass requires role 'admin' or higher, you have 'user')"),
            "{reason}"
        );
    }

    #[test]
    fn only_the_name_rule_answers_judge_name() {
        let l = list(RegistryKind::Npm, &[spec("react", None)], vec![]);
        let who = identity(Role::User);
        assert!(PackageAllowlistVersionRule::new(l.clone())
            .judge_name(&npm("x", "1"), &who)
            .is_none());
        assert!(PackageAllowlistRule::new(l)
            .judge_name(&npm("x", "1"), &who)
            .is_some_and(|d| d.is_deny()));
    }
}
