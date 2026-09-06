# What is next

For someone deciding what to build next. Written 2026-09-04, when the
eleven-item build order of the grading pass closed; re-read on 2026-09-05,
when nine of this page's own ten items had landed, and again that evening,
when 0011's canary closed and opened the signing item below. The criterion is
unchanged from [the index](/rfc/): **closing a hole that is open now beats
adding a capability that is missing**, and within that, a small phase that
is useful alone beats a large one. One rule was added the hard way and has
since paid for itself four times over: **an item is done when a real client
has been through it**, not when its unit tests pass.

## Where the seven stand

Checked against each RFC's header, its §11 list and its §13 landing notes,
and against `task rfc:status`.

| RFC | Header | Built | Left |
| --- | --- | --- | --- |
| [0008](/rfc/0008-mise-in-an-air-gapped-estate) — the air gap | Implemented | All six phases; `tests/heavy/mise.sh` §4 end to end with egress denied. | Nothing in the RFC. What a bundle does not carry — the listing a client resolves through — is [0008-bis](/rfc/0008-bis-listings-across-the-gap), Draft. |
| [0010](/rfc/0010-toolchain-managers) — toolchains | Implemented | All nine phases, `nvm.sh` and `sdkman.sh` green. | Nothing. |
| [0019](/rfc/0019-git-forge-registries-refs-releases-raw) — forges | In review | All five phases, every §11 question decided, the Explorer's short-SHA column, and `mise.sh` through phases 3–5 (§13.3). | Sign-off. |
| [0002](/rfc/0002-vulnerability-flags-and-exposure) — flags | In review | Everything §13 recast; the one question decided (*not now*, the feed surfacing is a follow-up). | Sign-off. |
| [0011](/rfc/0011-openvsx-login) — OpenVSX | In review | The §13 cut (§14); since 2026-09-05 the loopback proxy and the sign-in bootstrap (§14.8), and the canary with a **real Extensions view** (§14.9): VS Code 1.136.1's server build, its workbench driven in Chrome over CDP by `tests/heavy/vsx_view.sh` — the entry in browse and search, its page rendered, the same page after the sign-in. | Nothing here: the `batlehub-vsx` extension was built on 2026-09-06 in its own repository (§11 q6), both modes measured in a real editor. And what the view found is not this RFC's to fix: a current VS Code installs nothing unsigned from its view, and since 1.136 nothing unsigned from its CLI either without `extensions.verifySignature` off — a BatleHub `vscode-marketplace` registry signed nothing — [RFC 0020](/rfc/0020-signing-at-the-vscode-marketplace-registry) now signs what it hosts and relays what it proxies. |
| [0014](/rfc/0014-upstream-disappearance) — disappearance | Implemented | All nine phases (§13.1–§13.4): the sweep, the notifications, the block arm, the admin API, the console, the operations page; `tests/heavy/upstream_audit.sh` against a served upstream and a real receiver. | Nothing. Both gaps it recorded closed 2026-09-05: the path-proxy family is probed per file (§13.5, `upstream_audit.sh` §7 against a served directory as a `generic` registry), and `on_confirmed` is a registry-tier policy row over the estate key (§13.6, the same suite under a registry-tier `"block"`). |
| [0018](/rfc/0018-supply-chain-quarantine-and-verdicts) — quarantine | Implemented | Every phase (§13.1–§13.7): the gate, the scanners and the sandbox, the spike, the rescan and the flip alert with pullers, the external scanners and the admin surface, the HPA input; `tests/heavy/quarantine.sh` from npm's side, seven steps. | §11 q2 stays open by design (publish status per tool, measured as each registry opts in). The sandbox row of the heavy suite runs only where user namespaces exist — CI, not this workstation. |

## What the order did

| # | Item | Outcome |
| - | ---- | ------- |
| 1 | Bookkeeping on 0010, 0019, 0002 | Done. Three headers rewritten, four questions struck with their reasons, `rfc:index:check` and `docs:audience` green. |
| 2 | `tests/heavy/quarantine.sh` | Done, and it found two things before anything else was built: a flip to `warn` did not reach the listings (a stored `denied` kept hiding the version from every fresh resolve, which therefore never reached the path that would have re-judged it — fixed in `Verdict::hides_from_listings_under`), and npm's Hide axis has two halves, only the pinned one reaching the gate (0018 §4.4, §13.4). |
| 3 | 0014 phase 4 | Done, with `recheck` brought forward from phase 7 because the sweep interval's five-minute floor makes a confirmation ten minutes at least. The heavy fixture is an npm directory the suite serves, not the `pathproxy` registry this page wanted: the path-proxy kinds could not be probed at all (0014 §13.2) — until §13.5, when the same directory became a `generic` registry beside it. |
| 4 | 0018 phase 0b | Done. Seven canaries, seven layouts recognised, none `SCANNER_UNSUPPORTED`; the `.gem` needed its inner tarball opened, Rust has no source rules, a jar carries no source (0018 §11 q1's table, §13.5). |
| 5 | 0018 phase 4 | Done. The scheduler, the advisory-lock leader, the anti-starvation slot, the flip alert with pullers, `pullers` as JSON and CSV, `ArtifactReleased`; step 7 of the heavy suite observes the *scheduler's* rescan deny a served version. It also found that a scanner declared under a config key other than its type was never "done" (`NamedScanner`, §13.6). |
| 6 | 0014 phase 6 | Done. The block arm through `AdminService`, the conditional unblock, the reconciliation pass, the warnings; the heavy suite runs under `"block"` and sees a fresh `npm install` stop at the packument and a pinned `npm ci` refused, then both served after the restore. |
| 7 | 0014 phases 7–9 | Done. The listing and the status endpoints, `AdminUpstream.vue`, the health card, the package badge, the operations page in the sidebar. |
| 8 | 0018 phase 5 | Done. `socket` and `mlab` (the latter as a `FindingEnricher`, keyed by the CVE OSV now records beside a GHSA id), the admin verdict listing, bulk rescan and backfill, `batlehub verdicts list\|rescan\|backfill`, `worker.autoscaling` on `batlehub_scan_jobs_queued`, `server/tests/roles.rs`. |
| 9 | 0019's tails | Done. The short-SHA chip; `mise.sh` refuses a script under `[raw]`, serves a README beside it, reads the `tags` family, and reads a release document whose every download link points home. |
| 10 | `0008-bis` | Written on request, 2026-09-05, as [Listings across the gap](/rfc/0008-bis-listings-across-the-gap): Draft, the §14.8 measurement as its §2, and a decision — a disconnected instance **synthesises** a listing from what it holds rather than carrying upstream documents in the bundle. Its phases 0 to 3 landed the same day (its §13.1–§13.4): the measurement; the synthesised listings for npm, PyPI, the forges, cargo, Go, Maven, NuGet, nodedist and SDKMAN; the miss log's `requested` and `held` columns. `tests/heavy/airgap.sh` runs both halves — the refusal, then npm, pip, mise, cargo, go, mvn and dotnet completing off listings the instance composed. Phase 4 landed too, and then the renderers that open the artifact at import — RubyGems' compact index, conda's `repodata.json`, NuGet's registration page, Composer's `p2` (its §13.6); nine clients in `airgap.sh`; then Terraform's provider download document (its §13.7), composed from the held archive, checksum list and signature with the publisher's keys carried on the manifest as `facts` — the estate signs nothing (its §11 q6) — and `terraform init` the tenth client. *In review.* Nothing of its §4.3 stays a `503`. |

## What is left

- **Sign-off on 0008-bis** — *In review* with two deferrals open in §11; every kind of its §4.3 is composed and proven by a client.
- **Sign-off on 0020** — [RFC 0020](/rfc/0020-signing-at-the-vscode-marketplace-registry),
  *In review* since 2026-09-06: phases 1, 2, 3 and 5 landed (its §13), the
  three questions decided by measurement, phase 4 deferred until a VSIX
  crosses the gap at all. `tests/heavy/vsx_view.sh` is the proof: the view
  enables Install on a registry-signed extension, the editor's verifier
  refuses it as §4.5 said, and with `extensions.verifySignature` off the
  view installs it and the extension activates; a marketplace extension
  republished with its signature attached (`PUT …/vsix/signature`, its
  §13.6) gets `Success` from the editor's own verifier.
- **0011's `batlehub-vsx`** — **built**, 2026-09-06, in its own repository
  (its §11 q6): both cuts of §12, phase 7's broker (the contract file kept
  fresh, the credential in the status bar, the re-query a sign-in needs) and
  phase 8's fallback marketplace (a view that lists what the registry shows
  you and installs through the editor's own command, dependencies and packs
  resolved, RFC 0020's signature verified, RFC 0018's verdict honoured).
  Proven the way everything else here is: a real VS Code 1.136.1 web build
  driven in a browser over CDP, both modes, against a real BatleHub. Left:
  a release of that repository, and deciding whether this instance hosts it.
  Building it also found and fixed a defect in this repository's CLI: the
  contract file is keyed by origin, and `contract::normalize_origin` only
  trimmed a trailing slash, so `proxy serve` looked its entry up under the
  whole registry URL and never found what a schema-conformant writer filed
  (0011 §14.10).
- **Sign-off on 0002 and 0019** — both are *In review* with nothing open.

## Constraints that still hold

- Heavy suites share one Postgres, and every server's embedded worker
  leases from the one `scan_jobs` table: **run them one at a time**. Two at
  once had the wrong server take a job and drop it for lack of a security
  profile, and the other suite's hold never cleared.
- Heavy ports: servers 8081–8090 and 8101–8109, taps 8091–8100 and
  8111–8118; `upstream_audit.sh` also binds 8128 and 8138, `quarantine.sh`
  8127 and 8137; `airgap.sh` 8110, 8119, 8120 and 8121 (a TLS tap for
  Terraform's host); `vsx_login.sh` 8122/8129 and `vsx_view.sh`
  8123/8130 plus 8131 for the editor's web server (each proxy binds an
  ephemeral loopback port). **8124 and 8132 are taken** by the
  `batlehub-vsx` repository's `tests/heavy/view.sh`, which starts a BatleHub
  of its own against this same Postgres — so it is one of the suites that
  must not run beside another. The next suite takes 8125/8133.
