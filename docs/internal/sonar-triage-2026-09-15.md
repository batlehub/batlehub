# SonarCloud triage — 2026-09-15

Two rounds on `feat/new-reg` (PR **167**), the second one caused by the first.
[`sonar-triage-2026-09-14.md`](./sonar-triage-2026-09-14.md) is the previous
day; [`sonar-triage-2026-09-06.md`](./sonar-triage-2026-09-06.md) holds the
standing register of what is ignored and why.

**Round 1 — 7 issues, all fixed in code.** Four path-traversal findings on the
two perf scripts added since the last round (`pythonsecurity:S8707` ×3 and one
`pythonsecurity:S2083` Blocker), one `python:S3776`, one `githubactions:S8549`
and one `javascript:S7765`.

**Round 2 — 9 issues, open, and deliberately not suppressed.** Three of the
four code fixes closed their findings. The path ones did not: the rules flag
the sinks *below* the validator those fixes added, and the same analysis
re-raised the five on `soak_verdict.py` whose pin had been lifted the same day.
A file-wide ignore was written for them and then **reverted** — see
[Why these are not pinned](#not-pinned). The gate stays red on
`new_security_rating` (actual **5**, threshold 1) while they are open.

| Rule | Round 1 | Round 2 | What changed |
| --- | --- | --- | --- |
| `pythonsecurity:S8707` | 3 | 8 | **fixed in code, still reported** — a resolve-and-confine validator on every path argument of all three `perf/scripts/*.py`. Open: the analyser does not model the guard. Not pinned. |
| `pythonsecurity:S2083` | 1 (Blocker) | 1 (Blocker) | same sink, same guard, same visibility problem. Open. Not pinned — this is the Blocker that drives the rating to E. |
| `python:S3776` | 1 | **0** | `perf_report.py`'s `render` (29) split into `preamble`, `table`, `measured_cells`, `delta_cells` and `footnotes`. Closed. |
| `githubactions:S8549` | 1 | **0** | `perf-report.yaml` built `perf/mock-upstream` without `--locked`; it was the only `cargo build` in any workflow missing it. Closed. |
| `javascript:S7765` | 1 | **0** | `12_conda_filter.js` uses `.includes(BLOCKED_FILE)`. Closed. |

Statuses read from the public API rather than the UI, which needs no token:

```
api/issues/search?componentKeys=batleforc_batlehub&pullRequest=167&additionalFields=_all
```

---

## The path findings — the arc, in three moves

Worth writing down in order, because the middle move is the one a reader will
otherwise repeat.

**Pinned, 2026-09-14.** Five `pythonsecurity:S8707` on `soak_verdict.py`,
pinned on two arguments: the rule's premise is that an LLM supplies the
arguments, which does not hold for a script with exactly one caller; and "there
is no boundary to enforce — the script's whole contract is *read the samples at
this path, write the report to that one*, and rooting them under a fixed
directory would delete the interface".

**Lifted, 2026-09-15.** Round 1 raised the same rule on `perf_report.py` and
`record_run.py`, which made the second argument testable — and it was wrong. A
root set of *the repository, the working directory, and the temporary directory
the samplers write into* refuses an escape without touching the interface:
every real call site still passes, checked against `soak.sh`'s `mktemp -d` work
directory, a report under `perf/results/`, and `run_with_metrics.sh`'s `/tmp`
sample files. So `workspace_path` / `measurement_path` were written for all
three scripts and the pin was removed.

**Re-pinned, 2026-09-15, on a different claim.** The re-analysis returned nine
open findings. Every flagged line is a sink *downstream* of the guard, and the
five on `soak_verdict.py` are the same five as yesterday at new line numbers —
the old ones (L91, L127, L449, L500, L734) are closed, the guard having moved
them down by 32 lines:

| File | Lines | Rule | Sink |
| --- | --- | --- | --- |
| `perf_report.py` | 81 | S8707 | `path.read_text()` |
| `perf_report.py` | 325 | S8707 **and** S2083 | `args.out.write_text()` |
| `record_run.py` | 67 | S8707 | `path.read_text()` |
| `soak_verdict.py` | 123, 159, 532, 867, 915 | S8707 | `path.open()`, `read_text()`, three `write_text()` |

The analyser does not treat `Path.resolve()` plus a containment test as a
sanitiser. That is not a new discovery here: `stubServerPath` has been pinned
since 2026-09-06 for the identical limitation in the JavaScript analyser
(`jssecurity:S6549`, `resolve()` + `startsWith()` on
`ui/build/stub-server.mjs`). A stricter guard would not be stricter — only a
different shape the rule also does not recognise.

What the guard does, and what was checked before pinning:

```
perf_report.py  --out /etc/cron.d/pwn                → error: resolves outside …  (exit 2)
perf_report.py  --runs ../../../../../../etc/passwd  → error: resolves outside …  (exit 2)
record_run.py   --out /etc/cron.d/pwn                → error: resolves outside …  (exit 2)
soak_verdict.py --samples /etc/passwd                → error: resolves outside …  (exit 2)
soak_verdict.py --report /root/.ssh/authorized_keys  → error: resolves outside …  (exit 2)
```

`perf_report.py` gets the tighter root set its own arguments justify — the
working directory alone, since every path it touches belongs to the tree it is
reporting on. The other two also accept the temporary directory, because that
is where the sampler writes the RSS, CPU, marks and metrics files they read.

## Why these are not pinned {#not-pinned}

Four `sonar.issue.ignore.multicriteria` entries were written for these nine —
S8707 on the three scripts, S2083 on `perf_report.py` — and then reverted
before they were committed. Three reasons, and the first is the one that
decides it:

1. **The mechanism is wider than the finding.** `multicriteria` scopes to a
   *file*, never to a line or a sink. Pinning S8707 on `soak_verdict.py`
   silences the five sinks the validator already guards **and** the sixth one
   somebody adds next year without it. That sixth is precisely the finding
   worth having.
2. **The rule's premise now holds.** The 2026-09-14 pin argued that these
   scripts are nobody's tool — "not registered as a tool anywhere, has no MCP
   wrapper, and is not on any agent's PATH by intent". That stopped being true
   the same week: an agent session drove `perf_report.py`, `record_run.py` and
   `soak_verdict.py` repeatedly with paths it composed itself. A rule about
   agent-supplied paths firing on scripts an agent actually runs is not a
   false positive about the risk — only about the mitigation being invisible.
3. **S2083 is the Blocker rule for the real thing.** The register says it
   stays on everywhere, and one file's exemption is how that becomes two.

What is true, and is the reason none of this is urgent, is that the guard
works: every probe above is refused at the argparse boundary, before a path
reaches `read_text`, `open` or `write_text`. The exposure is a rating, not a
traversal.

### What was done instead

**`perf_report.py` no longer takes a path.** `--runs`, `--out`, `--json` and
`--baseline` are file *names*; the directory is `perf/results/`, derived from
the script's own location (`Path(__file__).resolve().parents[1] / "results"`).
A value containing a separator is refused rather than stripped, because a
caller that passes `../x.json` and gets `results/x.json` has been helped in the
wrong direction:

```
--out /etc/cron.d/pwn         → error: '…' is a path; this argument takes a file name
--out ../../../../etc/pwn.md  → error: …
--runs ../runs.jsonl          → error: …
--out .                       → error: '.' is not a file name
```

There is no tainted value left to reach `read_text` or `write_text`, so both
rules are satisfied by the code rather than by a suppression — S2083's Blocker
included. Output is byte-identical to the previous version on the same
fixtures, the CI argument form (`--out perf-report.md --json perf-report.json`)
was already name-shaped and is unchanged, and what moved is where the files
land: `perf-report.md`/`.json` are written to `perf/results/` instead of the
workspace root. `perf-report.yaml` downloads the baseline into `perf/results/`,
reads the summary from there, and attaches those paths to the release;
`task perf:report:compare BASE=` now takes a name, which is what
`perf/README.md` already documented.

**The other two keep the resolve-and-confine guard**, because the same
treatment is not available to them: `record_run.py` and `soak_verdict.py` read
the sampler's RSS, CPU, marks and metrics files out of a `mktemp -d` directory
whose name is random by design, so the path has to be an argument and
`measurement_path` (repository ∪ working directory ∪ temporary directory) is
the strongest form the interface allows. Their eight S8707 findings stay
**open**, to be resolved per issue in the SonarCloud UI — the only mechanism
narrow enough to leave the rule armed for a future sink that skips the
validator. Until then `new_security_rating` stays above A on this branch, and
that is the honest reading: the guard is tested, the analyser cannot see it,
and nothing in the repository claims otherwise.

A path built from a **request** remains a real finding under S2083 everywhere
in this repository, and nothing here changes that.

### The refactor that came with it

`render()` in `perf_report.py` was 29 cognitive complexity and is now three
lines over four helpers. Verified by parity rather than by reading: `git show
HEAD:` and the new file, over a synthetic `runs.jsonl` holding a row with no
`k6` block, a null `cpu_pct`, a non-zero `k6_exit`, an unknown label and a
duplicate label, with and without a baseline — identical markdown, identical
JSON, identical `--all` output, identical `--fail-on-regression` exit code.
`soak_verdict.py` got the same treatment against the real `perf/results/`
fixtures from the 2026-09-14 soak: identical `soak.md`, identical
`soak-chart.svg`, identical exit codes in both the full and the minimal
configuration.

---

## Audit of the standing ignores

Twenty-nine `sonar.issue.ignore.multicriteria` criteria were in
`sonar-project.properties`. Each was checked two ways: does the `resourceKey`
still name a file, and does that file still contain the construct the entry
excuses. Twenty-seven hold. Two did not and are removed.

### Removed — the construct is no longer there

| Criterion | Rule | Why it is gone |
| --- | --- | --- |
| `vhtmlSetupGuide` | `Web:S5247` | `ui/src/pages/SetupGuide.vue` has no `v-html` any more. The only one in `ui/src` is `CodeBlock.vue:20`, which keeps its own entry. |
| `hashMockUpstreamShasum` | `rust:S4790` | The 2026-09-14 refactor moved `sha1_hex` out of `perf/mock-upstream/src/main.rs` into `src/support.rs`; the entry pointed at a file with no weak digest left in it. |

`hashMockUpstreamShasum` is the more interesting of the two. The SHA-1 it
excused now lives at `support.rs:72` and carries `// NOSONAR -- required by
legacy dist.shasum wire formats`, and the API reports `rust:S4790` on that file
as **CLOSED** — so the line comment is what is holding, not the pin. That is
the narrower mechanism of the two: it suppresses one line rather than a file,
and it sits where a reader of the code will see it. If the next analysis
re-opens the finding on `support.rs`, the lesson is that `NOSONAR` is not
honoured for this rule and the entry comes back pointed at **`support.rs`** —
which is the truthful resourceKey either way.

### Worth a decision, not removed

- **`tableHeaders` (`Web:S5256`, `ui/src/**/*.vue` — 105 files)** is the widest
  entry in the file. Only three `.vue` files contain a raw `<table>`, and the
  two pages among them have their headers (`AdminAuthorization.vue` 21 `<th>`,
  `AdminConfigReload.vue` 5); the construct the rule actually trips over is
  `Table.vue` plus whichever pages compose `<Table>` without the analyser
  tracing a `<th>` through the slots. A hand-written header-less `<table>` in a
  new page is silenced today. Narrowing it to
  `ui/src/components/ui/table/**` and letting the next analysis name the pages
  that still report would cost one round and buy back the rule everywhere else.
- **`hashRubygemsRange` (`rust:S4790`)** is pinned on a *compatibility
  retention*, which its own comment says out loud: Bundler 2.7.0 removed MD5
  digesting of compact-index responses on 2025-07-16, so the ETag serves only
  older clients and changing it would cost them a refetch rather than break
  them. It is the one entry in the weak-hash family that a **code** change can
  retire, the day support for Bundler < 2.7 is dropped. Until then it is a
  retention, not a requirement — and it should not be read as one.
- **`heavyClearText` (`shell:S5332`, `tests/heavy/**` — 81 files)** rests on a
  sound argument (every suite drives a real client at `127.0.0.1` over plain
  HTTP, and several clients document working around the refusal). The residue:
  it would equally silence a future suite that talks to a real external host
  over `http://`, which is a finding worth having. No change today; a note for
  whoever adds a suite that reaches outside localhost.

### The bigger exclusion, outside the criteria

`sonar.exclusions=**/examples/**` is the only setting in the file carrying **no
comment at all**, and the glob matches two different trees: the user-facing
`examples/` (55 files, 5 shell scripts a reader is invited to run) and the
`crates/examples` crate — the integration-test helpers and the smoke /
real-proxy binaries. Nothing in either is analysed, for any rule, and no
argument for that is recorded anywhere. Whatever the answer turns out to be, it
should be a written one.

### Verified, unchanged

The remaining twenty-five were confirmed against the code rather than assumed:
the ten weak-digest entries all still compute the digest their comment names
(`integrity.rs`, `eco_rubygems.rs`, `range.rs`, the compact-index test,
`openpgp.rs`, `listing_synthesis.rs`, the Maven proxy, `air_gap.rs`,
`upstream_audit.sh`, `upstream_dir.sh`); the five `rust:S2612` entries still
set a tar header mode or clamp one; `oidcNonceDecode` still has `verify_nonce`;
`stubServerPath` still has the `resolve()` + `startsWith()` guard before
`existsSync`; `tableTabindex`, `bulkPreTabindex`, `comboboxRoles`, `meterRole`
and `vhtmlCodeBlock` still carry the attribute or role they argue for; every
`FROM` in the four `Containerfile*` still pairs a tag with a digest; and
`Containerfile.worker-guarddog` still builds `FROM ${WORKER_IMAGE}`.

### One observation about mechanism

Three suppression mechanisms are now in use for overlapping ground: this file's
criteria, one `// NOSONAR` (`support.rs`, the only one in the repository), and
one `eslint-disable vue/no-v-html` (`ReadmePanel.vue`, whose `v-html` has no
entry here). The register is meant to be the single home for "what is ignored
and why". Two suppressions live outside it.
