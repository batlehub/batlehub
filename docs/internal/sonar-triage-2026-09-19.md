# SonarCloud triage — 2026-09-19

The sweep after the new-registry work landed on `main` (`4f9788f5`).
[`sonar-triage-2026-09-15.md`](./sonar-triage-2026-09-15.md) is the previous
round and the one this continues: it ends mid-argument about the path findings,
and this round is the answer it said to wait for.
[`sonar-triage-2026-09-06.md`](./sonar-triage-2026-09-06.md) holds the standing
register of what is ignored and why.

**50 findings. 44 fixed in code, 6 left for the UI** — five path-traversal
reports whose guard the analyser does not model, and one permission hotspot that
is a tar field rather than a filesystem request. Nothing is pinned: the register
stays as [2026-09-15 left it](./sonar-triage-2026-09-15.md#not-pinned), and for
the reason stated there — `multicriteria` scopes to a file, so pinning a rule
silences the sink somebody adds next year without the guard.

| Rule / kind | n | Disposition |
| --- | --- | --- |
| `pythonsecurity:S8707` / `S2083` — path traversal | 7 | **2 fixed in code** (the breaking-point pair now take a label, not a path); **5 resolved per issue in the UI** — see below |
| `githubactions` — `pip install` without `--only-binary` | 1 | fixed in code |
| `rust` — "make sure this permission is safe" | 1 | **UI, reviewed safe** at the reported site — a tar header field, not a permission — but the sweep it prompted found four *real* permission sites and narrowed them; see below |
| shell — "not enforcing HTTPS" | 6 | fixed in code, `fetch_https` wrapper |
| `python:S3776` / `rust` — cognitive complexity | 5 | fixed in code |
| shell — no explicit return at end of function | 18 | fixed in code |
| shell — positional parameter not assigned to a local | 2 | fixed in code |
| duplicated literal (3 shell, 1 Python) | 4 | fixed in code |
| `javascript` — `charCodeAt`, `for`-over-`for-of`, unnamed function | 4 | fixed in code |
| `python` — `:=` in an argument list | 1 | fixed in code |
| accessibility — click handler on a `<tr>` | 1 | fixed in code |

Statuses read from the public API rather than the UI, which needs no token:

```
api/issues/search?componentKeys=batleforc_batlehub&additionalFields=_all
```

---

## The path findings — the arc ends

2026-09-15 closed with the question open:

> Whether that is the shape *this* analyser models is the next analysis's
> answer, not a claim to make here. If it still reports them, these two join the
> eight below: resolved per issue in the UI, with the rule left armed on the
> file.

**It still reports them.** `perf_report.py` is flagged at both sinks again
(`load_runs`' `read_text`, `main`'s `write_text`, the latter under S2083 as
well), and the `basename`-and-compare rewrite moved nothing. So the condition
that sentence set is met, and those three are resolved per issue.

Three findings in the batch are **of a different kind**, and they are worth
separating out because they are not the same claim at all. Read the execution
flow rather than the title: what the rule says it found is a tainted *path*,
and what the flow actually walks is a tainted *string of report prose*.

| File | Function | Sink | What the flow ends in | Any path argument? |
| --- | --- | --- | --- | --- |
| `lifecycle_report.py` | `main` | `REPORT_FILE.write_text(...)` | the report text | **none** — `--iterations`, `--in-flight`, `--upstream-delay-ms`, all `type=int` |
| `soak_verdict.py` | `main` | `REPORT_FILE.write_text(...)` | the report text | **none** — `--k6-exit` (int), `--duration`, `--rate` (strings, printed) |
| `perf_report.py` | `main` | `args.out.write_text(markdown, …)` | `markdown` | `--out`, but it is not on this flow |

The first two write to a module constant derived from `Path(__file__)`. Neither
takes an argument that could name a file — `soak_verdict.py`'s three were
removed by 2026-09-15's own fix, and `lifecycle_report.py` never had any. What
both do is interpolate a CLI *string* into the report's prose. The rule is
reading tainted **content** reaching a file sink and reporting it as a tainted
**path**, which is a different defect from the `resolve()`-is-not-a-sanitiser
limitation: there is no guard to model here, because there is nothing to guard.
Resolved in the UI, rule left armed.

`perf_report.py`'s S2083 belongs with them rather than with its own file's other
two, and the twelve steps of its flow are what say so: source at `report`,
through `report.get('version')` into `preamble`'s f-string, into `out`, through
`render`'s list concatenation and `"\n".join`, out as `markdown`, and into
`args.out.write_text(markdown)` as the *content* argument. The receiver —
`args.out`, the only thing on that call that is a path — is never on the flow at
all: `results_file` built it as `RESULTS_DIR / basename`, and the analyser
evidently accepts that much. So the S8707 pair on this file are the
guard-not-modelled claim and stay as 2026-09-15 left them; the S2083 report on
`main` is the content-as-path claim and there is nothing in it to fix. The taint
it walks is a value this file *prints to stdout on the next line* — a report the
script exists to write.

### Fixed in code: the breaking-point pair

`breaking_point_row.py` and `breaking_point_report.py` were the last two perf
scripts still taking paths — they were written after the 09-15 sweep, which is
why they missed it. They now follow the construction that round settled on, and
the new `perf/scripts/perf_paths.py` is its one home:

* the only things crossing the process boundary are the run's `--label` and the
  step's `--rate`, neither of which is path-shaped;
* `perf_paths.label` refuses anything that is not a single conservative path
  component, and `confined()` then *resolves* what it built and refuses a path
  whose parent is not `perf/results/` — a statement about the result, so it
  still holds if someone widens the pattern;
* `breaking_point.sh` validates the label to the same rule, so a bad one is one
  message at the top rather than a stack trace forty minutes into an escalation.

The scratch directory moved with them. `breaking_point.sh` used a `mktemp -d`
and passed its paths in, which is exactly the argument shape being removed, so
k6's per-step summaries, the accumulating rows and the build log now live in
`perf/results/breaking-point-<label>-work/` — derived from the label rather than
handed over, removed by the existing `cleanup` trap, and gitignored for the run
that is killed before it can. The escalation's `PROXY_CACHE__STORAGE__PATH` went
with it, so a crashed run no longer leaves its evidence in a temp directory
nobody looks in — the same thing that fell out of the soak's move on 09-15.

Verified by parity against the previous revision, not by inspection:

* `breaking_point_report.py` — **64 outputs byte-identical** (8 row-sets ×
  4 argument-sets × `.md` and `.json`), covering no rows, a clean run, a knee, a
  pool-starved knee, a CPU-saturated knee, a run with no backend columns, one
  with no CPU or pool columns, and a run that died.
* `breaking_point_row.py` — **90 cases byte-identical** on both the JSON row and
  the verdict line (5 sample-CSV shapes × 6 k6-summary shapes × died/clean/
  non-zero-k6), including a missing summary, invalid JSON, a summary with no
  metrics, and k6's two metric spellings.
* End to end, driven the way the shell drives it, producing the report above.

Neither script has another caller: `breaking_point.sh` is the only one, and CI
calls only `breaking_point_compare.py`, which was not flagged and is unchanged.

---

## The HTTPS hotspots — the `marketplace.sh` precedent, applied

Six on the two scripts that bootstrap a real `apk` from the Alpine CDN
(`tests/heavy/apk.sh` ×4, `tests/heavy/airgap.sh` ×2). Every one is a
`curl -fsSL` whose output goes straight into `tar`, so `-L` following a
downgrade to plain HTTP is the whole exposure, and neither script pinned the
redirect chain.

Fixed with the wrapper 2026-08-30 arrived at the hard way, verbatim:

```bash
fetch_https() {
  curl -fsSL --proto '=https' --proto-redir '=https' "$@"
  return $?
}
```

A wrapper and not an array of flags spliced into each call site, because that
was tried once and raised three *new* hotspots on the three call sites it had
just de-duplicated: the rule reads the flags lexically at the `curl` token and
an array splat is opaque to it. The `return $?` is the same line the explicit-
return rule wants (below), so the two rules are satisfied by one shape.

## `pip install` without `--only-binary`

One, in `test.yaml`'s dex step. A source distribution runs its `setup.py` at
install time — an arbitrary script from the index, executed on the runner before
anything in the workflow gets a say. bcrypt publishes manylinux wheels, so
`--only-binary :all:` gives nothing up, and a wheel that ever goes missing now
fails loudly instead of quietly building from source. It was the only
`pip install` in any workflow.

## The permission hotspot — safe where reported, narrowed where real

`crates/adapters/src/repo/apk.rs`, `tar_of`'s `header.set_mode(0o644)`, flagged
"make sure this permission is safe" and marked `former-hotspot`.

**At the reported site it is safe.** The mode is a field of a tar header built
in memory and served over HTTP; no filesystem is asked for anything, nothing
here unpacks the archive, and 0o644 is what GNU `tar` writes for a regular file
and what `apk` expects to read back — RFC 0026 §13 records that the ustar
spelling of this header was measured against `apk.static`, not deduced.
Narrowing it would only produce an index some client refuses. That the scanner
flagged this one and not the sixteen other identical `set_mode(0o644)` calls in
the tree is itself the tell that it is matching a literal, not a data flow.

**But the question it asks is the right one to ask of the whole tree**, so the
sweep was done properly: every site was classified by whether this process asks
a *filesystem* for a right, or writes a *byte into an archive*. Only 3 of the
19 `set_mode` calls are production tar headers (`apk.rs`'s `tar_of`,
`pacman.rs`'s `generate_db`, `bundle.rs`'s `write_bundle`); the other 16 are
test fixtures. The real permission sites were four, and all four were wider
than they needed to be:

| Site | Was | Now | Why |
| --- | --- | --- | --- |
| `scanners/extract.rs` `write_bounded` | file 0o644 | **0o600** | the work dir holds an attacker-controlled artifact, unpacked. The only readers are this process and the scanner, which `subprocess::run` starts with `--unshare-user` and no `--uid`, so the invoking uid maps to itself inside the sandbox and reads its own files unchanged |
| `scanners/extract.rs`, five `create_dir_all` | umask (0o755) | **0o700** | the extracted tree was listable by every other local user. `create_dir_private` chmods each ancestor it actually created, and leaves ones that already existed alone |
| `cli/src/config.rs` `save` | umask (0o755) | **0o700** | the file inside was already 0600, but a 0755 directory still names every server and profile. `contract.rs` already restricted its own parent this way — this is the inconsistency, not a new rule |
| `cli/src/cli/proxy.rs` `write_state_file` | umask (0o755) | **0o700** | same shape: the state file is 0600 because the session in it is the secret, and the directory named it anyway |

Each is pinned by an assertion, not left to the umask of whoever runs the
tests: `a_tarball_extracts_with_exec_bits_dropped` now checks the created
directory as well as the file, `restrict_dir_to_owner_sets_0700` starts from an
explicit 0755 so it would fail if the call were a no-op, and
`the_state_file_is_private_and_names_the_service_url` checks its directory.

The apk/pacman/bundle tar headers are unchanged, and the reasoning is a comment
at the `tar_of` call site so the next reader meets it where the scanner does.
Resolved in the UI as safe.

Left alone deliberately: `storage/filesystem.rs` creates the artifact-cache
tree under the process umask (0o755 dirs, 0o644 files). That is wider than
necessary for a cache that can hold privately-published packages, but it is the
standard Unix contract — hard-coding 0o700 there takes the choice away from the
operator and breaks a deployment whose uid changes between rollouts while the
volume does not. It belongs in the deployment (umask, `securityContext`), not
in a literal.

---

## Maintainability

### Cognitive complexity (5)

| Location | Was → allowed | Split into |
| --- | --- | --- |
| `crates/config/src/schema/mod.rs` `validate_registry_apk` | 39 → 30 | `reject_apk_fields_on_other_kind`, `validate_apk_signing_mode`, `validate_apk_upstreams`, `validate_apk_signing`, `validate_apk_rotation`, `require_apk_signing_when_hosting`, and `is_apk_key_segment` — which removes a real duplication, the three-condition segment check the current key and every retired key spelled separately |
| `breaking_point_report.py` `main` | 51 → 15 | `preamble`, `outcome`, `queue_section` (+ `queue_clauses`, `pool_starved`, `cpu_saturated`, and one function per note), `steps_table`, `backends_table`, `footnote`, `render`, `summary_json`, `parse_args` |
| `breaking_point_row.py` `main` | 38 → 15 | `read_summary`, `error_pct`, `dropped_pct`, `duration_ms`, `build_row`, `judge`, `verdict`, `parse_args` |
| `breaking_point_row.py` `window` | 28 → 15 | `within`, `collect`, `peak`, `mid` + a `SAMPLED_COLUMNS` table, so the loop is three lines whatever the sampler learns to record next |
| `soak_verdict.py` `read_samples` | 16 → 15 | `column`, `sample_of` — the `num` closure was being rebuilt per row |

The 13 `anyhow::bail!` messages of `validate_registry_apk` were checked
character-for-character against the previous revision after the split, with the
Rust line-continuations collapsed: all 13 preserved, none added, none lost. The
order of the checks is unchanged, which matters because the first one to fail is
the one an operator reads.

### Explicit returns at the end of shell functions (18)

`apk.sh` ×6, `galaxy.sh` ×7, `nix.sh` ×3, `backends.sh` ×1,
`breaking_point.sh` ×1. All get `return $?`, not `return 0`: the implicit status
of a bash function is its last command's, so `$?` is what preserves behaviour
exactly, and it is the form `backends.sh`'s own `fetch()` already used.

Located by walking the definitions rather than by line number — SonarCloud's
numbers are for the last analysed revision, and several of these files had
already moved under the other fixes in this batch. Three functions beyond the
reported lines were included because they are the same shape and the scanner
will reach them next: `galaxy.sh`'s `said` and the nested `run_warm`, and
`backends.sh`'s `pip_install`.

### Positional parameters (2)

`galaxy.sh`'s `newest_versions` reached `$1` from inside a single-quoted Python
heredoc's argument list; `backends.sh`'s `installed_ok` interpolated `$1` into a
JavaScript string. Both now name a `local` first (`how_many`, `label`).

### Duplicated literals (4)

| Location | Literal | Now |
| --- | --- | --- |
| `authz.sh` | `user:authz-denied` ×4 | `SUBJECT_DENIED`, declared in the existing "the literals this suite repeats" block. The comment records why it is spelled differently from `T_DENIED`: one is the subject the oracle names, the other the credential, and an oracle asked about the token answers about a subject that holds nothing — an `allow`-shaped pass |
| `che_code_patch.sh` | `batlehub-che-code-src` ×5 | `CHE_SRC_CONTAINER` — it is created, copied from and removed on three paths including the failure one, and a name matching in only two of them leaks a container |
| `nix.sh` | `%{http_code}` ×4 | `HTTP_CODE` |
| `breaking_point_report.py` | `" and "` ×3 | `AND` — the reader should see the same conjunction whichever of the three queue notes wrote the sentence |

`tests/heavy/quarantine.sh` also has four `%{http_code}` and was **not** flagged
this round. Left alone rather than fixed on spec; if it appears next round it
takes the same constant.

### JavaScript (4)

* `13_apk_index_regeneration.js` — two `charCodeAt`. One becomes `codePointAt`;
  the other *disappears*, because `put` was a second copy of the same loop and
  is now `h.set(str(s), off)`. Both agree on every input this file encodes —
  tar header fields, a `.PKGINFO` of `key = value` lines, a generated package
  name, all ASCII by construction — verified by byte-comparing 25 synthesised
  ustar headers and 5 `str()` inputs against the old encoders: identical. `set`
  also *refuses* a field that would overrun the 512-byte header instead of
  dropping the overflow the way an out-of-range index does; every offset is a
  fixed ustar field and the longest path is well under 100 bytes.
* Same file — the CRC loop reads each byte in order and uses the index for
  nothing else, so `for (const byte of bytes)`. The two remaining index loops
  are over a fixed 512 rather than a length and were not flagged.
* `14_breaking_point.js` — the default export is named `breaking_point`. It was
  the only scenario of nine with an anonymous one.

### `:=` in an argument list (1)

`soak_verdict.py`'s `heap_mib=(heap / 1024.0 if (heap := num("heap_kb")) …)`.
The walrus is gone with the closure it was assigning in; `sample_of` reads the
three optional columns into names first.

## Accessibility — the clickable `<tr>` (1)

`AdminConfigReload.vue`'s change-history row was `@click` with
`cursor-pointer`, no `tabindex`, no `role` and no key handler, so opening a
config diff — the only way to see what a reload changed — was unreachable by
keyboard and announced nothing.

Fixed the way `PackageDetailPage.vue` records for its version list: **not** by
adding `onKeyDown` to the row, which is what the rule literally asks for and
would leave a non-focusable element with a key handler on it, but by moving the
interaction to a real interactive element. The date cell is now a `<button>`
with `aria-expanded`, `aria-controls` naming the detail row, and a `ChevronRight`
that rotates — a button brings the focus ring, Enter and Space with it, and
`aria-expanded` is what tells a reader whether the diff is open, which the
chevron says only to someone who can see it. Two strings added to both
catalogues (`showDiff`, `hideDiff`).

The page's own test drove the old interaction and now drives the new one,
asserting `aria-expanded` on both edges and that `aria-controls` resolves to a
row that exists.

---

## What was verified

| Gate | Result |
| --- | --- |
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test -p batlehub-config` | 341 passed |
| `cargo test --workspace` | 5 611 passed, 0 failed, 151 targets |
| `pnpm run lint` (oxlint) | clean |
| `pnpm run build` (vue-tsc + vite) | clean |
| `pnpm run test` (vitest) | 1327 passed, 112 files |
| `bash -n` on all six touched shell scripts | clean |
| `breaking_point_report.py` parity | 64 outputs byte-identical |
| `breaking_point_row.py` parity | 90 cases byte-identical |
| `validate_registry_apk` messages | 13 of 13 preserved character-for-character |
| ustar header encoding parity | 30 cases byte-identical |

The workspace run was repeated without the `| tail` the first one had: a
pipeline reports the *last* command's status, so the exit code of a piped
`cargo test` is `tail`'s and is 0 whatever cargo did. The number above is from
the unpiped run.

The heavy suites were **not** run: the six scripts touched in `tests/heavy/`
changed only in ways `bash -n` and reading can settle — a `curl` behind a
wrapper with two added flags, `return $?` where the status was already implicit,
and three literals named. Nothing changed about what any phase asserts. The
`apk`, `galaxy`, `nix`, `backends` and `airgap` phases are the ones to run
before believing that, and they need a fresh database
(`heavy-suite-fresh-database`) and must not overlap
(`heavy-suites-share-scan-jobs`).
