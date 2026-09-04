//! Grants: who may do what, attached to a node of the resource hierarchy.
//!
//! RFC 0015 §4.3. A grant is `subject → [verb]` written on a registry, a
//! namespace, a package or a version, and the permissions a request resolves to
//! are the **union** of every grant on the path from registry to version whose
//! subject matches the caller.
//!
//! # Union, and only union
//!
//! There is no precedence between tiers and none between subject forms. A deeper
//! node does not replace a shallower one's set, and a more specific subject does
//! not replace a broader one's. A grant only ever adds.
//!
//! That is not a simplification, it is the load-bearing property. Replacement is
//! revocation wearing precedence's clothes: given registry `role:user =
//! ["releases:read", "source:read"]` and package `role:user = ["releases:read"]`,
//! a union keeps `source:read` and a "deepest wins" rule silently drops it. Every
//! safety argument in §4.3 assumes the first — the delegation bounds, §7's *a
//! grant can never be revoked by a deeper node, only unmatched*, and §8.2's case
//! against deny rules. A model that resolves by replacing has deny rules; it just
//! does not call them that, and gets them without the trace §8.2 says a deny rule
//! would have to carry.
//!
//! The cost is that a deeper node cannot narrow. Writing a smaller set further
//! down does nothing at all. What buys is [`resolve`] being order-independent by
//! construction rather than by care — which is what makes §11.2's shuffle test
//! assertable.
//!
//! # Absence is not "everything"
//!
//! A node with no grants inherits its parent's. A node with an *empty* grant map
//! grants nothing **and stops inheritance** — the explicit way to seal a subtree.
//! The two are different states and [`Node::grants`] is an `Option` so they
//! cannot be confused.
//!
//! This is the modelling rule the 2026-08-26 survey's finding 2 broke, one layer
//! up: an empty accessible-registry list was bound as `NULL`, `= ANY` read that
//! as *every* registry, and a scoping bug became an enumeration of the whole
//! private inventory. The resolved set here is a plain collection whose empty
//! value means empty.

use std::collections::BTreeSet;
use std::fmt;

use super::{Action, Identity, RegistryKind, Role, Subject, Tier};

// ── Subjects ─────────────────────────────────────────────────────────────────

/// The forms a grant's left-hand side can take (RFC 0015 §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SubjectMatcher {
    /// `*` — anyone, including anonymous.
    Anyone,
    /// `role:<role>` — the coarse form, kept for the common case and for
    /// backward compatibility with `[registries.rbac]` (§8.3).
    Role(Role),
    /// `group:<provider>:<name>`, `group:*:<name>` for any provider, or
    /// `group::<name>` for a group string that carries no provider at all.
    Group {
        provider: GroupProvider,
        name: String,
    },
    /// `user:<id>`.
    User(String),
    /// `token:<name>` — a machine credential with no user behind it.
    ///
    /// Distinct from a PAT, which *represents* a user and resolves to that
    /// user's subject: a PAT is a credential, not a principal, and can never
    /// resolve to more than its owner holds (§4.3).
    Token(String),
}

/// Which providers a `group:` subject accepts.
///
/// Three cases, and they are three because `[registries.rbac.groups]` already
/// distinguishes three — exactly, and by accident of its implementation rather
/// than by design. `is_permitted_by_group` compares the config key to the
/// identity's group string, and *additionally* tries `*:<name>` when the group
/// carries a `provider:` prefix. So today:
///
/// | config key | matches identity group |
/// | --- | --- |
/// | `oidc1:eng` | `oidc1:eng` and nothing else |
/// | `*:eng` | `<any>:eng`, but **not** a bare `eng` |
/// | `eng` | a bare `eng`, but **not** `oidc1:eng` |
///
/// Collapsing the last two into one wildcard would be a **widening applied to
/// every deployment on upgrade** — `eng` would start matching `oidc1:eng` — which
/// is the silent privilege escalation §7 names as the migration's central risk.
/// So the third case is representable, and spelled `group::<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GroupProvider {
    /// `group:*:<name>` — any provider, as long as there is one.
    Any,
    /// `group:<provider>:<name>` — that provider.
    Named(String),
    /// `group::<name>` — a group string with no provider prefix.
    ///
    /// Not a form §4.3 invites anyone to write; it exists so a legacy
    /// `[registries.rbac.groups]` key can be translated without changing what it
    /// matches.
    Unprefixed,
}

/// A subject string was not one of the five forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectParseError(pub String);

impl fmt::Display for SubjectParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown grant subject '{}'. Expected '*', 'role:<role>', \
             'group:<provider>:<name>' (provider may be '*'), 'user:<id>' or 'token:<name>'",
            self.0
        )
    }
}

impl std::error::Error for SubjectParseError {}

impl SubjectMatcher {
    /// Parse a config-file subject key.
    pub fn parse(s: &str) -> Result<Self, SubjectParseError> {
        if s == "*" {
            return Ok(SubjectMatcher::Anyone);
        }
        let err = || SubjectParseError(s.to_owned());

        if let Some(rest) = s.strip_prefix("role:") {
            return rest
                .parse::<Role>()
                .map(SubjectMatcher::Role)
                .map_err(|_| err());
        }
        if let Some(rest) = s.strip_prefix("group:") {
            // `provider:name`. Split on the *first* colon only: a group name may
            // contain one, and `signed_url.rs` already learned that lesson the
            // hard way ("a group containing a separator cannot pose as two").
            let (provider, name) = rest.split_once(':').ok_or_else(err)?;
            if name.is_empty() {
                return Err(err());
            }
            return Ok(SubjectMatcher::Group {
                provider: match provider {
                    "*" => GroupProvider::Any,
                    "" => GroupProvider::Unprefixed,
                    p => GroupProvider::Named(p.to_owned()),
                },
                name: name.to_owned(),
            });
        }
        if let Some(rest) = s.strip_prefix("user:") {
            return if rest.is_empty() {
                Err(err())
            } else {
                Ok(SubjectMatcher::User(rest.to_owned()))
            };
        }
        if let Some(rest) = s.strip_prefix("token:") {
            return if rest.is_empty() {
                Err(err())
            } else {
                Ok(SubjectMatcher::Token(rest.to_owned()))
            };
        }
        Err(err())
    }

    /// Whether this matcher names `subject`.
    pub fn matches(&self, subject: &Subject) -> bool {
        let identity = subject.identity();
        match self {
            SubjectMatcher::Anyone => true,
            // `has_role_at_least`, not equality: role inheritance is what
            // `[registries.rbac]` has always done — an admin holds what a user
            // holds — and a translation that dropped it would take access away
            // from every existing config (§10 rule 1).
            SubjectMatcher::Role(role) => identity.has_role_at_least(role),
            SubjectMatcher::Group { provider, name } => identity
                .groups
                .iter()
                .any(|g| group_matches(g, provider, name)),
            SubjectMatcher::User(id) => identity.user_id.as_deref() == Some(id.as_str()),
            // No principal is a machine token yet — `Subject` has one variant
            // (phase 2). Answering `false` is the correct reading of "this
            // subject is not that token", and it is also the fail-closed one.
            SubjectMatcher::Token(_) => false,
        }
    }

    /// The wire form, for `explain` output and round-tripping.
    pub fn as_string(&self) -> String {
        match self {
            SubjectMatcher::Anyone => "*".to_owned(),
            SubjectMatcher::Role(r) => format!("role:{r}"),
            SubjectMatcher::Group { provider, name } => {
                let p = match provider {
                    GroupProvider::Any => "*",
                    GroupProvider::Named(p) => p.as_str(),
                    GroupProvider::Unprefixed => "",
                };
                format!("group:{p}:{name}")
            }
            SubjectMatcher::User(u) => format!("user:{u}"),
            SubjectMatcher::Token(t) => format!("token:{t}"),
        }
    }
}

impl fmt::Display for SubjectMatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_string())
    }
}

/// Does the identity's group string `g` match this `group:` subject?
///
/// Split on the **first** colon, so a name containing one stays one name.
fn group_matches(g: &str, provider: &GroupProvider, name: &str) -> bool {
    match (g.split_once(':'), provider) {
        (Some((g_provider, g_name)), GroupProvider::Any) => {
            g_name == name && !g_provider.is_empty()
        }
        (Some((g_provider, g_name)), GroupProvider::Named(p)) => g_name == name && g_provider == p,
        (Some(_), GroupProvider::Unprefixed) => false,
        (None, GroupProvider::Unprefixed) => g == name,
        (None, _) => false,
    }
}

// ── Grants on a node ─────────────────────────────────────────────────────────

/// The grants written on one node.
///
/// An **empty** map is a seal, not an absence — see the module docs. `Node`
/// distinguishes the two with an `Option`; this type only ever describes
/// something that was written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrantMap {
    /// Ordered so `explain` output and any serialisation are stable. Resolution
    /// does not depend on the order — that is [`resolve`]'s whole point — but a
    /// diffable dump does.
    entries: Vec<(SubjectMatcher, Vec<Action>)>,
}

impl GrantMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a subject's verbs. Repeating a subject unions rather than replaces,
    /// for the same reason tiers do.
    pub fn grant(
        mut self,
        subject: SubjectMatcher,
        actions: impl IntoIterator<Item = Action>,
    ) -> Self {
        let actions: Vec<Action> = actions.into_iter().collect();
        if let Some(existing) = self.entries.iter_mut().find(|(s, _)| *s == subject) {
            for a in actions {
                if !existing.1.contains(&a) {
                    existing.1.push(a);
                }
            }
        } else {
            self.entries.push((subject, actions));
        }
        self.entries.sort_by(|a, b| a.0.cmp(&b.0));
        self
    }

    /// A seal: written, and empty.
    pub fn sealed() -> Self {
        Self::default()
    }

    pub fn is_sealed(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[(SubjectMatcher, Vec<Action>)] {
        &self.entries
    }

    /// The verbs this node grants `subject`, with the subject form that matched,
    /// ignoring inheritance.
    fn for_subject(&self, subject: &Subject) -> Vec<(Action, SubjectMatcher)> {
        self.entries
            .iter()
            .filter(|(m, _)| m.matches(subject))
            .flat_map(|(m, actions)| actions.iter().map(move |a| (*a, m.clone())))
            .collect()
    }
}

/// One node on the path from registry to version.
#[derive(Debug, Clone)]
pub struct Node {
    pub tier: Tier,
    /// A label for diagnostics — the registry name, the namespace match, the
    /// package name, the version.
    pub label: String,
    /// `None` inherits; `Some(empty)` seals; `Some(non-empty)` adds.
    pub grants: Option<GrantMap>,
    /// RFC 0015 §4.7 — this node's grants are in **shadow**: they resolve, the
    /// would-have-been is recorded, and nothing is refused because of them.
    ///
    /// Carried on the node rather than on the registry because §4.7 makes it a
    /// property of *a policy at a tier*: an operator migrating one namespace
    /// should not have to shadow the whole registry to do it, and the admin
    /// endpoint lists would-have-beens **per node** precisely so they can tell
    /// which one is still open.
    ///
    /// Only the config-file tiers can carry one: the `grants` table has no
    /// column for it, deliberately. A delegate holding `owners:write` who could
    /// shadow a package's grants could serve everything on it — which is the
    /// same reasoning that keeps sealing out of that table (§4.3), applied to
    /// the mode that fails open rather than the one that fails closed.
    pub dry_run: Option<DryRun>,
}

impl Node {
    pub fn new(tier: Tier, label: impl Into<String>, grants: Option<GrantMap>) -> Self {
        Node {
            tier,
            label: label.into(),
            grants,
            dry_run: None,
        }
    }

    /// This node, in shadow until `until`.
    pub fn shadowed(mut self, dry_run: Option<DryRun>) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// A node that declares nothing and simply inherits.
    pub fn inherits(tier: Tier, label: impl Into<String>) -> Self {
        Node::new(tier, label, None)
    }
}

// ── The resolved set ─────────────────────────────────────────────────────────

/// What a subject may do, after resolution.
///
/// A plain set. There is no `None`, no "unset" and no sentinel that could be
/// read as *everything* — the empty value means empty (§4.3, and survey finding
/// 2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrantSet {
    actions: BTreeSet<Action>,
    /// Which node granted each verb, and under which subject, for `explain`
    /// (§4.8).
    ///
    /// Provenance is the point of that endpoint: a resolved set without it says
    /// *what* you have, and naming the tier **and the subject form** says which
    /// line to edit. Both, because either alone leaves a search — the tier
    /// narrows it to a block and the subject narrows it to a row.
    ///
    /// Recorded here rather than recomputed, so the diagnostic cannot disagree
    /// with the decision. §11.6 tests it as an oracle against the authorization
    /// matrix for the same reason: a diagnostic that can contradict reality is
    /// worse than none, because it is trusted.
    provenance: Vec<Provenance>,
}

/// Where one verb came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub action: Action,
    /// The node's label — `registry:npm1`, `namespace:@acme/billing`.
    pub granted_by: String,
    /// The subject form that matched.
    pub subject: SubjectMatcher,
}

impl GrantSet {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn holds(&self, action: Action) -> bool {
        self.actions.contains(&action)
    }

    pub fn actions(&self) -> impl Iterator<Item = Action> + '_ {
        self.actions.iter().copied()
    }

    /// The node that first granted `action`, if any.
    pub fn granted_by(&self, action: Action) -> Option<&str> {
        self.provenance_for(action).map(|p| p.granted_by.as_str())
    }

    /// Where `action` came from — the node and the subject form.
    pub fn provenance_for(&self, action: Action) -> Option<&Provenance> {
        self.provenance.iter().find(|p| p.action == action)
    }

    /// Every verb, with its provenance, in resolution order.
    pub fn provenance(&self) -> &[Provenance] {
        &self.provenance
    }

    /// A stable key for the *set of verbs*, for caching a filtered document.
    ///
    /// RFC 0015 §11.7 arm 3: *"Filtered, keyed by resolved grant set — callers
    /// sharing a grant set share a cache entry, so the real question is how many
    /// distinct sets an estate has, not how many users."* Phase 0b found that
    /// key load-bearing rather than optional (§13.2), so it is a first-class
    /// operation on the resolved set rather than something a cache layer
    /// improvises.
    ///
    /// # What it is derived from, and what it must not be
    ///
    /// **The verbs, and only the verbs.** Not the identity, not the provenance,
    /// not which tier granted what. Two callers who resolve to the same
    /// permissions see the same filtered document, and the entire value of the
    /// key is that they share one cache entry — a key that mixed in a user id
    /// would be a per-user cache, which is the thing phase 0b measured as
    /// unaffordable.
    ///
    /// Provenance is deliberately excluded even though it is *available*: alice
    /// granted `releases:read` at the registry tier and bob granted it on a
    /// namespace see identical documents, and keying them apart would double the
    /// entries to record a distinction no reader can observe.
    ///
    /// # Why SHA-256 and not `DefaultHasher`
    ///
    /// `DefaultHasher` is randomly seeded per process. That is invisible in a
    /// single-node test and wrong the moment the cache is shared — a Redis or
    /// Postgres cache store would get a different key for the same grant set
    /// from each replica, so every entry would be written by one node and missed
    /// by the others. The cache would appear to work and never hit.
    pub fn cache_key(&self) -> String {
        use sha2::{Digest, Sha256};

        // `BTreeSet`, so iteration is already canonical: the same set produces
        // the same digest regardless of the order the grants were resolved in,
        // which is the property `resolve`'s union gives and §11.2 asserts by
        // shuffling.
        let mut hasher = Sha256::new();
        for action in &self.actions {
            // Length-prefixed rather than delimiter-joined: a delimiter that can
            // occur inside a verb makes two different sets hash alike, and every
            // verb here contains `:`. `signed_url.rs` records the same lesson —
            // "a group holding a separator cannot pose as two".
            let s = action.as_str();
            hasher.update((s.len() as u32).to_le_bytes());
            hasher.update(s.as_bytes());
        }
        // 16 hex characters — 64 bits. A cache key, not a security boundary: a
        // collision serves one grant set's document to another, so it has to be
        // improbable rather than infeasible, and a shorter key keeps the cache
        // index small.
        hex_prefix(&hasher.finalize(), 8)
    }

    /// First writer wins: a verb granted at two tiers reports the outermost,
    /// which is the one an operator most likely meant to edit. The union makes
    /// them equivalent for the decision, so this is purely about which line the
    /// diagnostic points at.
    fn add(&mut self, action: Action, node: &str, subject: &SubjectMatcher) {
        if self.actions.insert(action) {
            self.provenance.push(Provenance {
                action,
                granted_by: node.to_owned(),
                subject: subject.clone(),
            });
        }
    }
}

/// The verbs that survive a seal, for a subject holding them at registry tier.
///
/// RFC 0015 §4.3: a seal stops inheritance *including* of a registry-level
/// `role:admin = ["*"]`, which is what makes it useful and what makes it
/// dangerous. So an administrator can always see what a seal contains, change
/// it, and read who reached it — and can never be locked out of a subtree of
/// their own registry.
///
/// `releases:read`, `releases:list` and `releases:publish` deliberately do **not**
/// survive: the floor is the ability to *administer* the sealed node, never to
/// use it. A subtree nobody can reopen is a denial of service that looks like a
/// configuration.
pub const ADMINISTRATIVE_FLOOR: &[Action] =
    &[Action::OwnersRead, Action::OwnersWrite, Action::AuditRead];

/// Resolve the permissions `subject` holds over the node at the end of `path`.
///
/// `path` runs outermost-first: registry, then any namespace, then package, then
/// version. Nodes that declare nothing are still passed — they are what
/// `explain` walks — they simply contribute nothing.
///
/// # Order independence
///
/// The result depends on the *set* of matching grants, not on the order they
/// were supplied in, because the combining operation is a union. §11.2 asserts
/// this by shuffling; it holds by construction rather than by care.
///
/// # Sealing
///
/// A seal stops its ancestors' grants from flowing past it. It does not disable
/// the nodes beneath it: a grant written directly on a package inside a sealed
/// namespace resolves normally, which is what makes [`ADMINISTRATIVE_FLOOR`]
/// useful rather than ceremonial — the administrator who can still write below a
/// seal has a recovery that does not require reverting the seal itself.
pub fn resolve(path: &[Node], subject: &Subject) -> GrantSet {
    let mut set = GrantSet::default();

    // The deepest seal, if any: everything at or above it stops contributing.
    let seal = path
        .iter()
        .rposition(|n| n.grants.as_ref().is_some_and(GrantMap::is_sealed));
    let start = seal.map_or(0, |i| i + 1);

    for node in &path[start..] {
        let Some(grants) = &node.grants else {
            continue;
        };
        for (action, matcher) in grants.for_subject(subject) {
            set.add(action, &node.label, &matcher);
        }
    }

    // The floor, when a seal cut the path: only what the subject already held at
    // registry tier, and only the administrative verbs.
    if seal.is_some() {
        add_administrative_floor(path.first(), subject, &mut set);
    }

    set
}

/// Re-add the [`ADMINISTRATIVE_FLOOR`] verbs the subject already held at
/// registry tier, after a seal cut them off.
///
/// Only ever called for a sealed path, and only ever *adds*: a subject who held
/// nothing at registry tier gains nothing here. That is what keeps the floor a
/// recovery rather than a privilege — it restores what the seal took, and not
/// one verb more.
fn add_administrative_floor(registry: Option<&Node>, subject: &Subject, set: &mut GrantSet) {
    let Some(registry) = registry else {
        return;
    };
    let Some(grants) = &registry.grants else {
        return;
    };
    for (action, matcher) in grants.for_subject(subject) {
        if ADMINISTRATIVE_FLOOR.contains(&action) {
            set.add(action, &registry.label, &matcher);
        }
    }
}

/// A registry's configured grant hierarchy: the registry node, and its
/// namespaces.
///
/// Package and version tiers are not here — §4.1: *"a registry with 200 000
/// packages will not enumerate them in TOML, let alone their two million
/// versions"* — and arrive from the `policy` table when it exists.
#[derive(Debug, Clone)]
pub struct RegistryGrants {
    /// The ecosystem, which decides the namespace separator.
    ///
    /// Carried here rather than looked up per request: the separator is a
    /// property of the registry a namespace was written for, and a resolver that
    /// had to find it elsewhere could resolve against the wrong one — matching
    /// `com.acme` with `/` instead of `:` silently changes which packages a grant
    /// reaches.
    pub kind: RegistryKind,
    pub registry: Node,
    /// `(match_prefix, node)`, in config order. Matching is by
    /// [`namespace_separator`]; several may match one package, and all of them
    /// contribute, because resolution is a union.
    pub namespaces: Vec<(String, Node)>,
}

impl RegistryGrants {
    /// A registry that grants nothing to anyone.
    ///
    /// Not `Default`, because there is no neutral default here: a registry with
    /// no grants refuses everyone, and a type that produces that state by
    /// accident is how "absence is not everything" (§4.3) gets broken in the
    /// other direction. Callers that want it say so.
    pub fn empty(registry: &str, kind: RegistryKind) -> Self {
        RegistryGrants {
            kind,
            registry: Node::new(
                Tier::Registry,
                format!("registry:{registry}"),
                Some(GrantMap::new()),
            ),
            namespaces: Vec::new(),
        }
    }

    /// The nodes on the path to `package`, outermost first.
    ///
    /// Every matching namespace is included. That is not a "longest prefix wins"
    /// rule and must not become one: replacement is revocation under another
    /// name (§4.3), so a narrower namespace that matched cannot take away what a
    /// broader one granted.
    ///
    /// The package and version tiers are not appended here: they come from the
    /// `policy` table, which the config file cannot enumerate (§4.1).
    pub fn path_for(&self, package: &str) -> Vec<Node> {
        let mut path = vec![self.registry.clone()];
        for (prefix, node) in &self.namespaces {
            if namespace_matches(self.kind, prefix, package) {
                path.push(node.clone());
            }
        }
        path
    }
}

/// The first `n` bytes of a digest, as lower-case hex.
fn hex_prefix(bytes: &[u8], n: usize) -> String {
    bytes.iter().take(n).map(|b| format!("{b:02x}")).collect()
}

// ── Namespaces ───────────────────────────────────────────────────────────────

// ── Shadow mode (§4.7) ───────────────────────────────────────────────────────

/// The keys inside a `grants` block that are **settings rather than subjects**.
///
/// RFC 0015 §4.9 spells the flag `grants.dry_run`, which puts it inside a block
/// that is otherwise a `subject → [verb]` map. That is unambiguous rather than
/// merely conventional: every subject form carries a `:` or is exactly `*`
/// ([`SubjectMatcher::parse`]), so a bare `dry_run` can never be one. A key here
/// is extracted before parsing and is **rejected** as a subject, so a typo like
/// `dry-run` fails loudly as an unknown subject rather than silently becoming a
/// grant to nobody.
pub const RESERVED_GRANT_KEYS: &[&str] = &["dry_run", "dry_run_until"];

/// Whether a node's grants are in shadow mode, and until when.
///
/// # Why this is the most dangerous setting in the document
///
/// §4.7's table is blunt about the direction: for `retention`, dry run means
/// nothing is deleted — the system does less, which is **safe**. For `grants`,
/// dry run means *a request that would be refused is served*, which is
/// **fail-open**.
///
/// It is also the setting that makes §10's migration survivable in practice:
/// enable the new model in shadow, watch a week of real traffic, then enforce.
/// So it ships, and it is constrained rather than merely documented — the
/// expiry is required by the type, not checked by a caller who might forget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRun {
    /// The date it stops applying. **Required**, and config load refuses a date
    /// already past.
    ///
    /// A shadow mode that cannot be forgotten is the entire point: the failure
    /// this guards against is not a wrong decision, it is a right decision
    /// nobody revisited.
    pub until: chrono::NaiveDate,
}

impl DryRun {
    /// Whether the shadow is still in force on `today`.
    ///
    /// An expired shadow **enforces**. That is the fail-closed direction and the
    /// only defensible one: the alternative is a node that quietly keeps serving
    /// what it should refuse because a date passed and nobody noticed, which is
    /// precisely the failure the required expiry exists to prevent.
    pub fn is_active(&self, today: chrono::NaiveDate) -> bool {
        self.until >= today
    }
}

/// The character that separates a namespace from what is under it, per
/// ecosystem.
///
/// RFC 0015 §4.1 carries RFC 0011-bis §4.2's table over unchanged and makes it
/// the definition of "namespace" for every ecosystem. Matching is on segment
/// boundaries using this character, so `@acme/billing` never matches
/// `@acme/billing-internal` — the bug 0011-bis records for `digital` versus
/// `digital.pipeline-tools`.
///
/// Compared **literally, never as a pattern**. A `.` inside a `LIKE` would be
/// harmless; the rule that keeps it literal is the rule that keeps this correct
/// as separators multiply, and the SQL predicate has to agree character for
/// character or the listing becomes more permissive than the download gate.
pub fn namespace_separator(kind: RegistryKind) -> char {
    match kind {
        // Extension ids are `publisher.name`; NuGet ids are dotted too.
        RegistryKind::Openvsx | RegistryKind::VscodeMarketplace | RegistryKind::Nuget => '.',
        // Maven coordinates are `groupId:artifactId`.
        RegistryKind::Maven => ':',
        // SDKMAN has no namespace tier of its own — candidates are flat — but
        // its *listing* coordinate carries the platform under the candidate
        // (`java/linuxx64`, RFC 0010 §5.2), so `/` is the separator that makes
        // a grant written against `java` cover the listing as well as the
        // download. Written out rather than left to the default below, because
        // RFC 0010 §13 records the choice and the default would otherwise be
        // a coincidence.
        RegistryKind::Sdkman => '/',
        // Everything else: npm scopes, Go modules, conda channels, Terraform
        // namespaces, deb components, and the forges.
        _ => '/',
    }
}

/// Whether `package` lies under the namespace `prefix` in `kind`.
///
/// Equality counts — a namespace contains itself — and anything deeper must be
/// separated by the ecosystem's own character.
pub fn namespace_matches(kind: RegistryKind, prefix: &str, package: &str) -> bool {
    if prefix.is_empty() {
        return false;
    }
    if package == prefix {
        return true;
    }
    let sep = namespace_separator(kind);
    package
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with(sep))
}

/// A PAT resolves to its user and a subset of their groups — never a superset.
///
/// RFC 0015 §4.3 states the invariant and why it is worth stating: *"a token that
/// can exceed its owner is a privilege-escalation primitive, which is precisely
/// what a leaked token is worth to an attacker."* This is the check, so the
/// property is enforced rather than assumed of every provider that mints one.
pub fn pat_is_within_owner(pat: &Identity, owner: &Identity) -> bool {
    if pat.user_id != owner.user_id {
        return false;
    }
    if !owner.has_role_at_least(&pat.role) {
        return false;
    }
    pat.groups.iter().all(|g| owner.groups.contains(g))
}

/// Build the group snapshot a PAT is minted with, or name what its creator
/// does not hold.
///
/// The other half of [`pat_is_within_owner`]: that function asserts the subset
/// invariant of a token that already exists, this one is what makes it true at
/// the one moment a token is created. Keeping them adjacent is deliberate —
/// a check with no producer beside it is a check that eventually guards nothing.
///
/// Two rules, both from RFC 0011-bis §4.4:
///
/// - **Comparison is space-stripped**, matching `check_team_visibility` and
///   [`ExploreViewer::normalised_groups`]. One normalisation rule, applied
///   everywhere, so `platform team` typed into a console field and
///   `platformteam` emitted by the IDP are the same group here and in every
///   later comparison.
/// - **What is stored is the owner's string, not the requested one.** A caller
///   who asks for `platform team` gets the exact bytes their `Identity` carries,
///   so a snapshot is always literally a subset of what the provider emitted and
///   every downstream comparison — SQL predicate included — behaves for the PAT
///   exactly as it does for the OIDC session that minted it.
///
/// `Err` carries the requested groups the owner does not hold, in the order they
/// were asked for. Refusing is the whole point: silently dropping them mints a
/// token that is quietly narrower than what its creator asked for, and the first
/// report is a pipeline that cannot see a package with nothing to explain why.
///
/// [`ExploreViewer::normalised_groups`]: crate::entities::ExploreViewer::normalised_groups
pub fn snapshot_pat_groups(
    requested: &[String],
    owner: &Identity,
) -> Result<Vec<String>, Vec<String>> {
    fn normalise(g: &str) -> String {
        g.replace(' ', "")
    }

    let mut snapshot: Vec<String> = Vec::with_capacity(requested.len());
    let mut missing: Vec<String> = Vec::new();

    for want in requested {
        let key = normalise(want);
        match owner.groups.iter().find(|held| normalise(held) == key) {
            // Same group asked for twice is one group on the token, not two.
            Some(held) if snapshot.contains(held) => {}
            Some(held) => snapshot.push(held.clone()),
            None => missing.push(want.clone()),
        }
    }

    if missing.is_empty() {
        Ok(snapshot)
    } else {
        Err(missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::Identity;

    fn subject(role: Role, user: Option<&str>, groups: &[&str]) -> Subject {
        Subject::Identity(Identity {
            user_id: user.map(str::to_owned),
            role,
            auth_provider: None,
            groups: groups.iter().map(|g| (*g).to_owned()).collect(),
        })
    }

    fn anon() -> Subject {
        subject(Role::Anonymous, None, &[])
    }

    fn reg(grants: Option<GrantMap>) -> Node {
        Node::new(Tier::Registry, "registry:reg", grants)
    }

    // ── subject forms ────────────────────────────────────────────────────────

    #[test]
    fn every_subject_form_round_trips() {
        for s in [
            "*",
            "role:user",
            "role:admin",
            "group:oidc1:eng",
            "group:*:eng",
            "user:alice",
            "token:release-bot",
        ] {
            let m = SubjectMatcher::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"));
            assert_eq!(m.as_string(), s, "{s} did not round-trip");
        }
    }

    #[test]
    fn a_malformed_subject_is_refused() {
        for s in [
            "",
            "nope",
            "group:eng",
            "group:oidc1:",
            "user:",
            "token:",
            "role:wizard",
        ] {
            assert!(SubjectMatcher::parse(s).is_err(), "'{s}' should not parse");
        }
    }

    /// The three group shapes match exactly what `[registries.rbac.groups]`
    /// matches today, and no more.
    ///
    /// This is the migration's sharpest edge. `*:eng` and a bare `eng` look
    /// interchangeable and are not: today a bare config key matches a bare group
    /// string and nothing else, so folding the two into one wildcard would make
    /// `eng` start matching `oidc1:eng` on **every deployment, on upgrade** —
    /// the silent widening §7 calls the migration's central risk. Asserted in
    /// both directions, because a widening and a breakage are both wrong and
    /// only one of them is loud.
    #[test]
    fn the_three_group_shapes_match_exactly_what_rbac_matches_today() {
        let prefixed = subject(Role::User, None, &["oidc1:eng"]);
        let bare = subject(Role::User, None, &["eng"]);
        let other_provider = subject(Role::User, None, &["oidc2:eng"]);

        let exact = SubjectMatcher::parse("group:oidc1:eng").unwrap();
        assert!(exact.matches(&prefixed));
        assert!(!exact.matches(&bare));
        assert!(!exact.matches(&other_provider));

        let any = SubjectMatcher::parse("group:*:eng").unwrap();
        assert!(any.matches(&prefixed));
        assert!(any.matches(&other_provider));
        assert!(
            !any.matches(&bare),
            "`*:eng` requires a provider — today's wildcard is only built from a \
             group that has one"
        );

        let unprefixed = SubjectMatcher::parse("group::eng").unwrap();
        assert!(unprefixed.matches(&bare));
        assert!(
            !unprefixed.matches(&prefixed),
            "a bare config key must not start matching provider-prefixed groups"
        );
    }

    /// A group name containing a colon is one group, not two.
    ///
    /// `signed_url.rs` already carries a test for the same hazard; a matcher that
    /// split on every colon would read `group:oidc1:team:a` as provider `oidc1`,
    /// name `team`, and quietly grant to a different group than the one written.
    #[test]
    fn a_group_name_may_contain_a_colon() {
        let m = SubjectMatcher::parse("group:oidc1:team:a").unwrap();
        assert_eq!(
            m,
            SubjectMatcher::Group {
                provider: GroupProvider::Named("oidc1".to_owned()),
                name: "team:a".to_owned()
            }
        );
        assert!(m.matches(&subject(Role::User, None, &["oidc1:team:a"])));
        assert!(!m.matches(&subject(Role::User, None, &["oidc1:team"])));
    }

    #[test]
    fn the_group_provider_wildcard_matches_any_provider() {
        let any = SubjectMatcher::parse("group:*:eng").unwrap();
        assert!(any.matches(&subject(Role::User, None, &["oidc1:eng"])));
        assert!(any.matches(&subject(Role::User, None, &["oidc2:eng"])));
        assert!(!any.matches(&subject(Role::User, None, &["oidc1:sales"])));

        let specific = SubjectMatcher::parse("group:oidc1:eng").unwrap();
        assert!(specific.matches(&subject(Role::User, None, &["oidc1:eng"])));
        assert!(!specific.matches(&subject(Role::User, None, &["oidc2:eng"])));
    }

    /// Role inheritance survives, because `[registries.rbac]` has always had it.
    #[test]
    fn a_role_subject_inherits_downwards() {
        let user = SubjectMatcher::parse("role:user").unwrap();
        assert!(user.matches(&subject(Role::Admin, None, &[])));
        assert!(user.matches(&subject(Role::User, None, &[])));
        assert!(!user.matches(&anon()));
    }

    /// No principal is a machine token yet, so `token:` matches nobody — which
    /// is both the correct reading and the fail-closed one.
    #[test]
    fn a_token_subject_matches_nobody_yet() {
        let t = SubjectMatcher::parse("token:release-bot").unwrap();
        assert!(!t.matches(&subject(Role::Admin, Some("release-bot"), &[])));
    }

    // ── resolution ───────────────────────────────────────────────────────────

    #[test]
    fn a_matching_grant_resolves_and_names_its_tier() {
        let path = [reg(Some(
            GrantMap::new().grant(SubjectMatcher::Anyone, [Action::ReleasesRead]),
        ))];
        let set = resolve(&path, &anon());
        assert!(set.holds(Action::ReleasesRead));
        assert_eq!(set.granted_by(Action::ReleasesRead), Some("registry:reg"));
    }

    /// RFC 0015 §11.2: **a deeper node never narrows.**
    ///
    /// The intuitive implementation is "deepest wins" and it is wrong. Under it
    /// the package grant below would take `source:read` away, which is
    /// revocation — the one thing this model excludes — arriving as a
    /// precedence rule nobody called a deny.
    #[test]
    fn a_deeper_node_never_narrows() {
        let path = [
            reg(Some(GrantMap::new().grant(
                SubjectMatcher::Role(Role::User),
                [Action::ReleasesRead, Action::SourceRead],
            ))),
            Node::new(
                Tier::Package,
                "package:pkg",
                Some(
                    GrantMap::new().grant(SubjectMatcher::Role(Role::User), [Action::ReleasesRead]),
                ),
            ),
        ];
        let set = resolve(&path, &subject(Role::User, None, &[]));
        assert!(set.holds(Action::ReleasesRead));
        assert!(
            set.holds(Action::SourceRead),
            "a smaller set on a deeper node must add nothing, not take away"
        );
    }

    /// §11.2: resolution is order-independent.
    ///
    /// Asserted by shuffling the grants within each node — the property a
    /// precedence rule would have had to earn and a union has by construction.
    #[test]
    fn resolution_is_order_independent() {
        let subj = subject(Role::User, Some("alice"), &["oidc1:eng"]);
        let mut forwards = GrantMap::new()
            .grant(SubjectMatcher::Anyone, [Action::ReleasesRead])
            .grant(
                SubjectMatcher::Group {
                    provider: GroupProvider::Named("oidc1".to_owned()),
                    name: "eng".to_owned(),
                },
                [Action::SourceRead],
            )
            .grant(
                SubjectMatcher::User("alice".to_owned()),
                [Action::ReleasesList],
            );
        let backwards = GrantMap::new()
            .grant(
                SubjectMatcher::User("alice".to_owned()),
                [Action::ReleasesList],
            )
            .grant(
                SubjectMatcher::Group {
                    provider: GroupProvider::Named("oidc1".to_owned()),
                    name: "eng".to_owned(),
                },
                [Action::SourceRead],
            )
            .grant(SubjectMatcher::Anyone, [Action::ReleasesRead]);

        let a = resolve(&[reg(Some(forwards.clone()))], &subj);
        let b = resolve(&[reg(Some(backwards))], &subj);
        assert_eq!(
            a.actions().collect::<Vec<_>>(),
            b.actions().collect::<Vec<_>>()
        );

        // …and repeating a subject unions rather than replaces.
        forwards = forwards.grant(SubjectMatcher::Anyone, [Action::CatalogueBrowse]);
        let c = resolve(&[reg(Some(forwards))], &subj);
        assert!(c.holds(Action::ReleasesRead) && c.holds(Action::CatalogueBrowse));
    }

    /// §11.2: **empty is not all.**
    ///
    /// Survey finding 2 as an invariant. It shipped because an empty list meant
    /// "all registries" in four repository implementations that all agreed with
    /// each other.
    #[test]
    fn a_subject_matched_by_no_grant_resolves_to_nothing() {
        let path = [reg(Some(GrantMap::new().grant(
            SubjectMatcher::User("bob".to_owned()),
            [Action::ReleasesRead],
        )))];
        let set = resolve(&path, &subject(Role::User, Some("alice"), &[]));
        assert!(set.is_empty());
        for action in Action::ALL {
            assert!(!set.holds(*action), "{action} must not be held");
        }
    }

    /// A node that declares nothing inherits; a node that declares an empty map
    /// seals. The `Option` is what keeps those apart.
    #[test]
    fn absence_inherits_and_an_empty_map_seals() {
        let granted = GrantMap::new().grant(SubjectMatcher::Anyone, [Action::ReleasesRead]);

        let inheriting = [
            reg(Some(granted.clone())),
            Node::inherits(Tier::Package, "package:pkg"),
        ];
        assert!(resolve(&inheriting, &anon()).holds(Action::ReleasesRead));

        let sealed = [
            reg(Some(granted)),
            Node::new(Tier::Package, "package:pkg", Some(GrantMap::sealed())),
        ];
        assert!(resolve(&sealed, &anon()).is_empty());
    }

    /// A seal stops inheritance including from a registry-level wildcard.
    #[test]
    fn a_seal_stops_a_registry_wide_wildcard() {
        let path = [
            reg(Some(GrantMap::new().grant(
                SubjectMatcher::Role(Role::Admin),
                Action::ALL.to_vec(),
            ))),
            Node::new(Tier::Namespace, "namespace:ns", Some(GrantMap::sealed())),
            Node::inherits(Tier::Package, "package:pkg"),
        ];
        let set = resolve(&path, &subject(Role::Admin, Some("root"), &[]));
        assert!(
            !set.holds(Action::ReleasesRead),
            "a seal blocks the wildcard"
        );
        assert!(!set.holds(Action::ReleasesPublish));
    }

    /// …but the administrative floor survives it, and nothing else does.
    ///
    /// §7 names this a security control in its own right: a subtree an
    /// administrator cannot reopen is a denial of service that looks like a
    /// configuration.
    #[test]
    fn the_administrative_floor_survives_a_seal() {
        let path = [
            reg(Some(GrantMap::new().grant(
                SubjectMatcher::Role(Role::Admin),
                Action::ALL.to_vec(),
            ))),
            Node::new(Tier::Namespace, "namespace:ns", Some(GrantMap::sealed())),
        ];
        let set = resolve(&path, &subject(Role::Admin, Some("root"), &[]));

        for floor in ADMINISTRATIVE_FLOOR {
            assert!(set.holds(*floor), "{floor} must survive a seal");
        }
        for withheld in [
            Action::ReleasesRead,
            Action::ReleasesList,
            Action::ReleasesPublish,
        ] {
            assert!(!set.holds(withheld), "{withheld} must not survive a seal");
        }
    }

    /// The floor is what the subject *already held*, not a blanket grant.
    ///
    /// An anonymous caller gets nothing out of a seal, however administrative the
    /// verb — otherwise sealing a namespace would open it.
    #[test]
    fn the_floor_grants_nothing_the_subject_did_not_already_hold() {
        let path = [
            reg(Some(GrantMap::new().grant(
                SubjectMatcher::Role(Role::Admin),
                [Action::OwnersWrite, Action::AuditRead],
            ))),
            Node::new(Tier::Namespace, "namespace:ns", Some(GrantMap::sealed())),
        ];
        assert!(resolve(&path, &anon()).is_empty());
    }

    /// A seal stops inheritance; it does not disable the nodes beneath it.
    ///
    /// This is what makes the floor a recovery rather than a ceremony: the
    /// administrator who can still write below a seal does not have to revert it.
    #[test]
    fn a_grant_below_a_seal_still_resolves() {
        let path = [
            reg(Some(
                GrantMap::new().grant(SubjectMatcher::Anyone, [Action::ReleasesRead]),
            )),
            Node::new(Tier::Namespace, "namespace:ns", Some(GrantMap::sealed())),
            Node::new(
                Tier::Package,
                "package:pkg",
                Some(GrantMap::new().grant(
                    SubjectMatcher::User("alice".to_owned()),
                    [Action::ReleasesRead],
                )),
            ),
        ];
        let alice = subject(Role::User, Some("alice"), &[]);
        let set = resolve(&path, &alice);
        assert!(set.holds(Action::ReleasesRead));
        assert_eq!(set.granted_by(Action::ReleasesRead), Some("package:pkg"));

        // …and the registry-wide `*` is still blocked for everyone else.
        assert!(resolve(&path, &subject(Role::User, Some("bob"), &[])).is_empty());
    }

    /// The union runs over the whole path, tier by tier.
    #[test]
    fn grants_union_across_every_tier() {
        let path = [
            reg(Some(
                GrantMap::new().grant(SubjectMatcher::Anyone, [Action::ReleasesRead]),
            )),
            Node::new(
                Tier::Namespace,
                "namespace:ns",
                Some(GrantMap::new().grant(SubjectMatcher::Anyone, [Action::ReleasesList])),
            ),
            Node::new(
                Tier::Package,
                "package:pkg",
                Some(GrantMap::new().grant(SubjectMatcher::Anyone, [Action::SourceRead])),
            ),
            Node::new(
                Tier::Version,
                "version:1.0.0",
                Some(GrantMap::new().grant(SubjectMatcher::Anyone, [Action::CatalogueBrowse])),
            ),
        ];
        let set = resolve(&path, &anon());
        for a in [
            Action::ReleasesRead,
            Action::ReleasesList,
            Action::SourceRead,
            Action::CatalogueBrowse,
        ] {
            assert!(set.holds(a), "{a} should have been unioned in");
        }
        assert_eq!(set.granted_by(Action::SourceRead), Some("package:pkg"));
    }

    // ── namespaces ───────────────────────────────────────────────────────────

    /// RFC 0011-bis's bug, as a test: a prefix must not match a longer name that
    /// merely starts with it.
    #[test]
    fn a_namespace_matches_on_segment_boundaries_only() {
        assert!(namespace_matches(
            RegistryKind::Npm,
            "@acme/billing",
            "@acme/billing"
        ));
        assert!(namespace_matches(
            RegistryKind::Npm,
            "@acme/billing",
            "@acme/billing/cards"
        ));
        assert!(
            !namespace_matches(RegistryKind::Npm, "@acme/billing", "@acme/billing-internal"),
            "a hyphen is not a separator"
        );

        assert!(namespace_matches(
            RegistryKind::Openvsx,
            "digital",
            "digital.pipeline-tools"
        ));
        assert!(!namespace_matches(
            RegistryKind::Openvsx,
            "digital",
            "digitalpipeline"
        ));

        assert!(namespace_matches(
            RegistryKind::Maven,
            "com.acme",
            "com.acme:widget"
        ));
        assert!(
            !namespace_matches(RegistryKind::Maven, "com.acme", "com.acme.internal:widget"),
            "Maven separates the group from the artifact with ':'"
        );
    }

    #[test]
    fn an_empty_namespace_matches_nothing() {
        assert!(!namespace_matches(RegistryKind::Npm, "", "anything"));
    }

    // ── PATs ─────────────────────────────────────────────────────────────────

    /// §4.3's invariant: a PAT can never resolve to more than its user holds.
    #[test]
    fn a_pat_cannot_exceed_its_owner() {
        let owner = Identity {
            user_id: Some("alice".to_owned()),
            role: Role::User,
            auth_provider: None,
            groups: vec!["oidc1:eng".to_owned(), "oidc1:qa".to_owned()],
        };

        let subset = Identity {
            groups: vec!["oidc1:eng".to_owned()],
            ..owner.clone()
        };
        assert!(pat_is_within_owner(&subset, &owner));

        let extra_group = Identity {
            groups: vec!["oidc1:eng".to_owned(), "oidc1:admins".to_owned()],
            ..owner.clone()
        };
        assert!(!pat_is_within_owner(&extra_group, &owner));

        let escalated = Identity {
            role: Role::Admin,
            ..owner.clone()
        };
        assert!(!pat_is_within_owner(&escalated, &owner));

        let someone_else = Identity {
            user_id: Some("bob".to_owned()),
            ..owner.clone()
        };
        assert!(!pat_is_within_owner(&someone_else, &owner));
    }

    fn pat_owner() -> Identity {
        Identity {
            user_id: Some("alice".to_owned()),
            role: Role::User,
            auth_provider: None,
            groups: vec![
                "oidc1:eng".to_owned(),
                "oidc1:qa".to_owned(),
                "platform team".to_owned(),
            ],
        }
    }

    #[test]
    fn a_snapshot_of_nothing_is_the_old_behaviour() {
        assert_eq!(snapshot_pat_groups(&[], &pat_owner()), Ok(vec![]));
    }

    #[test]
    fn a_snapshot_keeps_the_requested_order() {
        assert_eq!(
            snapshot_pat_groups(
                &["oidc1:qa".to_owned(), "oidc1:eng".to_owned()],
                &pat_owner()
            ),
            Ok(vec!["oidc1:qa".to_owned(), "oidc1:eng".to_owned()])
        );
    }

    /// A group the creator does not hold is refused, not dropped. A quietly
    /// narrower token is a pipeline that cannot see a package with nothing on
    /// screen to explain why.
    #[test]
    fn a_snapshot_names_what_the_owner_does_not_hold() {
        assert_eq!(
            snapshot_pat_groups(
                &[
                    "oidc1:eng".to_owned(),
                    "oidc1:admins".to_owned(),
                    "finance".to_owned(),
                ],
                &pat_owner()
            ),
            Err(vec!["oidc1:admins".to_owned(), "finance".to_owned()])
        );
    }

    /// Space-stripped on both sides, matching `check_team_visibility` and
    /// `ExploreViewer::normalised_groups`.
    #[test]
    fn a_snapshot_compares_groups_space_stripped() {
        assert_eq!(
            snapshot_pat_groups(&["platformteam".to_owned()], &pat_owner()),
            Ok(vec!["platform team".to_owned()]),
            "stored as the owner holds it, so every later comparison sees the \
             same bytes the OIDC session would have carried"
        );
    }

    #[test]
    fn a_group_asked_for_twice_is_stored_once() {
        assert_eq!(
            snapshot_pat_groups(
                &[
                    "oidc1:eng".to_owned(),
                    "oidc1: eng".to_owned(),
                    "oidc1:eng".to_owned(),
                ],
                &pat_owner()
            ),
            Ok(vec!["oidc1:eng".to_owned()])
        );
    }

    /// The two functions are one rule from two sides: whatever the snapshot
    /// produces must satisfy the invariant check.
    #[test]
    fn a_snapshot_always_satisfies_the_invariant() {
        let owner = pat_owner();
        let groups =
            snapshot_pat_groups(&["platformteam".to_owned(), "oidc1:qa".to_owned()], &owner)
                .expect("all held");
        let pat = Identity {
            groups,
            ..owner.clone()
        };
        assert!(pat_is_within_owner(&pat, &owner));
    }

    #[test]
    fn an_owner_with_no_groups_can_snapshot_nothing() {
        let owner = Identity {
            groups: vec![],
            ..pat_owner()
        };
        assert_eq!(snapshot_pat_groups(&[], &owner), Ok(vec![]));
        assert_eq!(
            snapshot_pat_groups(&["oidc1:eng".to_owned()], &owner),
            Err(vec!["oidc1:eng".to_owned()])
        );
    }
}
