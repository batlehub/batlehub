# Grading the unbuilt RFCs

For someone deciding what to build next.

Seven RFCs describe work that is not built: 0002, 0008, 0010, 0011, 0014,
0018 and 0019. On 2026-09-02 every one of them was re-read against the tree
at that date — every file, function, table, config key, route and test each
document names was looked up, not taken on trust — with particular attention
to 0018 and 0019, which were drafted without the repository open. This page
records what that found, what was changed in each document as a result, a
grade out of 20 with the argument for it, and the order in which the seven
should be taken.

The grades are for the documents **after** the 2026-09-02 revision. Where the
revision moved a grade materially, the pre-revision figure is given so the
size of the correction is visible.

## Method

Each RFC was checked on four axes, in this order of weight:

1. **Truth about the tree.** Does what it says about existing code exist,
   under that name, doing that? A design built on a route that is not routed,
   a rule that is not in the chain, or a config section that is not a section
   is a design whose first implementation step is a surprise.
2. **Coherence with the RFCs that landed after it.** 0015 (grants), 0016
   (retention), 0017 (grants editor) and the drafts 0018/0019 changed
   vocabulary and ownership. An unbuilt RFC that still uses the old words
   will be built twice.
3. **Design quality.** Is the mechanism right, is the default honest, is the
   size proportionate to the hole it closes?
4. **Readiness.** Is the status honest? "In review, 0 open questions" is a
   claim about the codebase, not only about the author's list.

Every false claim found was fixed in the document. Where a fix was a decision
rather than a correction, it was taken and recorded in a dated §13 (0002,
0008, 0010, 0011, 0014) or in place with a header note (0018, 0019), so the
document can be re-reviewed as one piece. The `Still open` lists were updated
to match, because `task rfc:status` counts them.

## The order

The criterion is the one the [index](/rfc/) already states: **closing a hole
that is open now beats adding a capability that is missing**, and within
that, a small first phase that is useful alone beats a large one. Three
holes are open in shipped code today and were confirmed on this pass:
the GitHub client fetches without the SSRF guard the other two forge clients
use; `CveGateRule` and `BlockListRule` both fail open on a repository error,
and "not scanned" is indistinguishable from "clean"; and `EvictionService`
deletes the last copy of an artifact upstream has removed on the same
schedule as one it still has.

| # | Take | Why now | Waits on |
| - | ---- | ------- | -------- |
| 1 | **0010 phases 1–4** — `nodedist` | The hole that looks closed: `nodejs.org/dist` is cached as one synthetic package and nothing can block a Node release. Small, accepted, no dependency. | nothing |
| 2 | **0019 phase 1** — ref resolution, SHA cache key, rate-limit budget, GitHub onto the SSRF guard, `mise.sh` | The SSRF gap on GitHub is a shipped defect this pass found. The SHA key is the prerequisite of 0018's forge coverage and of 0008's identity model. | nothing |
| 3 | **0018 phase 0a, then phase 1** — the rejection survey; then the verdict model with age + OSV, fail-closed | The sharpest hole and the largest build. Phase 1 alone replaces two fail-open gates with one persisted verdict and needs no scanner toolchain. 0a is regression cover for four ecosystems that have none, quarantine or not. | nothing (0a and 1 are independent) |
| 4 | **0014 phases 1–3 and 5** — detection and the eviction hold | The only data-loss item. Now runs as a scanner on 0018's worker, so it costs one scanner and a state machine rather than a second sweeper. | 0018 phase 1 (`ScanQueue`) |
| 5 | **0010 phases 5–9** — `sdkman` | The other half of the toolchain layer; heavier (rendered-table filter, server-side 302 chain) and less exposed than `nodedist`. | 1 |
| 6 | **0018 phases 2–3** — error bodies, listings, `why`, the worker image and sandbox | Phase 2 is what makes a hold explicable to the person who hits it; phase 3 is the scanner runtime. | 3, and 0a for phase 2 |
| 7 | **0002, recast** — pushed flags as `SocVerdict` findings, the exposure report over 0018's query | Its core idea is right and its one remaining question is measured by 0018 phase 0a. As recast it is three to four weeks, not eight. | 6 |
| 8 | **0019 phases 2–5** — `ForgeRefRule`, raw policy, GitLab parity, provenance | Adds refusals (`TAG_MOVED`, raw off by default) that need 0018's verdict to be explained. | 3, and §11 q2 of 0019 before phase 3 |
| 9 | **0008** — the air gap | Rebuilt on 0019's identity and 0018's verdicts; the bundle is also 0004-bis §13.2's container. Nothing about it is urgent until the estate that needs it exists. | 1, 2, 3 |
| 10 | **0011, trimmed** — contract file, three CLI verbs, the che-code patch | About 500 lines once the proxy and extension are split out; low value until an editor that cannot repoint its gallery is in front of us. | nothing, but nothing waits on it either |
| 11 | **Two shipped defects outside any RFC** — `terraform/providers/write.rs` declares `201` in its `utoipa::path` and returns `200`; `signing.trusted_keys` has no load-time check that an entry is 32 hex bytes | Found by this pass in shipped code and recorded at the foot of this document rather than in a design: each is a one-file fix with a regression test, and neither has an RFC to wait on. Taken as a step so they are not lost between the documents. | nothing |

**Where the order stands (2026-09-04).** Items 1–6 have landed on
`feat/idk`, each recorded in its RFC's §13: `nodedist` (0010 §13.1), forge refs
(0019 §13.1), the rejection survey and the verdict model (0018 §13.1–13.2),
detection and the eviction hold (0014 §13.1), `sdkman` with the rest of 0010's
phases (0010 §13.2), and 0018's phases 2–3 — the verdict on the wire, the
listings, `why`/`wait`, the `bwrap` runner, the extraction policy, the four
scanners, the worker image and chart (0018 §13.3; postmortem's mapping is
observed on a canary package, GuardDog's is read). Item 7 — 0002 as recast —
landed the same day (0002 §13.1): flags as `SocVerdict` findings on a
`[security]` registry and `FlagsRule` elsewhere, the signed push and revoke,
the exposure report as one keyset query over `access_events`, `flags:read`,
`"flags"` exemptible; `flags:write` was not added (the push is HMAC-
authenticated, and a verb no route requests is §11.5's dead end) and
`CveGateRule` was left alone (a registry without that rule would have
ignored every flag). Item 8 — RFC 0019 phases 2–5 — landed on 2026-09-04 too
(0019 §13.2): the forge-ref gate (`MUTABLE_REF`, `TAG_MOVED`,
`ASSET_REPLACED`) as a rule that merges its findings into the *request's*
verdict rather than the stored one, the `[raw]` policy off by default with a
script check on both the name and the first bytes, the three typed
`[api_reads]` families and the release-document URL rewriting, GitLab brought
to parity and onto the shared rate-limit budget, and provenance from all
three forges — with `PROVENANCE_UNVERIFIABLE` proven to come from GitLab's
release evidence alone. The five parity cells still marked *(to confirm)*
were probed live before the code that relies on them, so §11 q1 is closed;
§11 q2 was decided before phase 3 and is recorded in both 0019 and 0010.
Item 9 — RFC 0008, the air gap — landed on 2026-09-04 as well (0008 §14),
all six phases, with `tests/heavy/mise.sh` §4 passing as the standing proof: a
plan from a lock, seeded and exported as a signed bundle, imported by a
*second* instance running `[air_gap] enabled = true`, and `mise install`
completing from the lock against that instance with egress denied to both
processes. Building it
corrected three things the document said about the tree, each found by a test
rather than by a re-read — a storage key is a function of the **route** and
not of the URL, so the server now reports the key it served from and the
bundle names that (0008 §14.1); an imported artifact needs the metadata entry
that finds it, because `ProxyService` resolves metadata before it looks at
the cache, so a bundle of bytes alone imported cleanly and served `503`
forever (§14.2); and `mise.lock` writes a *quoted* platform key and records
two addresses per asset, so the plan read neither correctly (§14.4). It also
found a defect in shipped code and fixed it: RFC 0019 phase 3's release
rewrite deleted each asset's `url`, a field the GitHub schema requires, and
every `mise install` through a github registry had answered `missing field
\`url\`` since that landed (§14.5). What a bundle still does not carry is
proxied documents, stated in §14.7 and left to an `0008-bis`.
Item 10 — RFC 0011 as §13 cut it — landed on 2026-09-04 as well (0011 §14):
the credential contract file with the normative JSON Schema §4.1 asked for,
`auth token`/`write-token-file`/`status`, and the editor patch carried in
`patches/che-code/`. It closes the thing this RFC could deliver without an
editor build in front of us — the warning on the OpenVSX page, that a gallery
requiring a credential answers every query with an empty list. Three decisions
were taken while building it: the reserved sources (`env`, `exchange`,
`keychain`) are **named in the schema** and read as "no credential", so the
first one implemented is not a breaking change; `auth token`'s subcommand
became optional rather than the verb gaining a fourth name the RFC's own
snippets would then have got wrong; and the patch is carried as a module plus
integration steps rather than as a diff with invented context lines, because
this repository builds no editor and a patch that fails to apply wastes the
time of whoever tries.

Item 11 closed the order on 2026-09-04, and closed it by disproving itself.
Its second defect — `signing.trusted_keys` unvalidated at load — was fixed
with item 9. Its first — a `201` declared and a `200` returned — **was not in
the tree**, and had not been since 2026-06-28. A sweep of all 299 documented
handlers found no annotation disagreeing with its body in either direction.
What replaced the one-line fix is the gate that would have caught it, and will
catch the next one, in `crates/web/tests/openapi_contract.rs` beside the spec
lint that was already there. The footnote at the end of this page records both
the correction and the reason a gate is the right answer to a claim like this
one.

**What can wait, and why.** 0014 phases 4 and 6–9 (the block arm and the
console) wait because there is nothing to block until a disappearance is
confirmed. 0018 phases 4–5 (rescan, external scanners) wait because the
internal profile is the one every deployment can run. 0019 phases 4–5 wait
because GitLab and provenance each need a live-forge confirmation first.
0011's remainder — the loopback proxy, the bootstrap entry, the extension —
waits because each is for an editor build that cannot repoint its gallery URL,
and none of ours currently is.
0008's own remainder is one thing and it is named in its §14.7: a bundle
carries artifacts and the metadata entry that finds them, not proxied
documents, which an `0008-bis` should settle for the clients that resolve
through a listing rather than from a lock.

**Two constraints, not preferences.** 0019 phase 1 must precede 0018 phase 3
(the age gate on a forge needs the derived date) and 0018 phase 1 must
precede 0019 phase 2 (`MUTABLE_REF` has nowhere to live without a verdict);
both documents now state it. And 0010 precedes 0008 for the reason the index
gives: an air gap has to hold for every toolchain, and two of them have no
registry kind yet.

## The grades

### RFC 0018 — Supply-chain quarantine and verdicts · 13 / 20

Pre-revision: 9. The most important document in the set and the one written
furthest from the tree. Thirteen claims about existing code were false or
stale, and three were load-bearing: every configuration example used
`[registries.cve_gate]` / `[registries.release_age]` sections that do not
exist (gates are `[[registries.rules]] kind = …` entries); `VerdictGateRule`
was placed "right after `RbacRule`", a rule that left the chain in RFC 0015
phase 3; and `batlehub serve --roles` was the operator interface for a
binary that has no `serve` subcommand. Also wrong: "RFC 0015 removed
`RuleDecision`'s third variant" (no RFC did; it has always been binary), an
inbound webhook whose payload is an enum (it is a `serde_json::Value`) and
whose HMAC is "already enforced" (the secret is optional), "sqlx migrations"
(the workspace uses `mig!`), role defaults in `build_policy` (they live in
`translate.rs`), the audit table `access_log` (it is `access_events`),
metrics "through the existing OpenTelemetry setup" (traces only), "publish
endpoints answer 201 today" (they are mixed: cargo, rubygems and openvsx
answer 200, npm, nuget, the three `repo` kinds and both Terraform kinds
answer 201). **The claim that one of them declared `201` and returned `200`
was itself wrong**, and item 11 is where that was found — see below. Internally it contradicted itself on
the publish status (§6.8 vs §4.2), on rescan leadership (advisory lock vs
"the existing coordinator"), and its cross-references used question numbers
from an earlier draft.

All of it is fixed in place, with a correction note in §6.8. Three decisions
were added to §11 (28–30) to settle its seams with 0002, 0014 and 0019, which
the three other documents mirror.

**Pros.** Right diagnosis: fail-open in two gates, no notion of "unscanned",
no lever for a SOC. Right mechanism: one persisted verdict, `SCAN_PENDING` as
a deny, existing gates wrapped as scanners rather than a fifth gate beside
them. Honest about the default (the maturity window is empty by default, and
it says so). The rejection survey (§4.4) is the best-argued preliminary in
the set. The security section is concrete: `bwrap`, no package manager ever
invoked on scanned content, `--no-config` because postmortem would otherwise
load a config from the tarball.

**Cons.** Five phases, a second process role, a sandbox and six scanner
adapters — the index already flagged that its cost has been argued less
hard than its value. A worker role on a codebase that has no leader election,
no `SKIP LOCKED` and no advisory lock today. The extractor hardening is
described as "hardened" when the existing one only checks path containment
and truncates. It had not been run against the tree at all before this pass.

### RFC 0019 — Forge registries: refs, releases, raw · 12 / 20

Pre-revision: 7. Rewritten. The coordinate model was built on paths that are
not routed: the summary's before/after used `/archive/main.tar.gz` (the route
is `/tarball/{ref}`; `archive/` is the upstream URL the client *builds*), the
`Release` row carried a `/repos/` prefix no handler has, and the whole
`[registries.api]` section "limited which families the proxy forwards" from a
`/repos/*` passthrough that does not exist. The parity table claimed the
Forgejo `verification` object was "present in the existing client's models";
those models are two structs, releases and assets. §7 said "the existing
`registry::ssrf` guard applies" — it applies to Forgejo and GitLab; the
GitHub client follows redirects unguarded, which is the one genuine shipped
defect this RFC now closes in phase 1. §11 said "Still open: none, ready for
In review" above a table with three cells marked *(to confirm)* and eight
more that no client calls today. Smaller: a docs directory that does not
exist, a `server/tests/` that does not exist, a `published_at: null` test
that does not exist, a cache-key scheme owned by the adapter when
`ProxyService` derives the key from the `PackageId`, `batlehub why` as if it
existed, and a declared dependency cycle with 0018.

The revision keeps the design — ref resolution, SHA identity, mutable refs
warned, raw off by default, shared budget — and rebuilds it on the routes
that exist: the wildcard passthrough became three typed read-only routes,
the cache key became a rewritten `PackageId`, the metadata rides in
`extra.forge` until 0018's typed fields land, the GitHub SSRF fix and a
`mise.sh` heavy suite entered phase 1, and the parity table marks honestly
which cells are exercised. Three open questions were reopened, including the
tension with 0010 decision 9 about serving installers.

**Pros.** The ref/SHA model is the right answer to "a branch is not a
version" and it produces exactly the metadata 0018's age and provenance
gates need. Raw off-by-default and moved-tag-denied are the honest defaults.
The rate-limit budget is the one thing that keeps 0018's worker from taking
the proxy down.

**Cons.** Eleven parity cells are still from documentation memory. Two new
tables and a new key scheme for archives whose old entries simply age out.
Phase 2 degrades on a forge without `[security]` to deny-or-header, which is
correct but thin. It cannot go to "In review" until phase 1 confirms its own
table.

### RFC 0010 — The toolchain layer · 15 / 20

The best-shaped document in the set and the only one Accepted. The design
holds: enforce at the client's own resolution chokepoint (`index.tab`,
`candidates/validate`), follow the broker's `302` server-side through the
SSRF pair so the JDK's bytes actually pass through the proxy, relay hooks and
checksums byte-exact, ship each half behind its own heavy suite. What this
pass found was vocabulary and drift: §7 used pre-0015 words (`releases:list`
now; the `strip` composes with 0015's grant filter, which runs first), the
namespace separator would have read SDKMAN's `{candidate}/{platform}` as a
namespace, four line references had moved, "six exhaustive matches" is ten
plus three `matches!` that default silently, "twenty kinds" disagreed with
"twenty-one", "retention" meant eviction, and two protocol details disagreed
with the recorded live probe (the `X-Sdkman-*` headers arrive on the `302`,
not the final `200`; `selfupdate` and the `broker/version` location were
missing). Its seams with 0018 — mandatory `deny_missing_timestamp` versus
0018's `hold_missing_timestamp`, and a JDK exceeding 0018's extraction
ceiling — are now stated on both sides.

**Pros.** Right enforcement point per client; reuses the SSRF, blocking and
listing infrastructure faithfully; honest about what it cannot filter; the
`nodedist` half is cheap and closes a hole a working cache hides.

**Cons.** The fixed-width parse of SDKMAN's rendered `versions/list` table is
brittle and its cache key is per client. Platform smuggled into the package
name leaks into namespaces, storage keys and stats. Accepted two weeks ago,
nothing started.

### RFC 0014 — Upstream disappearance · 14 / 20

Pre-revision: 11. The diagnosis is the strongest single sentence in the
seven: `run_ttl` selects on `cached_at` and knows nothing about upstream, so
the cache discards the last copy in the estate precisely because nothing is
refreshing it. The population gate and the three redundant thresholds are
the right discriminator. What had moved: the migration number was taken, two
paths were stale, motivation 2 ("a 404 is discarded") missed the existing
per-process negative cache in `UpstreamDetailCoordinator`, the actor
convention duplicated `Identity::system()`, the rung ladder inferred
capability from an empty `Vec` when `RegistryKind::upstream_detail()` already
declares it, and — the real one — it would have been a second periodic
walker beside 0018's rescan worker, with a second way to say "upstream no
longer lists this". It now runs its probe as an 0018 scanner and keeps the
state machine; all six open questions are closed, including the one the
index said was owed before phase 2 (`min_probed = 10`, and the block arm
refused under the floor).

**Pros.** Correct diagnosis of real data loss; safe defaults done carefully
(hold before block, conditional unblock, audited system actor); phases 1–3
and 5 are small and shippable.

**Cons.** Depends on 0018 phase 1 now, which it did not before. Three deleters
touch the same bytes (its hold, 0018's retention, eviction) and the
precedence had to be invented here. Nothing started.

### RFC 0008 — mise in an air-gapped estate · 11 / 20

Pre-revision: 8. The motivation is measured and still true, and every code
path it names exists or is honestly absent. But four load-bearing
assumptions no longer hold, and the header said "ready to schedule". "The
decision sits at one branch in `ProxyService::handle`" — there are two fetch
sites and five other dial-outs `handle` never sees, so "never dials" has to
be enforced where clients are built. "`serve_stale` becomes the normal path
for artifacts" — it is metadata-only. Its identity model keys forge archives
on the lock's sha256, which 0019 has just shown are not byte-stable; its
verification table duplicates 0018's verdicts. All four are rebuilt in §13,
the four open questions are closed (two of them by 0010 and 0018 having
decided the same shape), and the toolchain coverage it left unstated — which
mise backends are plannable and which fall to `generic` — is now a table.
The catch-all rewrite rule that would have rewritten BatleHub's own URLs is
fixed.

**Pros.** Miss recorded after the rule chain so a block is not a miss; the
503-not-404 argument is grounded in real code; the plan is computed offline;
import goes through the same three funnels as everything else; the bundle is
the container 0004-bis §13.2 asked for.

**Cons.** It now depends on three unbuilt items (0010, 0019 phase 1, 0018
phase 1). The bundle format, its signing and the dedup interaction are still
the largest unspecified piece. Roughly the size of 0010 for an estate that
has not yet asked.

### RFC 0002 — Vulnerability flags and exposure · 10 / 20

Pre-revision: 8. Reopened from "In review" to Draft, and the reopening is the
grade. Its separate-table argument is verified and correct, the kind/effect
split with an operator-owned ceiling is the right vocabulary, and retroactive
exposure by construction is the feature an incident actually needs. But it
was signed off against a tree that RFC 0004, 0005 and 0015 had already moved:
its migrations `031`–`034` are taken, every docs path in §6.7 moved, "admin
guard" is not a thing since 0015 and it declares no `Action` verb for six
endpoints, §4.11 attaches to an admin page that does not exist, §7 forbids a
per-user exposure view that `/api/v1/me/advisories` already provides, and
phase 1 would have created a second version order beside the one
`version_order.rs` deliberately keeps single. Against 0018 it overlapped on
four fronts — a `HardBlockSet` inside a rule that does not run on security
registries, a second SOC push surface, a second override path beside
`GateExemption`, a second who-pulled report. All of it is decided in §13 and
mirrored in 0018 decision 28: pushed flags become `SocVerdict` findings, this
RFC's batch contract is the SOC surface, `GateExemption` is the override,
one query serves both reports, phase 8's identity merge is split out. One
question remains, and it is a measurement 0018 phase 0a makes.

**Pros.** The core model survives the recast intact and gets simpler (no
second rule engine). Coverage tri-state and window-on-download semantics are
exactly right. Thorough test plan.

**Cons.** Eleven phases for one feature, now fewer but still the widest
surface after 0018. The recast makes it a consumer of 0018, so it moves from
"start early" to seventh. Its "In review" status was not honest about the
codebase it described.

### RFC 0011 — Authenticated OpenVSX access · 9 / 20

Pre-revision: 6. The lowest grade for a document with the best empirical
section in the set: §4.4.4's verification of what VS Code actually does
(`Code.Engine` mandatory, unsigned vsix installs, filterType 7 vs 10) is the
kind of evidence every other RFC here should have. But it had not been
re-read since July, and a quarter of its server-side work shipped under other
RFCs in a different shape. Phases 1–2 describe as future the middleware
already on the VSX routes, the TokenReview cache and the server-side audience
peek that already exist; its PAT model (scopes, optional expiry, a
`vsx:read` scope that is not in 0015's vocabulary) is not the shipped one
(role, groups, mandatory 1–90 day expiry); its CLI login flow (PKCE with a
loopback, device code) is not the shipped one (server-brokered paste); three
of its validation rows contradict the code; its own diagram says JWKS where
its text says TokenReview; twelve references point at a superseded RFC; and
its `exchange` credential source duplicates `ActionsOidcAuthProvider`. Five
deliverables across three languages and two repositories sat behind one
Draft. §13 cuts it to the contract file, three CLI verbs and the che-code
patch, moves the loopback proxy and the extension to a follow-up, and closes
all seven questions — two of them by leaving with the pieces that were cut.

**Pros.** Correct threat analysis of loopback in a pod and of refresh-token
rotation; refuses a `command` credential source; reuses the shipped profile
store. What remains after the cut is about 500 lines and useful.

**Cons.** The stalest document here; a reviewer could not trust a single
"today" statement in it without checking. What was cut is most of what it
argued for. Its value waits on an editor build that cannot repoint its
gallery, which none of ours currently is.

## What changed in each file

| RFC | Status | Changes |
| --- | ------ | ------- |
| [0002](/rfc/0002-vulnerability-flags-and-exposure) | In review → Draft | Migrations `047`–`050`, docs paths, `audit:read`, UI host; §13 with seven decisions; one open question stated. |
| [0008](/rfc/0008-mise-in-an-air-gapped-estate) | Draft | `NotFoundWithheld`, `render_mise_toml` arity, the `trusted_keys` rule it cited; §13 replacing four assumptions and closing four questions. |
| [0010](/rfc/0010-toolchain-managers) | Accepted | Line references, match count, `releases:list`, twenty-one; §13 on 0015/0018/0019 seams and two protocol corrections. |
| [0011](/rfc/0011-openvsx-login) | Draft → In review | The PAT row; §13 marking what shipped, what was wrong, the cut, and seven questions closed; §14 on building the cut. |
| [0014](/rfc/0014-upstream-disappearance) | Draft | `run_ttl` line, `PackageStatus` line, migration `047`, UI path; §13 re-basing onto 0018's worker and closing six questions. |
| [0018](/rfc/0018-supply-chain-quarantine-and-verdicts) | Draft | Fifty-two in-place corrections; decisions 28–30; a correction note in §6.8. Two questions still open, both by design. |
| [0019](/rfc/0019-git-forge-registries-refs-releases-raw) | Draft | Rewritten on the routed paths; typed API reads replace the passthrough; GitHub SSRF fix and `mise.sh` in phase 1; three questions reopened. |

Two things this pass found in *shipped* code, outside any RFC, and left for a
fix rather than a document: `terraform/providers/write.rs` declares `201` in
its `utoipa::path` and returns `Ok()`; and `signing.trusted_keys` has no
load-time validation that an entry is 32 hex bytes, so a bad key is found at
the first verify.

**The second is fixed** (2026-09-04): RFC 0008 §4.5 needed the same check for
`air_gap.bundle_trusted_keys`, and one rule applied to both is better than
two, so `validate_air_gap` refuses either malformed key at load and names the
registry.

**The first was not there** (2026-09-04). `terraform_provider_upload` passes
`StatusCode::CREATED` to the shared responder and has since 2026-06-28, when
that responder took a status argument — three months before this pass claimed
otherwise. A sweep of all 299 documented handlers in `crates/web` found no
annotation disagreeing with its body in either direction: none sends a `2xx`
it does not declare, and none declares a `2xx` its body cannot construct.

That is an uncomfortable finding for a page whose first axis is *truth about
the tree*, and the useful response is not a correction but a gate. It is now
`every_declared_success_status_is_one_the_handler_can_actually_send` in
`crates/web/tests/openapi_contract.rs`: a source lint beside the spec lint
that already lives there, reading each `#[utoipa::path]`'s `2xx` statuses and
the literal responses of the handler under it. Planting the exact defect this
page described makes it fail and name the handler; removing it makes it pass.
A wrong number in an annotation breaks nothing until a client believes it,
which is precisely why it needs checking rather than reviewing — and why a
reviewer, this one included, can report it either present or absent and be
wrong both times.

Item 9 found a third, in code shipped on this branch a few hours earlier:
RFC 0019 phase 3's release-document rewrite removed each asset's `url`,
which the GitHub schema requires, so every `mise install` through a github
registry failed to decode the release list. Fixed by repointing the field at
`releases/assets/{id}` — a route this proxy serves — rather than deleting it
(0008 §14.5). It is recorded here rather than added to the order because it
is already fixed, and because it is the argument for the heavy suites in one
line: the rule was written, reviewed and unit-tested, and only a real client
caught it.
