# SonarCloud triage — 2026-09-07

The analysis after the worker images and the RFC tooling landed: **26 issues on
new code**, none of them typed `VULNERABILITY`, so the quality gate's security
rating is unaffected. Companion to
[`sonar-triage-2026-09-06.md`](./sonar-triage-2026-09-06.md), which holds the
standing register of what is ignored and why.

All 26 were read. The disposition:

| | Count | Where it goes |
| --- | --- | --- |
| Fixed in code | 21 | this branch |
| Won't fix, resolved in `sonar-project.properties` | 5 | the section below |

---

## Fixed in code — 21

| Rule | Count | What changed |
| --- | --- | --- |
| `docker:S8549` | 2 | `Containerfile.worker`: both `cargo build` passes take `--locked`. The lockfile is copied into the image already; without the flag the build was free to resolve something else than the one CI resolved, which is the whole point of carrying it. |
| `docker:S6506` | 2 | same file: both release downloads take `--proto '=https' --proto-redir '=https'`. A forge release URL answers with a redirect to its object store, and neither hop may be talked out of TLS. The idiom is the one `.github/workflows/test.yaml` already uses for the same reason. |
| `docker:S6570` | 2 | the stub-crate loop's `$crate` is quoted on both uses. |
| `docker:S7018` | 2 | the builder and tools `apt-get install` lists sorted. |
| `docker:S7020` | 2 | the two download lines split on `\`; the release host is bound to a shell variable so the asset URL fits on one line. |
| `javascript:S8786` | 4 | `docs/build/rfc-meta.mjs` ×3 and `docs/build/rfc.mjs` ×1 — see below. |
| `javascript:S3776` | 1 | `rfc-meta.mjs`'s `readDeferrals` was 26; `claimAfter`, `firstSentence` and `trimEndOf` came out of it, and the four patterns it repeated are now named constants. |
| `javascript:S6557` | 1 | `rfc-meta.mjs`: `/^\|/.test(line)` → `line.startsWith("|")`. |
| `javascript:S4624` | 1 | `rfc.mjs`: the nested template in the deferral line hoisted to a `lead` binding. |
| `javascript:S7755` | 1 | `tests/heavy/console_fetch.mjs`: `out[out.length - 1]` → `out.at(-1)`. |
| `javascript:S6582` | 1 | `tests/heavy/vsx_view.mjs`: `!x.icon \|\| !x.icon.src` → `!x.icon?.src`. |
| `shelldre:S7679` | 1 | `tests/heavy/quarantine.sh`: `sign()` binds `$1` to `body`, as the six of 2026-09-06 do. |
| `rust:S3776` | 1 | `explore_upstream_search` was 19 — see below. |

### The four regexes

Every one is the same shape: a quantifier followed by something that can match
the same characters, so the engine retries the split from every position. None
of them reads user input — these run over this repository's own RFCs at build
time — so the finding is about runtime, not about a denial of service. They are
worth fixing anyway, because each was also saying less than it meant:

- `/^#{2,4}[ \t]+(.+?)[ \t]*$/` → `/^#{2,4}[ \t]+(.+)$/`. The lazy group and the
  trailing `[ \t]*` were competing for the same spaces, and the caller already
  calls `.trim()`.
- `/[:,.\s]+$/` and `/\s*\|\s*$/` → `trimEndOf(s, chars)`, a scan from the end,
  and an `endsWith("|")` test for the table cell's closing pipe.
- `` `…`.replace(/\s+$/, "") `` → `.trimEnd()`, which is the same thing named.

`node build/rfc.mjs deferred --json`, `… deferred` and `… status` produce
byte-identical output before and after, and `task rfc:index` regenerates the
index and the sidebar unchanged.

### `explore_upstream_search`

Extraction only, as the thirty-one of 2026-09-06 were: `search_clients`,
`search_upstreams`, `canonical_name`, `asked_coordinates`, `held_set` and
`fetch_offers`. The handler now reads as the six steps it always was, and the
paragraph-long comments moved with the code they explain — `canonical_name`'s
two-spelling argument, in particular, now sits on the function that implements
it rather than three screens above its second use.

---

## Not a finding — `docker:S8431`, tag *and* digest ×4

`FROM rust:1.98-slim-bookworm@sha256:…`, twice in `Containerfile.worker`, once
more for the runtime stage and once in `Containerfile.worker-guarddog`.

The pair is deliberate, and it is this repository's convention everywhere an
image or an action is pinned — `registry:3.1.1@sha256:…` in
`.github/workflows/image-scan.yaml`, and every `uses:` pinned to a commit with
its version in a comment beside it. The digest is the pin: it is what is
pulled, and `docker:S7023` asks for exactly that. The tag is what a person
reads — a bump from `bookworm-slim` to `trixie-slim`, or from rust 1.98 to
1.99, is legible in the diff instead of being forty hex characters changing.
Satisfying this rule would mean deleting the half of the line a reviewer can
check.

**Ignored in `sonar-project.properties`** — `imageTagAndDigest`, scoped to
`Containerfile*`. A tag with *no* digest is a real finding, it is a different
rule (`docker:S7023`), and it stays on.

---

## Not a finding — `docker:S6596`, `FROM ${WORKER_IMAGE}` ×1

`Containerfile.worker-guarddog` builds on the worker image, and CI passes that
image's **digest** as the build argument — stricter than any tag. The
`:latest` default is there so a developer can build the optional GuardDog
variant locally against the published worker image without building one first.
A pinned default would go stale on every release and would silently build the
GuardDog image on an old `batlehub` binary, which is the one failure mode the
argument exists to prevent.

**Ignored in `sonar-project.properties`** — `workerImageArg`, scoped to that
one file.
