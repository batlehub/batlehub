---
reference: true
---

# Access Control

Two layers, and they answer different questions.

**[The authorization model](#authorization)** decides whether a caller may
perform an action on a resource at all: one vocabulary of verbs, granted to
subjects, on a four-tier hierarchy. Everything routes through it.

**Three narrower features** sit alongside it, each older than the model and each
still doing a job the model does not:
[pre-release gating](#beta-channel), [IP blocking](#ip-blocking), and
[team namespaces and per-package visibility](#team-namespaces).

---


Every request to BatleHub is answered by one model: a **subject** asks to perform
an **action** on a **resource**, and the answer is yes or no.

This page is the operator's reference for that model — the verbs, who you grant
them to, where you write them, and how to find out why something was refused.

For the design reasoning behind any of it, see
[RFC 0015](/rfc/0015-grants-on-the-resource-hierarchy).

## The authorization model {#authorization}

### The shape of a decision {#shape}

A caller needs **two** things, and they run in opposite directions:

| | Says | Composes | Direction |
| --- | --- | --- | --- |
| **grants** | *this subject may* | union over the path | only widens |
| **visibility** | *the audience is this wide* | deepest wins | only narrows |

Both must pass. A `releases:read` grant does not make a `team` package public,
and a `public` namespace does not serve a caller no grant matches.

There is deliberately **no deny rule**. A grant can never be revoked by a deeper
node, only unmatched — which means a mistake in a grant block fails *closed*,
because a union of nothing grants nothing.

### The verbs {#verbs}

The set is closed. A verb not on this list is a startup error, not a permission
granted to nobody.

| Verb | What it authorises |
| --- | --- |
| `releases:read` | download an artifact |
| `releases:list` | read a version listing or index document |
| `releases:publish` | publish a new version |
| `releases:overwrite` | replace an existing version's bytes |
| `releases:yank` | yank and unyank |
| `releases:delete` | delete a version |
| `source:read` | download a source archive |
| `catalogue:browse` | use the console's package explorer |
| `owners:read` | see a package's owners |
| `owners:write` | change them |
| `packages:block` | block a package or version administratively |
| `gates:exempt` | exempt a version from a gate ([below](#exemptions)) |
| `stats:read` | read the dashboard's aggregates |
| `audit:read` | read the audit log |
| `audit:purge` | delete audit-log entries older than a cutoff |
| `quarantine:read` | see that a version is held or denied by the supply-chain layer, its reason codes and when it becomes available ([RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts)) |
| `findings:read` | see the findings behind those codes — CVE ids, scanner output, SOC text |

Fourteen more authorise the **control surfaces** — the server itself rather than
what is published on it. They were one `require_admin` check until they were
split, so an administrator holds all of them and each is now delegable on its
own:

| Verb | What it authorises |
| --- | --- |
| `config:read` | read the running configuration |
| `config:write` | reload it, or change a registry |
| `system:read` | health, metrics, the notification wiring |
| `system:write` | change that wiring |
| `blocks:read` | read the block lists |
| `blocks:write` | change them |
| `authz:read` | the authorization diagnostics (`explain`, shadow) |
| `grants:read` | read the grants written on a package or a version |
| `grants:write` | write and remove them ([Grants on one package](#package-grants)) |
| `cache:evict` | drop cached artifacts |
| `cache:warm` | pre-fetch them |
| `quota:read` | read quota usage |
| `quota:write` | reset a user's quota counters |
| `retention:run` | run retention, and pin a version against it |
| `tombstones:read` | read tombstones, and compact their detail |
| `packages:read` | the administrative package list |

Four are **ecosystem-scoped** and only grantable on the registry types that
define them:

| Verb | Registry type | What it authorises |
| --- | --- | --- |
| `openvsx:namespace:claim` | `openvsx` | claim a publisher namespace |
| `terraform:signing-keys:write` | `terraform` | register the GPG key a namespace's providers are signed with |
| `jetbrains:channel:assign` | `jetbrains-marketplace` | move a published build between release channels |
| `npm:dist-tags:write` | `npm` | *reserved* — dist-tags are derived here, so nothing requests it ([RFC 0015](/rfc/0015-grants-on-the-resource-hierarchy) §4.2 has the argument) |

::: tip The list above is the whole vocabulary
All 35 verbs, checked against the enum by a test rather than maintained by hand
— an earlier version of this table listed three verbs that did not exist, and
copying one into a config file failed the server at startup.
:::

`releases:*` expands to every `releases:` verb; `*` expands to everything the
registry's ecosystem defines. **Expansion happens at config load**, so what a
subject holds is a fact about the loaded model rather than something recomputed
per request — and `task config:explain` prints it.

::: warning `releases:*` does not reach `gates:exempt`
Silencing a security finding is not a release operation. `gates:exempt` is
granted deliberately or not at all.
:::

### Who you grant to {#subjects}

Five subject forms:

| Form | Matches |
| --- | --- |
| `*` | everyone, including anonymous callers |
| `role:anonymous`, `role:user`, `role:admin` | callers at that role or above |
| `group:<provider>:<name>` | members of that group from that auth provider |
| `group:*:<name>` | that group name from **any** provider |
| `group::<name>` | that group name with no provider prefix |
| `user:<id>` | one principal |

Repeating a subject is a **union**, not a second opinion: two blocks granting
`role:user` different verbs give `role:user` both.

**A personal access token matches a `group:` subject only for the groups it was
created with.** A token carries a snapshot of its creator's groups, capped to a
subset of them and chosen at creation — never more than its owner holds, and
never re-resolved afterwards, because a token has no session to re-resolve from.
Two things follow for a grant you write:

- A token created with no groups matches no `group:` subject at all, whatever
  its owner belongs to. That is the default, and it is what every token minted
  before the snapshot existed carries. If a pipeline cannot see something its
  owner can, this is the first thing to check: `batlehub auth token list` shows
  what each token carries.
- A token keeps its groups when its owner loses them. Revoking the token is what
  ends that, not the change at the IDP — so offboarding revokes tokens, and a
  token's expiry (mandatory, 90 days at most) is the outer bound.

`user:<id>` matches a token the same as a session: a token resolves to its
creator's id.

### Where you write them {#tiers}

Five tiers, outermost first:

```
instance                                 (the server itself)
  └── registry            npm1
        └── namespace     @acme/billing  (matched, not enumerated)
              └── package @acme/billing/cards
                    └── version 1.4.2
```

The first three live in the config file. The last two cannot — a registry with
200 000 packages will not enumerate them in TOML — and are written through the
admin API instead.

The **instance** tier exists because about a dozen endpoints name no registry:
the configuration, health and metrics, the notification wiring, the block lists,
the authorization diagnostics. There is no registry for those to resolve
against, so they resolve here. It is also where the administrative floor sits.

```toml
# Instance tier: applies above every registry. This is the only place a grant
# can reach an endpoint that names no registry.
[grants]
"group:oidc1:sre" = ["system:read", "cache:evict"]

[[registries]]
type = "npm"
name = "npm1"
mode = "local"

# Registry tier: the default for everything beneath.
[registries.grants]
"*"                = ["releases:read", "releases:list"]
"group:*:engineer" = ["releases:publish"]

[[registries.namespaces]]
match      = "@acme/billing"
visibility = "team"

[registries.namespaces.grants]
"group:oidc1:platform" = ["releases:*", "owners:write"]
```

A namespace is **matched on segment boundaries** using the ecosystem's own
separator, so `@acme/billing` never matches `@acme/billing-internal`. The
separator is `/` for npm scopes and Go modules, `.` for OpenVSX publishers and
NuGet ids, `:` for Maven groupIds, the channel for conda, the namespace segment
for Terraform, the component for deb.

The separator is recorded **on the claim**, not derived per lookup: a namespace
outlives the registry's `type`, and deriving it would silently re-point every
existing claim the day somebody changed one.

::: warning Namespaces claimed before this shipped match on `/`
The column defaults to `/`, which is what every claim already matched — so
nothing changes meaning on upgrade. But a namespace claimed on a **dotted or
colon** ecosystem (OpenVSX, NuGet, Maven) before the upgrade keeps matching only
its own exact name: `digital` covers `digital` and not `digital.exts`. Re-claim
it to pick up the right separator. New claims get it from the registry's type.
:::

#### Sealing {#sealing}

`grants = {}` on a namespace **seals** it: nothing is inherited from above, and
only what is written on that node or below applies.

An absent block inherits. An empty block seals. They are different states, and
the difference is the whole reason the key is optional rather than defaulted.

Sealing is the one construct in the model that takes access away, so it is
confined to the config file — there is no way to seal a package through the API.
An **administrative floor** survives every seal, so a sealed subtree is never one
an administrator cannot reopen. It sits on the `instance` tier and gives
`role:admin` the fourteen control verbs, both `audit:` verbs, `stats:read`,
`packages:block`, both `owners:` verbs, `releases:yank`, `releases:delete` and
the three ecosystem verbs that no legacy setting translates to.

`gates:exempt` is **not** on the floor, deliberately: it is the one verb that
silences a security finding, so it is held only where someone wrote it down.

#### Grants on one package, or one version {#package-grants}

The two deepest tiers are written through the admin API rather than the config
file, for the reason the tier list gives: a registry with 200 000 packages will
not enumerate them in TOML.

```sh
# every verb the vocabulary defines, on one package, for one group
batlehub admin grants set npm1 @acme/billing --subject group:oidc1:eng \
    --actions releases:read,releases:list

# …or on one version. `name@version` is the coordinate, split on the last `@`,
# so a scoped npm name is still a package name
batlehub admin grants set npm1 @acme/billing@2.4.0-rc.1 \
    --subject group:oidc1:release-managers --actions releases:read

batlehub admin grants list npm1 @acme/billing   # both tiers
batlehub admin grants rm   npm1 @acme/billing --subject group:oidc1:eng
```

The console has the same controls on a package's detail page, under **Who can
reach this package**. Version-tier rows are shown there but edited from the CLI.

Four things are worth knowing before you write one:

- **Reading and writing them are their own verbs**, `grants:read` and
  `grants:write`, held by `role:admin` at the instance tier and delegable per
  registry like every other control verb. A grant listing enumerates who can
  reach a private package, which is why it is not folded into `audit:read`.
- **`releases:*` is expanded when you write it**, not when a request is
  evaluated. What you get back from `set` is the set that was stored — check it,
  because it is not always what you typed.
- **Ownership rows are not editable here.** Adding an owner writes a package-tier
  row carrying `releases:publish`, `owners:read` and `owners:write`; a write that
  would drop one of those is refused with a `409` naming `admin owner rm`. Two
  writers on one table is fine, two writers on one verb is a race.
- **A version-tier grant only ever adds.** Grants union across the path, so
  granting a group read on `2.4.0-rc.1` does not hide that version from anyone
  else. What it changes is the version index served to a caller who holds
  `releases:list` and **not** `releases:read`: they see the versions they were
  granted, and nothing else. If every version is filtered out, the document is
  `404` rather than an empty index — hidden means absent.

### The other policies {#policies}

Grants are one of six things a tier carries. The rest:

| Policy | What it says | Composes |
| --- | --- | --- |
| `grants` | who may do what | **union** over the path |
| `visibility` / `prerelease_visibility` | how wide the audience is | deepest wins |
| `versioning` | what a version may be called, and whether it may change | deepest wins, **wholesale** |
| `quota` | how much may be published | deepest wins, wholesale |
| `rules` | which gates judge the artifact | deepest wins, **per gate** |
| `retention` | what is kept ([RFC 0016](/rfc/0016-retention-and-the-permanence-of-a-published-name)) | deepest wins, wholesale |

::: warning `versioning` and `quota` compose wholesale
A deeper block **replaces** its parent's entirely. A namespace that omits
`enforce_semver` drops it rather than inheriting it.

That is what makes "this one package follows a different release convention"
expressible, and it is a sharp edge: every reload warns about a constraint a
deeper tier dropped.
:::

`rules` is the exception and composes **per gate**, so a namespace can re-tune
`release_age` without redeclaring `cve_gate`. A wholesale override there would
make a forgotten gate a silently disabled one.

```toml
[[registries.namespaces]]
match = "@acme/ci"

# First-party CI builds need no quarantine. The registry's other gates keep
# running — only `release_age_gate` is replaced.
[[registries.namespaces.rules]]
kind = "release_age_gate"
min_age_secs = 0
```

#### Visibility {#visibility}

Four values, widest to narrowest:

| Value | Audience |
| --- | --- |
| `public` | anyone, including anonymous |
| `internal` | any authenticated caller |
| `team` | members of the owning group |
| `private` | **only grants written on this node or below** — inherited grants do not apply |

`private` is a package- and version-tier value. Higher up it either says nothing
or duplicates a seal, and config load rejects it there.

`prerelease_visibility` is the same setting for pre-releases only, and is what
`[registries.beta_channel]` becomes. When it is not declared it **follows**
`visibility` — setting a package to `team` does not leave its pre-releases
public.

#### Immutability and ordering {#versioning}

```toml
[registries.namespaces.versioning]
enforce_semver = true
immutable      = "released"   # never | released | always
monotonic      = true
```

`immutable` decides whether published bytes may be replaced. `released` is the
Maven shape: a SNAPSHOT churns, a release does not.

::: tip Immutability is a property of the resource, not of the caller
A replace needs both a mutable resource **and** `releases:overwrite`. That split
is what lets a namespace be append-only for *everyone, including
administrators* — there is no bypass role, deliberately.
:::

`monotonic` refuses a publish whose version does not sort strictly above the
newest existing one, which catches republishing an *older* number after a bad
release. A yanked or deleted version still counts as the newest, so deleting
`2.0.0` does not free `1.9.9` to be re-taken.

Bulk import is incompatible with `monotonic` by construction, since a history
publishes oldest-first. Import with it off and turn it on afterwards.

### Gate exemptions {#exemptions}

"This CVE does not apply to how we use this library" is a real judgement, and
without a way to record it the only option is turning the gate off for the whole
registry.

```http
PUT /api/v1/admin/registries/{registry}/policy/version/{package}/{version}/rules/cve_gate
```

```json
{
  "exempt_until": "2026-12-01T00:00:00Z",
  "reason": "GHSA-… — the affected code path is not reachable from our usage"
}
```

**Only `cve_gate` and `license_gate` are exemptible**, and the line is not
arbitrary: an exemptible gate reports a finding a human can *assess*, while every
other gate establishes an *invariant*. A quarantine a version can skip is not a
quarantine, and an unsigned artifact is an absence of evidence rather than a
finding to accept.

Writing one requires **`gates:exempt`**, which nothing grants by default. Both
`exempt_until` and `reason` are required, so an exemption expires on its own —
the realistic failure is not a wrong assessment, it is a right assessment nobody
revisited.

Where the principal granting an exemption also published the version, it is
accepted and **flagged** `self_approved` rather than refused. Four-eyes enforced
by the tool is friction a small team routes around, most often by granting the
verb more widely.

### Shadow mode {#shadow}

The migration setting, and the most dangerous one on this page.

```toml
[registries.grants_shadow]
until = "2026-12-01"
```

A node in shadow-mode resolves its grants, records what it **would** have
refused, and refuses nothing. That is what makes adopting the model survivable:
enable it, watch a week of real traffic, then enforce.

::: danger Shadow-mode on grants fails open
A request that would be refused is **served**. Forgotten, this is an
authorization bypass configured on purpose.

`until` is required — a shadow with no expiry cannot be written — and config load
refuses to start with a date already past. An expired shadow **enforces**.
:::

Every reload warns, naming each node and its expiry, and the warning appears on
the Config Reload page rather than only in a log. What a shadow has served is on
the [authorization page](#watching) and in `batlehub authz shadow`.

`versioning` takes a `dry_run = true` too. Its direction is milder — a
badly-named or duplicate version is accepted, so bad data lands but nothing leaks
— which is why it needs no expiry.

### Watching it {#watching}

**`/admin/security/authorization`** gathers the five things that are otherwise
scattered:

| Panel | Answers |
| --- | --- |
| **Shadow** | what is being served that grants would refuse, per node, with each expiry |
| **Exemptions** | live gate exemptions, their expiry and reason, filterable to the self-approved ones |
| **Explain** | resolve any subject against any coordinate, with provenance |
| **Recent denials** | what has actually been refused |
| **Retention** | where to review what a live run would reclaim |

Three of those five are the fail-open or destructive directions of features
decided elsewhere. They are on one page on purpose: individually each is easy to
forget, and collectively they are the list of everything currently trusting you
to remember.

#### From a terminal {#cli}

```bash
batlehub authz explain npm1 --subject role:user --action releases:read \
  --package @acme/billing/cards

batlehub authz shadow --detail
```

`explain` answers with **which tier granted each verb**, which is the difference
between knowing what a subject holds and knowing which line to edit. It also
reports what it did *not* consider — per-package visibility, the artifact gates
and the block layers all sit behind grants — because a bare verdict is ambiguous
between "nothing denies this" and "nothing I looked at denies this".

::: tip A denial under a shadow says so
`explain` reports `deny` *and* names the node serving the request anyway. Without
that, the diagnostic would contradict the server on exactly the configuration
where being wrong matters most.
:::

For the config-file half — what a block expands to before any request — use
`task config:explain`.

### Upgrading from `[registries.rbac]` {#migrating}

`[registries.rbac]` is still read and always will be. There is no flag day.

It translates to registry-tier grants: `anonymous`, `user` and `admin` become
`*`, `role:user` and `role:admin`; `groups` entries become `group:*:<name>`
subjects. Your existing config keeps its exact meaning — the translation is
checked against the previous evaluator over every fixture, subject shape and verb
rather than trusted to review.

Two things worth knowing when you migrate:

- A `"*"` in `[registries.rbac]` means *today's two read verbs*, not the new
  wildcard. It expands to `releases:read`, `releases:list`, `source:read` and
  `catalogue:browse` — never to publish or delete.
- `[registries.beta_channel]` becomes `prerelease_visibility = "team"`, and its
  member group becomes a registry-tier grant.

Start with [shadow-mode](#shadow) if you are rewriting grants by hand.

---

## The three narrower features {#features}

Each of these predates the model above and each still does a job it does not:

- **[Beta/Pre-Release Channel](#beta-channel)** — restrict pre-release versions
  to approved users or groups. Superseded in expression by
  `prerelease_visibility` ([above](#visibility)), which is what a
  `[registries.beta_channel]` block now translates to; the block and its member
  list keep working and are still the way to manage membership.
- **[IP-Based Blocking](#ip-blocking)** — block abusive addresses fail2ban-style.
  Orthogonal to the model: it judges *where a request came from*, which is not a
  subject, an action or a resource.
- **[Team Namespaces & Package Visibility](#team-namespaces)** — assign name
  prefixes to auth-provider groups and set per-package visibility. The claim is
  what `visibility = "team"` resolves *against*, so the two are halves of one
  mechanism rather than alternatives.

---

## Beta/Pre-Release Channel {#beta-channel}

### How it works {#beta-how-it-works}

BatleHub determines whether a version is a pre-release from the version string
itself. The rule is [semver](https://semver.org/), after the same normalisations
the server's version *ordering* applies — so a two-component core is padded and a
leading `v` is dropped before the parse:

| Version | Pre-release? | |
|---------|-------------|---|
| `1.0.0` | No | |
| `1.0.0-beta.1` | **Yes** | |
| `1.0.0-rc.2` | **Yes** | |
| `1.0.0-alpha` | **Yes** | |
| `1.0-SNAPSHOT` | **Yes** | Maven's spelling; padded to `1.0.0-SNAPSHOT` before the parse |
| `1.0.0rc1` | **Yes** | PEP 440 attaches its marker with no separator |
| `dev-main`, `1.x-dev` | **Yes** | Composer dev-branch aliases |
| `2.0.0+build-1` | No | build metadata is not a pre-release |

::: warning This changed in RFC 0015 phase 4
There used to be two definitions of "pre-release" in the codebase and they
disagreed — one called `1.0-SNAPSHOT` a release, the other called
`2.0.0+build-1` a pre-release. They are now one, which is the definition above.

Two consequences on upgrade: a SNAPSHOT-shaped version becomes gated by the beta
channel where it previously was not, and the console's version table labels those
rows correctly. Nothing becomes *more* visible.
:::

There is **no separate flag or publish step** — the version string itself determines gating. Publish `mylib@1.0.0-beta.1` the same way as any other version; BatleHub infers it is a pre-release from the `-beta.1` suffix.

When `beta_channel.enabled = true` for a registry:

- **Non-members** — pre-release versions are hidden from version listings, and artifact downloads return 404.
- **Members** — pre-release versions are visible and downloadable alongside stable versions.

Stable versions are always visible to everyone regardless of membership.

### Configuration {#beta-config}

Add a `[registries.beta_channel]` block to any registry in `local` or `hybrid` mode:

```toml
[[registries]]
type = "npm"
name = "my-npm"
mode = "local"

[registries.beta_channel]
enabled = true
```

`enabled` is the only option. Members are managed at runtime via the admin API.

Omitting the block (or setting `enabled = false`) makes all versions visible to everyone.

### Managing members {#beta-members}

All endpoints require an `Admin` role token.

#### List members

```sh
curl -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/my-npm/beta-channel
```

```json
[
  { "principal_type": "user",  "principal_id": "alice",   "granted_by": "admin" },
  { "principal_type": "group", "principal_id": "qa-team", "granted_by": null }
]
```

#### Add a member

`principal_type` is `"user"` or `"group"`. A `"group"` entry grants access to every user carrying that group claim (from OIDC or Kubernetes auth).

```sh
# Add a specific user
curl -s -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"principal_type":"user","principal_id":"alice","granted_by":"admin"}' \
  https://batlehub.example.com/api/v1/admin/registries/my-npm/beta-channel

# Add an entire group
curl -s -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"principal_type":"group","principal_id":"qa-team"}' \
  https://batlehub.example.com/api/v1/admin/registries/my-npm/beta-channel
```

Returns `204 No Content` on success, `409 Conflict` if the principal is already a member.

#### Remove a member

```sh
curl -s -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/my-npm/beta-channel/user/alice

curl -s -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/my-npm/beta-channel/group/qa-team
```

### What users see {#beta-user-experience}

#### As a non-member

```sh
# npm — only stable versions are listed
npm view my-package versions --registry https://batlehub.example.com/proxy/my-npm
# [ '1.0.0', '1.1.0' ]

# Attempting to install a pre-release → 404
npm install my-package@1.0.0-beta.1 --registry https://batlehub.example.com/proxy/my-npm
# npm error 404 Not Found
```

#### As a member

```sh
# All versions listed, including pre-releases
npm view my-package versions --registry https://batlehub.example.com/proxy/my-npm
# [ '1.0.0', '1.0.0-beta.1', '1.0.0-rc.2', '1.1.0' ]

npm install my-package@1.0.0-beta.1 --registry https://batlehub.example.com/proxy/my-npm
# added 1 package
```

### Registry support {#beta-registries}

Gating applies in **local and hybrid mode** only — proxy-only registries proxy upstream as-is.

| Registry | Listing gated | Download gated |
|----------|:------------:|:--------------:|
| npm | ✓ | ✓ |
| Cargo | ✓ | ✓ |
| Go modules | ✓ | ✓ |
| RubyGems | ✓ | ✓ |
| Maven | ✓ | ✓ |
| Terraform modules | ✓ | ✓ |
| Terraform providers | ✓ | ✓ |
| PyPI | ✓ | ✓ |
| Conda | ✓ | ✓ |

::: warning Maven and non-semver versions
Maven versions that are not valid semver (e.g. `1.0-SNAPSHOT`) are never treated as pre-releases and are always visible. SNAPSHOT gating would require a separate feature.
:::

::: tip PyPI and Conda pre-release detection
For **PyPI**, PEP 440 pre-release versions (`.aN`, `.bN`, `.rcN` suffixes) are detected via their version string — no semver required.
For **Conda**, pre-release detection uses the same version-string heuristic (any version containing `alpha`, `beta`, `rc`, `dev`, or a semver pre-release component).
:::

---

## IP-Based Blocking {#ip-blocking}

### How it works {#ip-how-it-works}

BatleHub counts violation events per IP address within a sliding time window. When the count exceeds the configured threshold, the IP is automatically blocked for the configured duration.

A **violation** is any response whose status code appears in `trigger_on_status` (default: 429 and 401). This means:

- Repeated rate-limit hits → violations accumulate → auto-block.
- Auth brute-force attempts → violations accumulate → auto-block.

Blocked IPs receive `403 Forbidden` with an `X-Block-Expires` header containing the Unix timestamp when the block lifts. The check runs **before authentication**, so blocked IPs consume no auth resources.

The store is fail-open: if the backing store is unavailable, requests are allowed through rather than hard-blocked.

### Configuration {#ip-config}

Add an `[ip_blocking]` section at the **root** of `config.toml` (not inside a `[[registries]]` block):

```toml
[ip_blocking]
enabled               = true
violation_threshold   = 10       # violations before auto-block
violation_window_secs = 300      # counting window (5 minutes)
ban_duration_secs     = 3600     # block duration (1 hour)
trigger_on_status     = [429, 401]
```

| Field | Default | Description |
|-------|---------|-------------|
| `enabled` | `false` | Activate IP blocking |
| `violation_threshold` | `10` | Violations in the window before auto-block |
| `violation_window_secs` | `300` | Window duration in seconds |
| `ban_duration_secs` | `3600` | How long an auto-block lasts |
| `trigger_on_status` | `[429, 401]` | HTTP status codes that count as violations |

Only `enabled = true` is required; all other fields have sensible defaults.

::: tip Behind a load balancer
If BatleHub sits behind a proxy, real client IPs arrive via `X-Forwarded-For`. BatleHub uses the **first** IP from that header. Ensure your load balancer sets this header correctly and strips any client-supplied values to prevent spoofing.
:::

### Manual block management {#ip-admin}

All endpoints require an `Admin` role token.

#### List blocked IPs

```sh
curl -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/ip-blocks
```

```json
[
  {
    "ip":         "1.2.3.4",
    "blocked_at": 1748304000,
    "unblock_at": 1748307600,
    "reason":     "auto"
  }
]
```

#### Block an IP manually

```sh
curl -s -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"ip":"1.2.3.4","reason":"known bad actor","duration_secs":86400}' \
  https://batlehub.example.com/api/v1/admin/ip-blocks
```

| Field | Required | Description |
|-------|:--------:|-------------|
| `ip` | Yes | IP address to block |
| `reason` | No | Stored for audit purposes |
| `duration_secs` | No | Defaults to `3600` |

#### Unblock an IP

```sh
curl -s -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/ip-blocks/1.2.3.4
```

Auto-blocking will resume if the IP continues to trigger violations after being unblocked.

### Storage backends {#ip-storage}

Violation counters and block records share the backend selected by `config.cache.cache_type`:

| `cache_type` | Storage | Survives restart | Shared across instances |
|-------------|---------|:---------------:|:----------------------:|
| `memory` (default) | In-process | No | No |
| `postgres` | `ip_violation_counters` + `ip_blocks` tables | Yes | Yes |
| `redis` | Keys with TTL | Yes (if Redis persists) | Yes |

Use `postgres` or `redis` in production so blocks survive restarts and are enforced consistently across multiple BatleHub replicas.

---

## Combining both features {#combining}

The two features are independent and work well together. A common private-registry setup:

```toml
[[registries]]
type = "npm"
name = "my-npm"
mode = "local"

[registries.rate_limit]
requests_per_window = 100
window_secs         = 60
enforcement         = "block"

[registries.beta_channel]
enabled = true

[ip_blocking]
enabled               = true
violation_threshold   = 10
violation_window_secs = 300
ban_duration_secs     = 3600
trigger_on_status     = [429, 401]
```

Flow:
1. Rate limiting blocks excessive requests → 429 counts as a violation.
2. Auth failures (401) also count → brute-force attempts auto-block the source IP.
3. Beta releases are visible only to users or groups added via the admin API.

---

## Team Namespaces & Package Visibility {#team-namespaces}

### How it works {#ns-how-it-works}

A **team namespace** maps a package name prefix to an auth-provider group. Once claimed, only members of that group — plus admins — can publish packages whose name starts with `prefix` or `prefix/`.

**Example:** claiming prefix `frontend` for group `oidc:frontend-team` restricts publishing of `frontend/utils`, `frontend/components`, and any package named exactly `frontend` to members of that group. Publishing `backend/api` is unaffected.

Groups are not managed inside BatleHub. Membership is read from the `groups` claim delivered by the configured auth provider (OIDC, Kubernetes, or static token) on every request — no separate sync required.

**Package visibility** controls who can _download_ a package, independently of who published it:

| Visibility | Who can download |
|------------|-----------------|
| `public` (default) | Everyone, including unauthenticated users |
| `internal` | Any authenticated user |
| `team` | Members of the group that owns the namespace |

Visibility is **package-level** — all versions of a package share the same setting. When a new version is published, it inherits the existing visibility automatically. Admins always bypass visibility checks.

There is no TOML configuration required. Namespace claims and visibility are managed entirely at runtime via the admin API.

### Managing namespace claims {#ns-claims}

All endpoints require an `Admin` role token.

#### List claims

```sh
curl -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces
```

```json
[
  { "registry": "internal-npm", "prefix": "frontend", "group_id": "oidc:frontend-team", "claimed_by": "admin" },
  { "registry": "internal-npm", "prefix": "backend",  "group_id": "oidc:backend-team",  "claimed_by": null }
]
```

#### Claim a namespace

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"prefix":"frontend","group_id":"oidc:frontend-team","claimed_by":"admin"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces
```

| Field | Required | Description |
|-------|----------|-------------|
| `prefix` | Yes | Package name prefix (no trailing slash). May contain slashes: `org/team`. |
| `group_id` | Yes | Group name as it appears in the auth provider claim, e.g. `oidc:frontend-team`. |
| `claimed_by` | No | Free-text note; typically the admin who created the claim. |

Returns `204 No Content`; `409 Conflict` if the prefix is already claimed.

#### Release a claim

Prefixes containing slashes are passed verbatim in the URL path:

```sh
# Simple prefix
curl -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces/frontend

# Slash-containing prefix
curl -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces/org/team
```

Returns `204 No Content` even if the claim did not exist.

### Package visibility {#ns-visibility}

#### Get current visibility

```sh
curl -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/frontend%2Futils/visibility
```

```json
{ "visibility": "public" }
```

:::tip URL encoding
Package names that contain slashes must be percent-encoded in the URL: `/` → `%2F`.
:::

#### Set visibility

```sh
# Team-only
curl -X PUT \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"visibility":"team"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/frontend%2Futils/visibility

# Any authenticated user
curl -X PUT \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"visibility":"internal"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/frontend%2Futils/visibility

# Restore public access
curl -X PUT \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"visibility":"public"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/frontend%2Futils/visibility
```

Accepted values: `public`, `internal`, `team`. Returns `204 No Content`; `404` if the package has never been published; `400` for an unknown value.

#### Download-time enforcement

When a request arrives for a package with non-public visibility, BatleHub evaluates in order:

1. **Admin?** → allow.
2. **`public`?** → allow.
3. **`internal`?** → allow if the caller has at least `User` role (i.e. is authenticated).
4. **`team`?** → allow if the caller's group claims include the group that owns the namespace. If no claim is found, deny all non-admin access.

The same check applies to every access path: artifact downloads, index/metadata responses, version listings. A user who cannot download a package also cannot see it in `npm view`, `cargo search`, etc.

### Registry support {#ns-registries}

Team namespaces and visibility apply to all registry types in `local` or `hybrid` mode:

| Registry | Prefix example |
|----------|---------------|
| npm | `@scope` or `team/` |
| Cargo | `my-prefix/` or an exact crate name |
| Go modules | `github.com/org/` |
| RubyGems | `my-gem` |
| Maven | `com.example.group:` |
| Terraform modules | `namespace/module/provider` |
| Terraform providers | `namespace/type` |
| Composer | `vendor/` |
| OpenVSX / VSIX | `publisher.name` |
| PyPI | `my-org-` (package name prefix) |
| Conda | `my-org-` (package name prefix) |

Prefixes are matched by a **longest-prefix rule**: if both `frontend` and `frontend/ui` are claimed, `frontend/ui/button` is governed by the `frontend/ui` claim.

### User-facing namespace dashboard {#ns-user-dashboard}

Once claims are in place, users can manage their own packages without needing admin access. The **Team Namespace** page (`/my-namespace` in the web UI) lets group members:

- See all namespace prefixes their groups own, across every registry.
- Browse published package versions and change visibility inline.
- Upload new packages via a browser form (supported for RubyGems, Composer, OpenVSX, Go modules, PyPI, and Conda) or copy CLI instructions for other registry types.

::: tip Group name normalisation
Spaces in group names are stripped before matching — `"oidc:my team"` and `"oidc:myteam"` are treated as the same group. Set `group_id` without spaces when creating claims to avoid ambiguity.
:::

See the [Team Namespace dashboard section in the User Guide](/use/#team-namespace) for end-user instructions.
