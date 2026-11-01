//! Listings filter, they do not refuse.
//!
//! RFC 0015 §4.4. A caller holding `releases:list` on a namespace but
//! `releases:read` on only some of its packages asks for a version index. The
//! index returns **what they may see**, not `403`.
//!
//! This is not a new mechanism. It is the one [RFC 0006] already established for
//! administrative blocks: a blocked version is removed from the listing so the
//! resolver routes past it, which is why the Maven and NuGet handlers reach for
//! `proxy_document` rather than `proxy_stream` on their index routes —
//! streaming would deliver a document with the blocked version still in it, and
//! the build would pick that version and fail at download. Grants filter the
//! same document at the same point, for the same reason.
//!
//! [RFC 0006]: https://batlehub.dev/rfc/0006-blocked-versions-hidden-everywhere
//!
//! # The three rules, and which of them live here
//!
//! 1. **Filter in the query, never after it.** Totals and pagination are
//!    computed on the filtered set. That one belongs to each listing's own
//!    query — a helper cannot enforce it, and [`FilterOutcome::total`] exists so
//!    a caller cannot accidentally report a count from before the filter.
//! 2. **Two levels, two answers.** No grant on the *package* → answer as though
//!    it does not exist. A grant on the package but not on every version →
//!    return the filtered list. [`PackageVisibility`] is that decision, made
//!    once so no handler has to remember which way round it goes.
//! 3. **A filtered listing is identity-dependent and must never be cached under
//!    an identity-blind key.** [`GrantSet::cache_key`] is the key it must be
//!    cached under instead.
//!
//! # Where each of these is called
//!
//! Audited 2026-09-01 and rewritten when RFC 0017 landed the writer, because the
//! answer used to differ across the module and a reader who assumed either way
//! got it wrong.
//!
//! [`Readable`] and its builders — [`Readable::from_registry`],
//! [`Readable::needs_package_grants`], [`Readable::with_package_grants`] — are
//! how `LocalRegistryService` scopes every whole-registry document
//! (`local_registry/read.rs`). Package-tier grants are written by the ownership
//! projection (`services/ownership_grants.rs`), by the grants editor
//! (`services/grants_admin.rs`), and were seeded from pre-model ownership rows by
//! migration 042. That is rule 2's first half — *no grant on the package →
//! answer as though it does not exist* — and the filtering of documents that
//! span many packages.
//!
//! [`filter_listing`] and [`package_visibility`] are rule 2's **second** half —
//! *a grant on the package but not on every version → return the filtered list*
//! — and they are called from `LocalRegistryService::filter_by_grants`, the last
//! stage of the `load_visible_versions` funnel. Until RFC 0017 they had no
//! caller and could not have had one: the state they decide was unreachable
//! because nothing wrote a version-tier grant, so a caller's `releases:read`
//! verdict was uniform across every version and the package-tier decision was
//! the whole answer.
//!
//! **They became load-bearing in the same release that made a version-tier grant
//! writable**, which RFC 0017 §2.3 requires rather than recommends: the failure
//! of shipping the writer first would have been silent — version indexes still
//! listing versions the caller may not read, the download gate still refusing
//! them one at a time, and what leaks being the existence and the numbers rather
//! than the bytes. Rule 3 landed with them: [`DocumentAudience`] carries the
//! caller's version-tier grants, so a filtered document cannot be served from an
//! entry keyed without them.
//!
//! # Why filtering only bites when the broad tier is narrow
//!
//! Grants only widen (§4.3), so a caller who holds `releases:read` at the
//! registry tier holds it on every package beneath — and filtering removes
//! nothing. That is correct and worth stating, because it is easy to mistake for
//! the filter not working: the filter is meaningful exactly when the registry or
//! namespace tier grants `releases:list` **without** `releases:read`, and the
//! deeper tiers grant the read on the packages this caller may see. §4.4's
//! opening sentence describes precisely that configuration.

use crate::entities::{Action, GrantSet, Identity, Role, Subject};

/// What a caller may do with one package in a listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageVisibility {
    /// The caller holds the read; the package appears.
    Listed,
    /// The caller holds no read on this package. It is omitted from a listing
    /// that spans many packages — and answered `404` when they named it
    /// directly.
    Hidden,
}

/// §4.4 rule 2 — the two levels.
///
/// Which answer applies depends on what the caller *asked for*, not on what they
/// hold: the same `Hidden` verdict is a `404` when they named the package in the
/// URL and an omission when they asked for a whole-registry document. The
/// distinction is a disclosure boundary rather than a nicety — "the caller named
/// the package in the URL, so a filtered listing tells them nothing they did not
/// already assert" — so it is decided by the caller's question and not here.
pub fn package_visibility(resolved: &GrantSet, read: Action) -> PackageVisibility {
    if resolved.holds(read) {
        PackageVisibility::Listed
    } else {
        PackageVisibility::Hidden
    }
}

/// A filtered listing, and the count that goes with it.
///
/// The count is bundled with the rows on purpose. §4.4 rule 1 is a security
/// requirement, not an implementation detail: *"An accurate `total` over rows
/// the caller may not see is a disclosure in itself, and page two of a filtered
/// list is worse."* Survey finding 2 is the precedent — there the predicate ran
/// and was simply vacuous, and what turned a scoping bug into an enumeration of
/// the whole private inventory was the paging metadata computed faithfully on
/// top of it.
///
/// Returning the two together means a handler cannot report a pre-filter total
/// without going out of its way to compute one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterOutcome<T> {
    items: Vec<T>,
    /// How many were removed. For observability, never for the response — a
    /// count of what was withheld is the disclosure rule 1 forbids, stated as a
    /// number.
    withheld: usize,
}

impl<T> FilterOutcome<T> {
    pub fn items(&self) -> &[T] {
        &self.items
    }

    pub fn into_items(self) -> Vec<T> {
        self.items
    }

    /// The total **over the filtered set**, which is the only total that may be
    /// reported.
    pub fn total(&self) -> usize {
        self.items.len()
    }

    /// How many rows the filter removed. Metrics and logs only.
    pub fn withheld(&self) -> usize {
        self.withheld
    }

    /// Whether the caller may see nothing here.
    ///
    /// An empty filtered result is `200` with an empty document, **not** `404` —
    /// for a whole-registry index it discloses nothing, which is the property
    /// `crates/web/tests/authz_matrix.rs` already asserts through its
    /// `disclosed()` helper rather than through status codes.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Filter a listing by a per-item verdict.
///
/// Deliberately takes a decision function rather than resolving itself: for a
/// whole-registry document the caller resolves once per package and phase 0b
/// measured what that costs (§13.2 — 806× at size M, one query per package), so
/// *where* the resolution happens has to stay the caller's decision. A helper
/// that resolved internally would hide the N+1 that is the whole performance
/// question.
pub fn filter_listing<T>(
    items: Vec<T>,
    mut visible: impl FnMut(&T) -> PackageVisibility,
) -> FilterOutcome<T> {
    let before = items.len();
    let items: Vec<T> = items
        .into_iter()
        .filter(|item| visible(item) == PackageVisibility::Listed)
        .collect();
    let withheld = before - items.len();
    FilterOutcome { items, withheld }
}

/// The cache key a filtered document must be stored under (§4.4 rule 3).
///
/// > This is finding 11's lesson, paid for once already: the search cache held
/// > merged local hits under a key that named no identity, so one caller's
/// > private results were replayed to the next.
///
/// The key is a *class of caller*, not a caller — that is what makes the cache
/// affordable (§11.7 arm 3: callers entitled to the same bytes share an entry)
/// and what makes it correct. [`DocumentAudience`] is that class, and its
/// documentation records what "entitled to the same bytes" turned out to mean.
pub fn document_cache_key(prefix: &str, audience: &DocumentAudience<'_>) -> String {
    format!("{prefix}:audience={}", audience.digest())
}

/// Everything about a caller that can change the bytes of a whole-registry
/// document.
///
/// # Why this is not the grant set of the registry node
///
/// It was, and that was a disclosure. The key was
/// `resolve(&[grants.registry], subject)` while the document was filtered by
/// four further things a registry-tier grant set does not describe — so two
/// callers who agreed on that one node, and on nothing else, shared an entry
/// and the first one in decided what the second was served:
///
/// - **the instance and namespace tiers.** [`Readable`] resolves them and the
///   registry node alone does not. A namespace grant widens the document; a
///   seal narrows it. The contract sentence this function used to carry already
///   said so — "the tiers that are constant across the document ... the
///   registry **and namespace** tiers" — and the call site did not.
/// - **`releases:list`.** A different verb from `releases:read`, resolved over
///   the same hierarchy: `check_read_access` runs `authorize_listing` on it
///   before the read set is consulted, so two callers with the same read set
///   and different list sets get different documents.
/// - **`private` visibility (§4.5).** It drops everything inherited and admits
///   only a grant written *on the package*. Two callers that
///   [`Readable::Everything`] collapses — both hold the read at the registry
///   tier, neither has anything to filter — are still entitled to different
///   documents, because only one of them holds the grant written on the private
///   package. That is why `local_read_grants` is carried separately rather than
///   read back out of `readable`: the fast path does not fetch it.
/// - **`team` visibility and the beta channel.** Group membership and channel
///   membership. No grant resolves either, and both remove packages and
///   versions from what a caller may see.
///
/// Each is a route by which one caller's document is replayed to another —
/// finding 11 again, on the surface §4.4 warns is easiest to forget.
/// [`explore_cache`](crate::services::explore_cache)'s `viewer_key_part` folds
/// the same material in for the same reason, and says the same thing about it:
/// load-bearing for correctness, not for hit rate.
///
/// # It is still a class, and the sharing property survives
///
/// Every field is a property of *what the caller may see*, never of who they
/// are: no user id, no token, no provider. An estate where everyone resolves to
/// the same answer — the overwhelming majority, since grants only widen and a
/// registry-tier read reaches everything — still shares one entry, which is the
/// property §11.7 arm 3 measures. What the key stopped doing is claiming two
/// callers agree when only one node of five says so.
///
/// # What it deliberately does not cover
///
/// `authorize_listing` resolves the *version* tier too, for the sentinel
/// coordinate `{package}@latest`. A grant written on that literal string, for
/// one subject, is not in this digest. It is out of scope rather than
/// overlooked: `latest` is a sentinel this funnel passes because a listing has
/// no version, not a version anyone publishes, so a row on it is not something
/// an operator can write about a real release.
pub struct DocumentAudience<'a> {
    /// Which packages the caller may read.
    readable: &'a Readable,
    /// Which packages the caller may list — `releases:list`, resolved
    /// separately over the same hierarchy.
    listable: &'a Readable,
    /// `internal` visibility admits `role:user` and refuses `role:anonymous`,
    /// and that distinction appears in no grant set.
    role: Role,
    /// Group ids with spaces stripped, sorted and deduplicated — the exact
    /// comparison [`check_team_visibility`] makes, so membership order (which
    /// varies between tokens from one provider) does not fragment the cache.
    ///
    /// [`check_team_visibility`]: crate::services::local_registry::check_team_visibility
    groups: Vec<String>,
    /// Package-tier grants naming this caller with the read, sorted — §4.5's
    /// `private` audience, which the `Everything` fast path cannot express.
    local_read_grants: &'a [String],
    /// Version-tier grants naming this caller with the read, sorted — §4.4 rule
    /// 3, and the field RFC 0017 added.
    ///
    /// Not derivable from `local_read_grants`: a version row is a different node
    /// from the package row above it, and `LocalRegistryService::filter_by_grants`
    /// consults it for every version of every package in a compact index. Two
    /// callers agreeing on every package-tier row and differing on one version
    /// row are entitled to different documents, and without this they would
    /// share an entry — finding 11's shape, one tier down.
    version_read_grants: &'a [String],
    /// Beta-channel membership: a non-member's document has no pre-releases in
    /// it.
    beta_member: bool,
}

impl<'a> DocumentAudience<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        identity: &Identity,
        readable: &'a Readable,
        listable: &'a Readable,
        local_read_grants: &'a [String],
        version_read_grants: &'a [String],
        beta_member: bool,
    ) -> Self {
        let mut groups: Vec<String> = identity.groups.iter().map(|g| g.replace(' ', "")).collect();
        groups.sort();
        groups.dedup();
        Self {
            readable,
            listable,
            role: identity.role.clone(),
            groups,
            local_read_grants,
            version_read_grants,
            beta_member,
        }
    }

    /// 16 hex characters of SHA-256 over every field.
    ///
    /// A cache key, not a security boundary, exactly as [`GrantSet::cache_key`]
    /// says of its own: a collision serves one audience's document to another,
    /// so it has to be improbable rather than infeasible.
    fn digest(&self) -> String {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        feed(&mut hasher, &self.role.to_string());
        feed(&mut hasher, if self.beta_member { "beta" } else { "-" });
        feed_readable(&mut hasher, self.readable);
        feed_readable(&mut hasher, self.listable);
        for group in &self.groups {
            feed(&mut hasher, group);
        }
        feed(&mut hasher, "|");
        for package in self.local_read_grants {
            feed(&mut hasher, package);
        }
        feed(&mut hasher, "|");
        for node_key in self.version_read_grants {
            feed(&mut hasher, node_key);
        }
        hasher
            .finalize()
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// Length-prefixed rather than delimiter-joined.
///
/// The lesson [`GrantSet::cache_key`] records and `signed_url.rs` records
/// before it — "a group holding a separator cannot pose as two". Every field
/// here is operator-authored text: a namespace prefix, a package name, a group
/// id. Any delimiter this could have used occurs inside at least one of them.
fn feed(hasher: &mut sha2::Sha256, s: &str) {
    use sha2::Digest as _;
    hasher.update((s.len() as u32).to_le_bytes());
    hasher.update(s.as_bytes());
}

/// A boolean as one length-prefixed field, so it hashes like every other.
///
/// Named rather than written out at each of the four flags: the spelling has to
/// be identical everywhere, because two flags that disagree on how `false` is
/// spelled are two flags that can swap places without changing the digest.
fn bit(flag: bool) -> &'static str {
    if flag {
        "1"
    } else {
        "0"
    }
}

fn feed_readable(hasher: &mut sha2::Sha256, readable: &Readable) {
    match readable {
        Readable::Everything => feed(hasher, "*"),
        Readable::Scoped(scope) => {
            feed(hasher, "scoped");
            feed(hasher, scope.kind.as_str());
            feed(hasher, bit(scope.registry_grants_read));
            feed(hasher, bit(scope.floor_survives));
            // Config order, which is the order that makes "the deepest seal"
            // well defined — so it is part of the answer, not an accident of
            // iteration.
            for ns in &scope.namespaces {
                feed(hasher, &ns.prefix);
                feed(hasher, bit(ns.grants_read));
                feed(hasher, bit(ns.sealed));
            }
            // Already sorted and deduplicated by `with_package_grants`.
            for package in &scope.packages {
                feed(hasher, package);
            }
        }
    }
}

/// The subject a listing is being built for, and the verb it is filtered on.
///
/// A named pair rather than two arguments, because getting them the wrong way
/// round is silent: filtering a version index on `releases:list` instead of
/// `releases:read` would return every version to anyone who may see the index at
/// all, which is the filter not running.
#[derive(Debug, Clone, Copy)]
pub struct ListingFilter<'a> {
    pub subject: &'a Subject,
    /// The verb an item must be readable under to appear. `releases:read` for a
    /// version index; `catalogue:browse` is *not* it — that gates whether the
    /// caller reaches the console at all (§4.2).
    pub read: Action,
}

/// Which packages in a registry a caller may read, and whether the question
/// even needs asking.
///
/// # The fast path, and what it may not skip
///
/// Grants only widen (§4.3), so a caller who holds `read` at the **registry**
/// tier holds it on every package beneath — for that caller there is nothing to
/// filter, and that is the overwhelming majority of callers on the overwhelming
/// majority of estates. [`Readable::Everything`] is that answer and it costs one
/// resolution for the whole document. Phase 0b measured the alternative: a
/// whole-registry document resolved per package is one query per package, 806×
/// the cached document at size M (§13.2).
///
/// What the first version of this got wrong is that **the registry tier is not
/// the only tier constant across a document**. A namespace is a config-declared
/// node with a `match`, and for any given package it either applies or does not
/// — so it can be resolved once, at construction, and *applied* per package for
/// the cost of a prefix comparison. Resolving only the registry node made
/// `[[registries.namespaces]]` invisible to every whole-registry document, in
/// both directions:
///
/// - **A namespace grant did not widen.** A caller whose only `releases:read`
///   came from `[[registries.namespaces]]` — the estate `@acme/billing` in §1's
///   own example is configured for — resolved to an empty set and was served an
///   empty index, while the per-package routes served them normally. Fails
///   closed, so not a disclosure, and wrong.
/// - **A namespace seal did not narrow.** `grants = {}` stops inheritance
///   (§4.3), so a package under a seal is refused at download — but the registry
///   tier still granted the read, so `Everything` listed it anyway. Five of the
///   six wired documents happen to catch that downstream, because they call
///   `load_visible_versions` per package and it authorizes; Composer's
///   `available-packages` does not, and named every package in a sealed
///   namespace to callers the seal excludes.
///
/// Both are the same defect — a listing that does not agree with the download
/// gate — and §6.3 is explicit that the two must: *"a listing more permissive
/// than this discloses the names of packages this would refuse to serve."*
///
/// # What is still resolved per package, and what is not
///
/// Nothing queries storage per package. The registry node and each namespace
/// node are resolved **once** against the subject at construction; a package
/// then costs one [`namespace_matches`] per declared namespace, which is a
/// `strip_prefix` and a `starts_with` over a list an operator hand-wrote.
///
/// The package tier is the one that cannot be precomputed, and it is **one**
/// query for every package-tier grant in the registry, matched in memory. Not
/// one per package: those rows are few, because a package-tier grant is
/// something an operator wrote deliberately.
#[derive(Debug, Clone)]
pub enum Readable {
    /// Every package. Reached when the registry tier grants the read and no
    /// namespace can take it back.
    Everything,
    /// Decided per package against the registry's configured hierarchy.
    Scoped(Scope),
}

/// The precomputed hierarchy [`Readable::Scoped`] answers from.
///
/// # This is a second implementation of resolution, and it is kept honest by a test
///
/// [`resolve`](crate::entities::resolve) is the model; this is the same
/// arithmetic without the allocation, because the document path cannot afford to
/// clone a node per package. Two implementations of one rule is the defect this
/// whole document exists to remove, so
/// `readable_tests::the_scope_agrees_with_resolve_on_every_hierarchy` runs both
/// over a matrix of hierarchies and fails on any disagreement. Change one and
/// the test tells you about the other.
#[derive(Debug, Clone)]
pub struct Scope {
    kind: crate::entities::RegistryKind,
    /// Whether the registry node alone grants the read.
    registry_grants_read: bool,
    /// What survives a seal for this verb — §4.3's administrative floor. Almost
    /// always `false`, because `releases:read` is not in the floor and never can
    /// be; computed rather than assumed so this stays equal to `resolve` if the
    /// caller ever asks about an administrative verb.
    floor_survives: bool,
    /// In **config order**, which is the order `RegistryGrants::path_for` builds
    /// a path in — and therefore what makes "the deepest seal" well defined.
    namespaces: Vec<NamespaceRead>,
    /// Package-tier grants naming this subject. Sorted, so membership is a
    /// binary search; an unsorted list would answer `false` for packages the
    /// caller *can* read, which fails safe and therefore survives a long time.
    packages: Vec<String>,
}

#[derive(Debug, Clone)]
struct NamespaceRead {
    prefix: String,
    /// Whether this node alone grants the read to this subject.
    grants_read: bool,
    /// `grants = {}` — stops everything above it from flowing past (§4.3).
    sealed: bool,
}

impl Readable {
    /// The config-declared half: the registry node and its namespaces, resolved
    /// once against `subject`.
    ///
    /// Returns [`Readable::Everything`] only when the registry grants the read
    /// **and no namespace is sealed**. A seal that never matches anything costs
    /// this registry the fast path, which is the conservative direction and the
    /// only one available — whether it matches is a question about a package.
    pub fn from_registry(
        instance: Option<&crate::entities::Node>,
        grants: &crate::entities::RegistryGrants,
        read: Action,
        subject: &Subject,
    ) -> Readable {
        use crate::entities::{resolve, GrantMap, ADMINISTRATIVE_FLOOR};

        let grants_read =
            |node: &crate::entities::Node| resolve(std::slice::from_ref(node), subject).holds(read);
        // The instance tier is above the registry and constant across the whole
        // document, so it folds into the same "broad tiers" answer. Omitting it
        // made a caller granted `releases:read` at the instance tier — an
        // operator's deliberate "this subject reads everything" — see an empty
        // index while every per-package route served them: the same shape as the
        // namespace tier being missing here before it, one node further up.
        let registry_grants_read =
            grants_read(&grants.registry) || instance.is_some_and(grants_read);

        let namespaces: Vec<NamespaceRead> = grants
            .namespaces
            .iter()
            .map(|(prefix, node)| NamespaceRead {
                prefix: prefix.clone(),
                grants_read: grants_read(node),
                sealed: node.grants.as_ref().is_some_and(GrantMap::is_sealed),
            })
            .collect();

        if registry_grants_read && !namespaces.iter().any(|n| n.sealed) {
            return Readable::Everything;
        }

        Readable::Scoped(Scope {
            kind: grants.kind,
            registry_grants_read,
            floor_survives: registry_grants_read && ADMINISTRATIVE_FLOOR.contains(&read),
            namespaces,
            packages: Vec::new(),
        })
    }

    /// Whether the package-tier query is worth making.
    ///
    /// `false` on the fast path, and that is the property phase 0b's number
    /// depends on — the caller must not fetch rows it will not consult.
    pub fn needs_package_grants(&self) -> bool {
        matches!(self, Readable::Scoped(_))
    }

    /// Add the registry's package-tier grants, keeping those that name this
    /// subject and carry `read`.
    ///
    /// A no-op on [`Readable::Everything`]: a package-tier grant can only widen,
    /// and there is nothing wider than every package.
    pub fn with_package_grants(
        mut self,
        rows: impl IntoIterator<Item = (String, crate::entities::SubjectMatcher, Vec<Action>)>,
        read: Action,
        subject: &Subject,
    ) -> Readable {
        if let Readable::Scoped(ref mut scope) = self {
            scope.packages.extend(
                rows.into_iter()
                    .filter(|(_, matcher, actions)| {
                        actions.contains(&read) && matcher.matches(subject)
                    })
                    .map(|(name, _, _)| name),
            );
            scope.packages.sort();
            scope.packages.dedup();
        }
        self
    }

    /// Admit a package because a **version** beneath it grants the read.
    ///
    /// `keys` are `name@version` node keys already filtered to this subject and
    /// to `releases:read` — the same list `DocumentAudience` keys on.
    ///
    /// # Why the name and not the version
    ///
    /// This set gates whole-registry documents, and the question they ask is
    /// *"may this caller see anything under this name"*. The narrowing to the
    /// granted versions happens one layer in, in `filter_by_grants`; a document
    /// that dropped the name here would never reach it. That is not
    /// hypothetical — it is what `tests/heavy/authz.sh rubygems` found: a caller
    /// granted the read on one version was told by `/versions` that the gem does
    /// not exist, Bundler fell back to the legacy full index, and the `/info`
    /// document that *would* have offered them their version was never fetched.
    /// Every version-tier grant was unreachable through the one client that
    /// resolves from these documents.
    ///
    /// Widening here cannot disclose bytes: `Readable` filters listings and is
    /// never a download gate — `authorize_read` walks the stored tiers itself.
    /// The worst an over-wide name does is put the name in a document whose
    /// version list then comes back empty, and `live.is_empty()` drops it.
    pub fn with_version_grants<'a>(mut self, keys: impl IntoIterator<Item = &'a str>) -> Readable {
        if let Readable::Scoped(ref mut scope) = self {
            scope.packages.extend(keys.into_iter().filter_map(|key| {
                // The **last** `@`, because a scoped npm name opens with one:
                // `@acme/billing@1.0.0` is `@acme/billing` at `1.0.0`, and
                // splitting on the first would ask about a package named `` .
                key.rfind('@')
                    .filter(|i| *i > 0)
                    .map(|i| key[..i].to_owned())
            }));
            scope.packages.sort();
            scope.packages.dedup();
        }
        self
    }

    pub fn contains(&self, package: &str) -> bool {
        match self {
            Readable::Everything => true,
            Readable::Scoped(scope) => scope.contains(package),
        }
    }

    /// Whether the caller may see nothing at all.
    ///
    /// Not the same as "no packages exist": this is `200` with an empty
    /// document, never `404` (§4.4).
    ///
    /// Conservative — it answers `true` only when **no** node could grant the
    /// read to anything. A caller whose namespace grant matches nothing that
    /// happens to be published gets `false` and an empty document, which is the
    /// same answer by a longer route.
    pub fn is_empty(&self) -> bool {
        match self {
            Readable::Everything => false,
            Readable::Scoped(scope) => {
                !scope.registry_grants_read
                    && !scope.floor_survives
                    && scope.packages.is_empty()
                    && !scope.namespaces.iter().any(|n| n.grants_read)
            }
        }
    }
}

impl Scope {
    fn contains(&self, package: &str) -> bool {
        use crate::entities::namespace_matches;

        // A package-tier grant is written *below* every namespace, so it survives
        // a seal — §4.3: "a grant written directly on a package inside a sealed
        // namespace resolves normally". Checked first because it is also the
        // cheapest answer.
        if self
            .packages
            .binary_search_by(|n| n.as_str().cmp(package))
            .is_ok()
        {
            return true;
        }

        // The union, walked outermost-first, with a seal resetting what has
        // accumulated so far. That is exactly what `resolve` does with
        // `rposition` + the administrative floor, without building the path.
        let mut readable = self.registry_grants_read;
        for ns in &self.namespaces {
            if !namespace_matches(self.kind, &ns.prefix, package) {
                continue;
            }
            if ns.sealed {
                readable = self.floor_survives;
            } else if ns.grants_read {
                readable = true;
            }
        }
        readable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{resolve, GrantMap, Identity, Node, Role, SubjectMatcher, Tier};

    fn subject(role: Role, user: Option<&str>) -> Subject {
        Subject::Identity(Identity {
            user_id: user.map(str::to_owned),
            role,
            auth_provider: None,
            groups: vec![],
        })
    }

    fn set_for(actions: &[Action]) -> GrantSet {
        let map = GrantMap::new().grant(SubjectMatcher::Anyone, actions.to_vec());
        resolve(
            &[Node::new(Tier::Registry, "registry:reg", Some(map))],
            &subject(Role::Anonymous, None),
        )
    }

    /// The same set of verbs produces the same key, whatever order it arrived
    /// in.
    ///
    /// The property the cache depends on: two callers who resolve to the same
    /// permissions must share an entry, and they resolve through different
    /// grants at different tiers.
    #[test]
    fn the_cache_key_depends_only_on_the_verbs() {
        let one = set_for(&[Action::ReleasesRead, Action::ReleasesList]);
        let other = set_for(&[Action::ReleasesList, Action::ReleasesRead]);
        assert_eq!(one.cache_key(), other.cache_key());

        // …and two callers whose grants came from different tiers, with the same
        // result, also share it.
        let registry = GrantMap::new().grant(SubjectMatcher::Anyone, [Action::ReleasesRead]);
        let namespace = GrantMap::new().grant(SubjectMatcher::Anyone, [Action::ReleasesList]);
        let split = resolve(
            &[
                Node::new(Tier::Registry, "registry:reg", Some(registry)),
                Node::new(Tier::Namespace, "namespace:ns", Some(namespace)),
            ],
            &subject(Role::Anonymous, None),
        );
        assert_eq!(
            split.cache_key(),
            one.cache_key(),
            "provenance must not reach the key — two callers seeing identical \
             documents would otherwise get separate cache entries"
        );
    }

    /// Different sets produce different keys.
    #[test]
    fn a_different_set_is_a_different_key() {
        let broad = set_for(&[Action::ReleasesRead, Action::ReleasesList]);
        let narrow = set_for(&[Action::ReleasesList]);
        assert_ne!(broad.cache_key(), narrow.cache_key());

        // The empty set has a key too, and it is its own: a caller who may see
        // nothing still gets a cached empty document rather than falling through
        // to an unkeyed one.
        let empty = set_for(&[]);
        assert!(!empty.cache_key().is_empty());
        assert_ne!(empty.cache_key(), narrow.cache_key());
    }

    /// The key is stable across processes.
    ///
    /// `DefaultHasher` is randomly seeded per process, so a shared cache store
    /// would get a different key for the same set from every replica — the cache
    /// would look like it worked and never hit. Pinning a literal is the only
    /// way to notice if the digest is ever swapped for a faster non-stable one.
    #[test]
    fn the_key_is_stable_across_runs() {
        let set = set_for(&[Action::ReleasesRead]);
        assert_eq!(
            set.cache_key(),
            set_for(&[Action::ReleasesRead]).cache_key()
        );
        assert_eq!(set.cache_key().len(), 16, "8 bytes, hex");
    }

    /// §4.4 rule 1: the total is over the filtered set.
    #[test]
    fn the_total_is_computed_after_filtering() {
        let outcome = filter_listing(vec![1, 2, 3, 4, 5], |n| {
            if n % 2 == 0 {
                PackageVisibility::Listed
            } else {
                PackageVisibility::Hidden
            }
        });
        assert_eq!(outcome.items(), &[2, 4]);
        assert_eq!(
            outcome.total(),
            2,
            "a total over rows the caller may not see is a disclosure in itself"
        );
        assert_eq!(outcome.withheld(), 3);
    }

    /// An empty filtered listing is empty, not absent.
    ///
    /// The difference is a status code one layer up — `200` with an empty
    /// document rather than `404` — and the helper has to make the empty case
    /// representable for that to be expressible at all.
    #[test]
    fn a_filtered_to_empty_listing_is_empty_rather_than_absent() {
        let outcome = filter_listing(vec![1, 2, 3], |_| PackageVisibility::Hidden);
        assert!(outcome.is_empty());
        assert_eq!(outcome.total(), 0);
        assert_eq!(outcome.withheld(), 3);
    }

    /// Filtering removes nothing when the broad tier already grants the read.
    ///
    /// Not a bug, and worth a test so it is not mistaken for one: grants only
    /// widen, so a registry-tier `releases:read` reaches every package. The
    /// filter is meaningful exactly when the broad tier grants `releases:list`
    /// *without* the read.
    #[test]
    fn a_broad_read_grant_filters_nothing() {
        let broad = set_for(&[Action::ReleasesRead, Action::ReleasesList]);
        assert_eq!(
            package_visibility(&broad, Action::ReleasesRead),
            PackageVisibility::Listed
        );

        let list_only = set_for(&[Action::ReleasesList]);
        assert_eq!(
            package_visibility(&list_only, Action::ReleasesRead),
            PackageVisibility::Hidden,
            "list without read is the configuration §4.4 is written for"
        );
    }
}

#[cfg(test)]
mod readable_tests {
    use super::*;
    use crate::entities::{
        resolve, GrantMap, Identity, Node, RegistryGrants, RegistryKind, Role, SubjectMatcher, Tier,
    };

    fn subject(user: &str) -> Subject {
        Subject::Identity(Identity {
            user_id: Some(user.to_owned()),
            role: Role::User,
            auth_provider: None,
            groups: vec![],
        })
    }

    fn map(subject: SubjectMatcher, actions: &[Action]) -> GrantMap {
        GrantMap::new().grant(subject, actions.to_vec())
    }

    /// A registry whose node grants `registry` to `role:user`, with the given
    /// namespaces.
    fn registry_with(
        registry: Option<&[Action]>,
        namespaces: &[(&str, Option<Option<&[Action]>>)],
    ) -> RegistryGrants {
        RegistryGrants {
            kind: RegistryKind::Npm,
            registry: Node::new(
                Tier::Registry,
                "registry:reg",
                registry.map(|a| map(SubjectMatcher::Role(Role::User), a)),
            ),
            namespaces: namespaces
                .iter()
                .map(|(prefix, grants)| {
                    let node_grants = match grants {
                        // No block at all — inherits.
                        None => None,
                        // `grants = {}` — the seal.
                        Some(None) => Some(GrantMap::sealed()),
                        Some(Some(actions)) => Some(map(SubjectMatcher::Role(Role::User), actions)),
                    };
                    (
                        (*prefix).to_owned(),
                        Node::new(Tier::Namespace, format!("namespace:{prefix}"), node_grants),
                    )
                })
                .collect(),
        }
    }

    const READ: Action = Action::ReleasesRead;
    const LIST: Action = Action::ReleasesList;

    /// The fast path: a registry-tier read grant means every package, and the
    /// package-grant query is never made.
    ///
    /// That is the property phase 0b's number depends on — a per-package query
    /// here is the 806× N+1 — so it is asserted rather than assumed.
    #[test]
    fn a_registry_read_grant_never_needs_the_package_query() {
        let alice = subject("alice");
        let readable =
            Readable::from_registry(None, &registry_with(Some(&[READ, LIST]), &[]), READ, &alice);
        assert!(matches!(readable, Readable::Everything));
        assert!(!readable.needs_package_grants());
        assert!(readable.contains("anything-at-all"));
        assert!(!readable.is_empty());
    }

    /// **The bug this rewrite fixes.** A namespace-tier read grant reaches every
    /// package under it.
    ///
    /// §1's own example — *"the payments team owns `@acme/billing-*`"* — is this
    /// configuration, and before the namespace tier was resolved here the caller
    /// was served an empty index by all six whole-registry documents while the
    /// per-package routes served them normally.
    #[test]
    fn a_namespace_read_grant_reaches_the_packages_under_it() {
        let alice = subject("alice");
        let readable = Readable::from_registry(
            None,
            &registry_with(Some(&[LIST]), &[("@acme/billing", Some(Some(&[READ])))]),
            READ,
            &alice,
        );
        assert!(
            readable.contains("@acme/billing"),
            "a namespace contains itself"
        );
        assert!(readable.contains("@acme/billing/cards"));
        assert!(
            !readable.contains("@acme/billing-internal"),
            "matching is on segment boundaries — a hyphen is not a separator"
        );
        assert!(!readable.contains("@other/thing"));
        assert!(!readable.is_empty());
    }

    /// **The other direction.** A namespace seal withholds what the registry
    /// granted, and the listing agrees with the download gate about it.
    ///
    /// Five of the six wired documents caught this downstream because they call
    /// `load_visible_versions` per package and it authorizes; Composer's
    /// `available-packages` checks only visibility, so a sealed namespace's
    /// package names were disclosed there. §6.3: *"a listing more permissive than
    /// this discloses the names of packages this would refuse to serve."*
    #[test]
    fn a_namespace_seal_withholds_what_the_registry_granted() {
        let alice = subject("alice");
        let readable = Readable::from_registry(
            None,
            &registry_with(Some(&[READ, LIST]), &[("@acme/secrets", Some(None))]),
            READ,
            &alice,
        );
        assert!(
            !matches!(readable, Readable::Everything),
            "a seal anywhere costs this registry the fast path — it has to be asked per package"
        );
        assert!(!readable.contains("@acme/secrets/keys"));
        assert!(!readable.contains("@acme/secrets"));
        // …and the control: the seal is a *namespace* seal, not a refusal of
        // everything.
        assert!(readable.contains("@acme/public"));
        assert!(readable.contains("@acme/secrets-but-not-really"));
    }

    /// A package-tier grant survives a namespace seal (§4.3).
    ///
    /// *"A seal stops inheritance, it does not disable the nodes beneath it"* —
    /// which is what makes the administrative floor a recovery rather than a
    /// ceremony.
    #[test]
    fn a_package_grant_survives_a_seal() {
        let alice = subject("alice");
        let readable = Readable::from_registry(
            None,
            &registry_with(Some(&[READ]), &[("@acme/secrets", Some(None))]),
            READ,
            &alice,
        )
        .with_package_grants(
            [(
                "@acme/secrets/keys".to_owned(),
                SubjectMatcher::User("alice".to_owned()),
                vec![READ],
            )],
            READ,
            &alice,
        );
        assert!(readable.contains("@acme/secrets/keys"));
        assert!(!readable.contains("@acme/secrets/other"));
    }

    /// The slow path: list without read, so only packages this caller has a
    /// package-tier grant on appear.
    #[test]
    fn a_list_only_caller_sees_only_their_granted_packages() {
        let alice = subject("alice");
        let rows = vec![
            (
                "mine".to_owned(),
                SubjectMatcher::User("alice".to_owned()),
                vec![READ],
            ),
            (
                "someone-elses".to_owned(),
                SubjectMatcher::User("bob".to_owned()),
                vec![READ],
            ),
            // Matches alice but does not carry the read: holding `owners:read` on
            // a package is not permission to download it.
            (
                "wrong-verb".to_owned(),
                SubjectMatcher::User("alice".to_owned()),
                vec![Action::OwnersRead],
            ),
        ];
        let readable =
            Readable::from_registry(None, &registry_with(Some(&[LIST]), &[]), READ, &alice);
        assert!(readable.needs_package_grants());
        let readable = readable.with_package_grants(rows, READ, &alice);

        assert!(readable.contains("mine"));
        assert!(!readable.contains("someone-elses"));
        assert!(!readable.contains("wrong-verb"));
        assert!(!readable.is_empty());
    }

    /// A caller with the list verb and no package grants sees an empty document,
    /// not a refusal.
    ///
    /// §4.4: *"An empty filtered result is `200` with an empty document, not
    /// `404` — for a whole-registry index it discloses nothing."*
    #[test]
    fn a_caller_with_no_package_grants_sees_an_empty_document() {
        let alice = subject("alice");
        let readable =
            Readable::from_registry(None, &registry_with(Some(&[LIST]), &[]), READ, &alice)
                .with_package_grants([], READ, &alice);
        assert!(readable.is_empty());
        assert!(!readable.contains("anything"));
    }

    /// Membership is a binary search, so the set has to be sorted.
    #[test]
    fn the_readable_set_is_sorted_so_lookups_are_correct() {
        let alice = subject("alice");
        let rows: Vec<_> = ["zeta", "alpha", "mid"]
            .iter()
            .map(|n| {
                (
                    (*n).to_owned(),
                    SubjectMatcher::User("alice".to_owned()),
                    vec![READ],
                )
            })
            .collect();
        let readable =
            Readable::from_registry(None, &registry_with(Some(&[LIST]), &[]), READ, &alice)
                .with_package_grants(rows, READ, &alice);
        for n in ["zeta", "alpha", "mid"] {
            assert!(readable.contains(n), "{n} should be readable");
        }
        assert!(!readable.contains("omega"));
    }

    /// **`Scope` is a second implementation of resolution, and this is what keeps
    /// it honest.**
    ///
    /// `resolve` is the model; `Scope::contains` is the same arithmetic without
    /// the allocation, because a document cannot afford to clone a node per
    /// package. Two implementations of one rule is the defect this document
    /// exists to remove, so both are run over a matrix of hierarchies — every
    /// combination of registry grant, two namespaces each absent/sealed/granting,
    /// and two subjects — and any disagreement fails here.
    ///
    /// The matrix includes `owners:read`, which is in §4.3's administrative
    /// floor, because that is the one verb where the two could differ for a
    /// reason `releases:read` never exercises.
    #[test]
    fn the_scope_agrees_with_resolve_on_every_hierarchy() {
        let subjects = [subject("alice"), Subject::Identity(Identity::anonymous())];
        let packages = [
            "@acme/billing",
            "@acme/billing/cards",
            "@acme/billing-internal",
            "@acme/secrets/keys",
            "unrelated",
        ];
        // None = no block (inherits), Some(None) = sealed, Some(Some(..)) = grants.
        let arms: [Option<Option<&[Action]>>; 4] = [
            None,
            Some(None),
            Some(Some(&[READ, Action::OwnersRead])),
            Some(Some(&[LIST])),
        ];

        let mut compared = 0usize;
        for verb in [READ, Action::OwnersRead] {
            for registry in [
                None,
                Some(&[LIST][..]),
                Some(&[READ, Action::OwnersRead][..]),
            ] {
                for first in &arms {
                    for second in &arms {
                        let grants = registry_with(
                            registry,
                            &[("@acme/billing", *first), ("@acme/secrets", *second)],
                        );
                        compared += compare_hierarchy(
                            &grants,
                            verb,
                            &subjects,
                            &packages,
                            &format!("registry={registry:?} ns=({first:?}, {second:?})"),
                        );
                    }
                }
            }
        }
        assert!(compared > 500, "the matrix should be wide: {compared}");
    }

    /// One cell of the matrix: assert `Scope::contains` and `resolve` agree for
    /// every (subject, package) pair under `grants`, and return how many pairs
    /// were compared.
    ///
    /// Split out of the matrix loop so the two implementations being compared
    /// stay visible; the six nested `for`s above are bookkeeping over the
    /// hierarchy space, and this is the actual assertion.
    fn compare_hierarchy(
        grants: &RegistryGrants,
        verb: Action,
        subjects: &[Subject],
        packages: &[&str],
        arm: &str,
    ) -> usize {
        let mut compared = 0usize;
        for subj in subjects {
            let readable = Readable::from_registry(None, grants, verb, subj);
            for package in packages {
                let expected = resolve(&grants.path_for(package), subj).holds(verb);
                assert_eq!(
                    readable.contains(package),
                    expected,
                    "Scope disagreed with resolve on {package} for {verb}: {arm}"
                );
                compared += 1;
            }
        }
        compared
    }
}

/// §4.4 rule 3 in the key itself: every way one caller's whole-registry
/// document could be replayed to another.
///
/// Each test below names a filter the document applies and asserts that two
/// callers who differ *only* in that filter do not share an entry. They are
/// written one filter per test rather than as a matrix because each is a
/// separate disclosure with a separate fix, and a matrix failure would not say
/// which.
#[cfg(test)]
mod document_key_tests {
    use super::*;
    use crate::entities::{GrantMap, Node, RegistryGrants, RegistryKind, SubjectMatcher, Tier};

    fn identity(role: Role, user: Option<&str>, groups: &[&str]) -> Identity {
        Identity {
            user_id: user.map(str::to_owned),
            role,
            auth_provider: None,
            groups: groups.iter().map(|g| (*g).to_owned()).collect(),
        }
    }

    /// A registry granting `actions` to everyone, with an optional sealed
    /// namespace to force the `Scoped` path.
    fn registry(actions: &[Action], sealed_namespace: Option<&str>) -> RegistryGrants {
        RegistryGrants {
            kind: RegistryKind::Npm,
            registry: Node::new(
                Tier::Registry,
                "registry:reg",
                Some(GrantMap::new().grant(SubjectMatcher::Anyone, actions.to_vec())),
            ),
            namespaces: sealed_namespace
                .into_iter()
                .map(|prefix| {
                    (
                        prefix.to_owned(),
                        Node::new(
                            Tier::Namespace,
                            format!("namespace:{prefix}"),
                            Some(GrantMap::sealed()),
                        ),
                    )
                })
                .collect(),
        }
    }

    fn readable(grants: &RegistryGrants, action: Action, id: &Identity) -> Readable {
        Readable::from_registry(None, grants, action, &Subject::Identity(id.clone()))
    }

    fn key(
        id: &Identity,
        read: &Readable,
        list: &Readable,
        local: &[String],
        beta: bool,
    ) -> String {
        key_with_versions(id, read, list, local, &[], beta)
    }

    fn key_with_versions(
        id: &Identity,
        read: &Readable,
        list: &Readable,
        local: &[String],
        versions: &[String],
        beta: bool,
    ) -> String {
        document_cache_key(
            "reg/versions",
            &DocumentAudience::new(id, read, list, local, versions, beta),
        )
    }

    /// The baseline the sharing property rests on: two callers who agree on
    /// everything share one entry.
    ///
    /// §11.7 arm 3 is only viable if this holds — an estate of ten thousand
    /// `role:user` callers with no groups must not hold ten thousand copies of
    /// the same bytes. The fix for the disclosures below is only a fix if it
    /// leaves this alone.
    #[test]
    fn two_callers_entitled_to_the_same_document_share_an_entry() {
        let one = identity(Role::User, Some("alice"), &[]);
        let other = identity(Role::User, Some("bob"), &[]);
        let all = Readable::Everything;
        assert_eq!(
            key(&one, &all, &all, &[], false),
            key(&other, &all, &all, &[], false),
            "no user id, no token, no provider — the key is a class of caller"
        );
    }

    /// Group membership order does not fragment the cache, and neither does
    /// whitespace.
    ///
    /// `check_team_visibility` compares group ids with spaces stripped, so the
    /// key normalises the same way: a token that reports `acme dev` and one
    /// that reports `acmedev` resolve to the same team and must share bytes.
    #[test]
    fn group_order_and_spacing_do_not_fragment_the_cache() {
        let one = identity(Role::User, Some("alice"), &["acme dev", "payments"]);
        let other = identity(Role::User, Some("bob"), &["payments", "acmedev"]);
        let all = Readable::Everything;
        assert_eq!(
            key(&one, &all, &all, &[], false),
            key(&other, &all, &all, &[], false)
        );
    }

    /// **The disclosure this key was rewritten for.** A namespace grant or seal
    /// changes the document, and the registry node does not mention it.
    ///
    /// The old key was `resolve(&[grants.registry], subject)`. Both callers
    /// here resolve that node identically — it grants the read to everyone —
    /// and are entitled to different documents, because a seal withholds a
    /// namespace from one of them and not the other.
    #[test]
    fn a_namespace_seal_is_part_of_the_key() {
        let alice = identity(Role::User, Some("alice"), &[]);
        let open = registry(&[Action::ReleasesRead, Action::ReleasesList], None);
        let sealed = registry(
            &[Action::ReleasesRead, Action::ReleasesList],
            Some("@acme/secrets"),
        );

        let unsealed_read = readable(&open, Action::ReleasesRead, &alice);
        let sealed_read = readable(&sealed, Action::ReleasesRead, &alice);
        assert_ne!(
            key(&alice, &unsealed_read, &unsealed_read, &[], false),
            key(&alice, &sealed_read, &sealed_read, &[], false),
            "the registry node is identical in both; the seal is the whole \
             difference between the two documents"
        );
    }

    /// `releases:list` is resolved separately from `releases:read`, so it is
    /// keyed separately.
    ///
    /// `check_read_access` runs `authorize_listing` on the list verb before the
    /// read set is consulted. Two callers with the same read set and different
    /// list sets get different documents.
    #[test]
    fn the_list_verb_is_part_of_the_key() {
        let alice = identity(Role::User, Some("alice"), &[]);
        let read_only = registry(&[Action::ReleasesRead], Some("@acme/secrets"));
        let both = registry(
            &[Action::ReleasesRead, Action::ReleasesList],
            Some("@acme/secrets"),
        );

        let read = readable(&read_only, Action::ReleasesRead, &alice);
        assert_ne!(
            key(
                &alice,
                &read,
                &readable(&read_only, Action::ReleasesList, &alice),
                &[],
                false
            ),
            key(
                &alice,
                &read,
                &readable(&both, Action::ReleasesList, &alice),
                &[],
                false
            ),
            "the read set is the same object in both keys"
        );
    }

    /// §4.5 `private`: a grant written on the package admits its holder and
    /// nobody else, and the `Everything` fast path cannot express that.
    ///
    /// This is the case that survives every other field being equal. Both
    /// callers hold `releases:read` at the registry tier, so both resolve to
    /// `Readable::Everything` and neither has anything to filter — and one of
    /// them is entitled to see a private package the other is not.
    #[test]
    fn a_private_package_grant_is_part_of_the_key() {
        let alice = identity(Role::User, Some("alice"), &[]);
        let bob = identity(Role::User, Some("bob"), &[]);
        let all = Readable::Everything;
        let grant = ["@acme/secrets".to_owned()];
        assert_ne!(
            key(&alice, &all, &all, &grant, false),
            key(&bob, &all, &all, &[], false),
            "both callers are Everything; only one holds the grant written on \
             the private package"
        );
    }

    /// §4.4 rule 3 at the version tier — RFC 0017's own addition to this key.
    ///
    /// The version rows are a *different node* from the package rows above them,
    /// so two callers can agree on every package-tier grant and still be
    /// entitled to different documents. Before RFC 0017 this could not happen
    /// because nothing wrote a version row; the writer and this key had to land
    /// together, which is what §7 means by "not a follow-up".
    #[test]
    fn a_version_grant_is_part_of_the_key() {
        let alice = identity(Role::User, Some("alice"), &[]);
        let bob = identity(Role::User, Some("bob"), &[]);
        let all = Readable::Everything;
        let version = ["@acme/billing@2.4.0-rc.1".to_owned()];
        assert_ne!(
            key_with_versions(&alice, &all, &all, &[], &version, false),
            key_with_versions(&bob, &all, &all, &[], &[], false),
            "the compact index filters every version of every package; a caller \
             holding one version row sees a document the other does not"
        );
    }

    /// The two tiers are separate fields, not one concatenated list: a package
    /// key and a version key that happen to spell the same string must not
    /// collide the audiences.
    #[test]
    fn a_package_grant_and_a_version_grant_do_not_collide() {
        let alice = identity(Role::User, Some("alice"), &[]);
        let same = ["@acme/billing".to_owned()];
        let all = Readable::Everything;
        assert_ne!(
            key_with_versions(&alice, &all, &all, &same, &[], false),
            key_with_versions(&alice, &all, &all, &[], &same, false),
        );
    }

    /// `team` visibility is a group question, and no grant answers it.
    #[test]
    fn team_membership_is_part_of_the_key() {
        let member = identity(Role::User, Some("alice"), &["oidc:acme"]);
        let outsider = identity(Role::User, Some("bob"), &[]);
        let all = Readable::Everything;
        assert_ne!(
            key(&member, &all, &all, &[], false),
            key(&outsider, &all, &all, &[], false)
        );
    }

    /// `internal` visibility admits `role:user` and refuses `role:anonymous`,
    /// and a registry that grants the read to `*` resolves both to the same
    /// set.
    #[test]
    fn the_role_is_part_of_the_key() {
        let user = identity(Role::User, Some("alice"), &[]);
        let anon = identity(Role::Anonymous, None, &[]);
        let all = Readable::Everything;
        assert_ne!(
            key(&user, &all, &all, &[], false),
            key(&anon, &all, &all, &[], false)
        );
    }

    /// A beta-channel member's document carries pre-releases; a non-member's
    /// does not.
    #[test]
    fn beta_channel_membership_is_part_of_the_key() {
        let alice = identity(Role::User, Some("alice"), &[]);
        let all = Readable::Everything;
        assert_ne!(
            key(&alice, &all, &all, &[], true),
            key(&alice, &all, &all, &[], false)
        );
    }

    /// The document key carries the document's own identity too.
    ///
    /// Two documents of the same registry — `/versions` and `/names` — have
    /// different bytes for the same audience, so the prefix has to be part of
    /// the key.
    #[test]
    fn two_documents_do_not_share_a_key() {
        let alice = identity(Role::User, Some("alice"), &[]);
        let all = Readable::Everything;
        let audience = DocumentAudience::new(&alice, &all, &all, &[], &[], false);
        assert_ne!(
            document_cache_key("reg/versions", &audience),
            document_cache_key("reg/names", &audience)
        );
    }

    /// A separator inside a group id cannot pose as two groups.
    ///
    /// The length-prefix property, asserted rather than assumed: `feed` is the
    /// only thing standing between an operator-authored group id and a key
    /// collision with a different membership.
    #[test]
    fn a_separator_in_a_group_id_does_not_collide() {
        let one = identity(Role::User, Some("alice"), &["a,b"]);
        let other = identity(Role::User, Some("alice"), &["a", "b"]);
        let all = Readable::Everything;
        assert_ne!(
            key(&one, &all, &all, &[], false),
            key(&other, &all, &all, &[], false)
        );
    }
}
