---
name: rfc
description: >
  Write, revise or land a BatleHub RFC under docs/rfc/. Use when the user asks
  for an RFC or a design document, a "bis" of an existing RFC, to resolve an
  RFC's open questions, to move one to In review / Accepted / Implemented /
  Rejected, or to check an RFC against the docs gates. Not for bug fixes,
  registry pages, changelog entries or the roadmap on its own.
user-invocable: true
argument-hint: "[new|revise|land] [title or NNNN]"
allowed-tools:
  - Bash(task rfc:*)
  - Bash(task docs:*)
---

An RFC here is a change worth arguing about *before* it is written, and the
document is what the argument is had against. The rules of the form live in
two places this skill points at rather than repeats, because a third copy
would drift (RFC 0005 is the record of what that costs):

- `docs/internal/0000-rfc-template.md` — the sections, the header table, the
  status vocabulary, what to delete. Read it in full before `new`.
- `docs/contributing/contributing.md` §12 — the mechanics: `task rfc:new`,
  what is generated from the header rows, the drift gate.

This skill is the procedure around them: what to read first, what to run, in
what order, and the mistakes the gates do not catch.

## 1. Before writing a line

**Verify the protocol against its client, not against memory or the roadmap.**
The roadmap entry that sketches a feature is a hypothesis written without the
source open. RFC 0024 found two of its claims wrong in the first hour: the
client verified a checksum the entry did not mention, and it had removed a
signature check the entry said was "off by default". Either would have shipped
a broken adapter.

- Fetch the client's source and quote it: file, function, the line that sends
  the request or checks the answer. Name the client version you read.
- Probe the upstream on the wire (`curl -sI`, a `HEAD` on the real files) for
  sizes, content types and which paths exist. Numbers in the RFC come from
  here, not from estimates.
- Read the nearest sibling RFC for vocabulary before inventing any: a registry
  kind reads RFC 0010 (nodedist, sdkman); a policy reads 0015/0018; a listing
  question reads 0008-bis. Reuse its names for the same things.
- Grep for every type, function and file the RFC will name. An RFC that
  invents symbols the codebase does not have is hard to review and harder to
  implement. `RegistryKind`'s exhaustive matches, `DocumentKind`,
  `blocking::strip`, `listing_filter()` and `builders.rs` are the usual
  anchors for a registry kind.

## 2. `new` — scaffold, fill, regenerate

```bash
task rfc:new TITLE="…" SETTLES="…" [SHORT=…] [SLUG=…] [BIS=NNNN]
task rfc:index          # after every header-table edit
task docs:design        # the finish line: index drift, links, structure, audience, i18n, mermaid, roadmap
```

`rfc:new` takes the next free number (never reused), fills author and date,
sets `Draft`, and writes the page into both listings so it is never an orphan.
Then fill the template top to bottom and **delete every HTML comment**; what
is left must read as a document, not a filled-in form. Sections that do not
apply are deleted and the numbering closed up, never left as "N/A" — with the
one exception §4 names, the heavy-case list, which no document is allowed to
be without.

Set `Co-author` to the model that wrote with the user, in the form the
existing RFCs use. `Touches` lists the crates and trees the design actually
edits, and it is what a reviewer reads first.

## 3. What the gates do not check

The gates catch a broken link, a heading too deep, a diagram that does not
parse and a listing that drifted. They do not catch a weak document. The
house rules, from the RFCs that read well:

- **Motivations are numbered and falsifiable.** Each names the code path or
  workflow that hurts today. "It would be cleaner" is not one; "the client
  refuses the manifest as *checksum failed*" is.
- **Non-goals are the most useful section.** Write the things a reasonable
  reader would assume are in scope, one clause each on why not.
- **Before / after shows, prose tells, and it is drawn.** §1 carries both: a
  config block or client snippet *and* a mermaid diagram of the two paths, the
  one the request takes today and the one it takes after. §4 shows again, in
  its own terms. See §5.1 of this skill for what the diagram has to contain.
- **Behaviour rules state the uninteresting case.** What happens when nothing
  is blocked, when the upstream is the default, when the option is absent as
  distinct from empty.
- **Validation splits hard errors from warnings**, in two tables, and says
  where the operator will see a warning. A `tracing::warn!` nobody reads is
  not an answer.
- **The design names the invariant, not only the steps.** Under each
  mechanism: "because X, Y cannot happen here" is the sentence a reviewer
  checks.
- **A "deliberately untouched" list** in §6 for whatever a reviewer will go
  looking for and should not: the sibling implementation not reused, the rule
  that needs no new branch, the question another RFC already closed.
- **Alternatives are rejected on a concrete cost**, never "it is worse". The
  literal reading of the roadmap entry is usually one of them.
- **Decisions keep their numbers.** A question moves from *Still open* to
  *Resolved* with its number, its decision in bold and a one-line rationale.
  The gap in the resolved table is the record.
- **Phases each leave the tree green**, and the note says which phase is
  useful on its own. Two phases that cannot land apart (a kind with no client)
  say so and land together.
- **The heavy-case block is present in every document**, as §4 requires, and
  the document is not valid without it — including when all it says is that no
  heavy test applies, which is a written decision rather than a silence. The
  list in the RFC and the suite that gets built diverge later; that delta is
  the record §13 keeps, and it only exists if the list came first.
- **Word count.** A page over 4 000 words declares `reference: true` in
  frontmatter, or `docs:structure` fails. Most RFCs are over it; check with
  `wc -w`.
- **Prose width** is 80 columns like the rest of the tree; tables and code
  are exempt.

## 4. The heavy-case block, which no document may omit

Every RFC names the cases a **real client** will prove it by, and it names them
before any of it is built. The list is the last `§6.x` before the docs
subsection — `tests/heavy/<name>.sh` — and §10's test plan points at it by
number rather than repeating it, the way RFC 0031 §10 does: ``**Heavy**
(`tests/heavy/galaxy.sh`): §6.10.``

**The block is mandatory unconditionally — an RFC without it is not valid.**
It stays `Draft`: it does not move to `In review`, it is not signed off, and
`land` has nothing to write its §13 against. This is the second readiness test
beside *Still open* being empty, and it binds a *bis* exactly as it binds a
first document.

"This change needs no heavy test" is a **valid answer inside the block** and
never a reason to leave the block out. The subsection is written either way,
and so is the §10 bullet pointing at it; only the contents differ — a numbered
list of cases, or the paragraph §4.2 describes saying why there are none. This
is the one place §2's "sections that do not apply are deleted" does not reach,
and the reason is that the two are indistinguishable afterwards: an RFC with
no block does not tell a reviewer whether the author decided no client could
see the change or never asked the question. One of those is a decision, the
other an omission, and only the written block separates them.

Each case is **numbered, and an observation rather than an intention**: the
command a client actually runs, what crosses the tap while it runs, and the
fact that settles it — an exit code, the client's own error text quoted, a
request that was *not* made, a counter that moved. "Verify that blocking
works" is not a case. "An exact pin on a blocked version exits non-zero with
*Failed to resolve the requested dependencies map* and issues no artifact
request" is. A case a reviewer cannot check against a wire transcript
afterwards is not a case — and `heavy_client_said` prints rather than asserts,
so the assertion is on the transcript either way.

### 4.1 The floor for a kind, an upstream or a protocol surface

A route test proves routing. The suite that finds the shipped bugs is the one
that runs the real package manager, and an RFC that does not name it is an RFC
whose "it works" is a guess. For a new kind the `§6.x` names:

- the script, its `config.<name>.toml`, `task test:<name>-heavy`, and the row
  it adds to the `heavy-client` matrix;
- **the client, and how it is pinned** — the version, where the job installs
  it from, and which of its caches are redirected into the run's directory so
  no two steps share state;
- **what it proves on the wire, through the tap**, as the numbered list above.
  At minimum: an install that succeeds; a **refusal**, with the client's own
  error text and the assertion that the refused bytes were never requested; a
  listing that no longer names the blocked version; and a second install that
  moves `batlehub_artifact_cache_hits_total`.
- for a kind with a publish protocol, a publish followed by an install of what
  was published, from a clean client cache.

A kind owes four more proofs beside this one — the closed-world phase, the
credential boundary, the air gap, and the soak arm — and five gates fail the
build without them; `CLAUDE.md` § *Adding a new registry adapter* step 9 and
`docs/contributing/adding-a-registry.md` §11 are where they are enumerated.
The RFC lists them in the same `§6.x`, one line each, so the document a
reviewer signs off names every suite the branch will have to turn green.

Read `docs/rfc/0010-toolchain-managers.md` §13 for what the difference between
this list and the built thing looks like afterwards — that delta is the reason
the list is written before the code.

### 4.2 A change that adds no registry kind of its own

Most RFCs here are not a new kind, and the rule does not soften for them. The
`§6.x` then names the **existing** suite that is the change's regression
signal, the phase or arm it adds to it, and the same numbered observations.
The cross-cutting suites already own most surfaces:

| What the RFC touches | The suite that proves it |
| --- | --- |
| grants, tokens, a credential boundary | `tests/heavy/authz.sh` |
| a bundle, an instance with no upstream | `tests/heavy/airgap.sh` |
| a rule, a policy, a gate, a scan | `tests/heavy/quarantine.sh` |
| storage, a backend, the router | `tests/heavy/backends.sh` |
| memory, latency, a cache under load | `tests/heavy/soak.sh` |
| routing, a path prefix, a host | `tests/heavy/pathproxy.sh` |
| local, proxy and hybrid against each other | `tests/heavy/hybrid.sh` |
| every kind at once | `tests/heavy/closed_world.sh` |
| the console, the UI against a live server | `tests/heavy/console_fetch.sh` |

When the change genuinely cannot be seen from a client — a generated table, a
refactor with no user-facing surface, a docs reorganisation — the subsection
stays, titled `### 6.N Heavy coverage` since there is no script to name, and
its content becomes the argument for that. Three sentences, in this order:
what a client *would* have observed if the change had a client surface; why it
does not have one; and which non-heavy suite carries the regression signal
instead (`cargo test --workspace`, a `docs:*` gate, a conformance fixture).
The §10 bullet stays too, reading ``**Heavy**: none, §6.N`` rather than
disappearing.

That paragraph is a claim a reviewer can refuse, which is exactly why it is
written rather than left out. The one judgement to distrust is your own on a
change that *feels* invisible: `console_fetch.sh`, `airgap.sh` and `soak.sh`
all exist because a change that looked client-invisible was not.

## 5. A new registry kind carries two more things

Most RFCs here add a protocol. Two sections, beside the heavy list §4 already
demands of every document, separate the ones that were implementable from the
ones that had to be re-researched at implementation time, and neither is
optional for a kind, an upstream or a protocol surface that does not exist yet.

### 5.1 The before / after diagram in §1

The text block says what the operator writes; the diagram says what changes on
the wire. Both, always, in `### Before / after`.

```mermaid
flowchart LR
    subgraph T["today"]
        C1["client"] --> U1["upstream"]
    end
    subgraph W["with this RFC"]
        C2["client"] --> P["this instance"] --> U2["upstream"]
    end
```

That sketch is the floor, not the target. A good one names the documents, and
marks the one request that is refused or rewritten — the whole point of the
RFC, in the place a reviewer looks first. A reader who stops after §1 should be
able to draw the request path from memory.
### 5.2 How the real registry actually works, as §5.1

**A new registry kind, a new upstream protocol, or a new protocol surface on an
existing kind opens §5 with the protocol as the real thing serves it** —
before any design is laid on top of it. Title it for the subject ("The
protocol as `static.rust-lang.org` serves it"), and renumber the mechanism
subsections after it.

It carries, for the *real* upstream and the *real* client:

- **Every endpoint the client touches, in the order it touches them**, as a
  table: the path, what answers, the content type, and what the client does
  with the answer. One full install from a cold cache, nothing skipped — the
  discovery document, the listing, the per-version document, the artifact, and
  whatever signature or checksum sits beside them.
- **A `sequenceDiagram` of that install**, against the real upstream with no
  proxy in it. This is the diagram a reviewer checks the design against, and
  the one an implementer works from.
- **The numbers, from probes**: status codes, response sizes, page caps,
  redirect chains and where they land, cache headers. Each one observed, not
  estimated. A `302` to a signed URL, an index capped at 100 entries and a
  27 MB listing are all design constraints, and none of them is in the
  protocol's documentation.
- **What the client verifies, quoted from its source**: the checksum, the
  signature, the lockfile digest, and the error text it prints when the check
  fails. Name the client version read.
- **Auth, and what the client will not send**: which requests carry a
  credential, in what header, and what it drops across a redirect.
- **The spellings**: how the upstream names a package, a version and an
  artifact file, including the normalisations (case, separators, `v` prefixes)
  and the filename the client reconstructs.

When the protocol is one **this instance defines** rather than one it proxies —
a publish surface with no upstream, as RFC 0025 adds to `generic` — there is no
real registry to describe, and the requirement is met by a `### 4.2 The
protocol` subsection carrying the same endpoint table and the same client
snippets. Say in its first line that there is no upstream, and do not write a
§5.1 that repeats it; an instruction has one home here too.

Write it so a second implementer could build the adapter from §5.1 alone, and
so a reviewer can tell a protocol fact from a design decision by which
subsection it is in. When a fact could not be observed — a WAF refused the
probe, the endpoint needs a credential nobody has — say so in place rather
than filling the gap from memory.

## 6. Diagrams

Two to four mermaid diagrams in the RFC's §5, beside the two §5 of this skill
asks for. `flowchart` for a decision, a `sequenceDiagram` for a request
crossing components, `graph` for wiring. Each one carries a sentence beneath
it stating what it proves.

- Wrap labels in double quotes; use `<br/>` for line breaks.
- Inside a label, brackets and braces are HTML entities: `#91;` `#93;`
  `#123;` `#125;`. **The braces that make a decision node are structure, not
  label**: `B{"blocked?"}` stays as written, only the `{date}` inside a label
  becomes `#123;date#125;`. A blanket replace over the fence breaks every
  decision node.
- `task docs:mermaid` parses every fence in the repository; the site renders
  on the client, so a broken diagram builds green and draws an error box for
  the reader. Run it before calling the RFC done.

## 7. What else changes with an RFC

An RFC is never the only file. The table is the checklist.

| When | What | How |
| --- | --- | --- |
| Any header-table edit | `docs/rfc/index.md` (between the `rfc-index` markers) and `docs/.vitepress/nav/en.ts` (`rfc-sidebar`) | `task rfc:index`, never by hand |
| The RFC covers a roadmap entry | The entry in `ROADMAP.md` gains `(RFC NNNN, \`docs/rfc/NNNN-slug.md\`, Draft)`; the checkbox and the status word move as the RFC does | edit `ROADMAP.md`, then `task docs:roadmap` |
| The RFC changes what is next | `docs/rfc/plan.md` | by hand |
| The RFC corrects a claim elsewhere in the docs | The page that made it, in the same change | by hand; `task docs:links` for the cross-reference |
| The RFC adds a repo convention | `CLAUDE.md`, one paragraph, and `docs/contributing/` where a person reads it | by hand |
| The design adds a generated table (support, endpoints, listing coverage) | The generator, not the page | `task docs:*:check` names the task |

## 8. `revise` — open questions and status

`task rfc:status` prints every RFC's status and open-question count. Readiness
is two tests, not one: *Still open* is empty, **and** the heavy-case block of
§4 is there — with cases in it, or with the paragraph that says none apply.
`rfc:status` counts the first; the second is read by opening the document at
its `§6.x`, and an RFC that fails it stays `Draft` however settled its
questions are.

- Resolve a question by moving it up with its number and writing the
  decision; do not delete it. If the answer went against the draft's own
  recommendation, say so, that is the useful part.
- **A status move past `Draft` is refused while the block is missing.** Write
  it — the numbered cases, or the paragraph §4.2 asks for when none apply —
  then move the status. Adding it after sign-off inverts the order the rule
  exists to protect.
- Status moves in the header table only, and the listings follow through
  `task rfc:index`. The vocabulary is the template's: `Draft`, `In review`,
  `Accepted`, `Implemented`, `Rejected`, `Superseded by NNNN`. Anything
  else fails the build.
- The `/rfc/` page and sidebar are three shelves read off the status: *In the
  works* (Draft, In review), *Ready to build* (Accepted), *Settled*
  (Implemented, Rejected, Superseded). A document changes shelf by having its
  status row edited. **The files never move**; some two hundred links name
  them by path.
- A follow-on that reopens a settled RFC is a *bis* (`BIS=NNNN`), not an edit
  to the settled one beyond a pointer in its header.

## 9. `land` — when the implementation ships

- Add `## 13. Revision against the tree (date)` at the end, in the form RFC
  0010 §13 uses: what shipped, and every point where the built thing differs
  from the text above, each one small and each one deliberate, with the
  reason. Do not rewrite the earlier sections to match; the delta is the
  record.
- Say what the heavy suite observed, not what the RFC predicted: the
  client's own error text, what was and was not requested on the wire, the
  counter that moved.
- Status to `Implemented` with the landing dates in the note; `Touches`
  updated if the tree differs; the roadmap entry ticked and its status word
  removed; `task rfc:index`, `task docs:roadmap`, `task docs:design`.

## 10. Finish

Run `task docs:design` and report its last line. Then tell the user, in that
order: the number and path, where the heavy-case block is (the `§6.x`, by
number) and whether it carries cases or the paragraph saying none apply, what
is still open in §11, every other file the change touched, and what was
verified against a real client versus read from its source. If the block is
absent, say the document is not valid yet, and say it first. Do not commit;
the user signs.
