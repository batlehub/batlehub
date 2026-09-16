# SonarCloud triage — 2026-09-14

The analysis of pull request **167** (`feat/new-reg`, commit `c7206be5`):
**49 new issues**, and the quality gate red on two conditions —
`new_security_rating` (actual D, threshold A) from the 7 `VULNERABILITY`s, and
`new_coverage` (actual 79.3%, threshold 80.0%). No hotspots.

[`sonar-triage-2026-09-11.md`](./sonar-triage-2026-09-11.md) is the previous
round; [`sonar-triage-2026-09-06.md`](./sonar-triage-2026-09-06.md) holds the
standing register of what is ignored and why.

Forty-three are fixed in code. Six are pinned in `sonar-project.properties`
under two new criteria — one an instance of a case already in the register
(npm's `dist.shasum`, which the format defines as SHA-1), one new
(`pythonsecurity:S8707`, a rule whose premise does not hold for this script).

> **Five of those six pins lasted one day.** The `pythonsecurity:S8707` pin was
> lifted on 2026-09-15 and `soak_verdict.py` validates its paths in code
> instead — see [the note at the end of that section](#s8707-superseded).

| Rule | Count | Type | What changed |
| --- | --- | --- | --- |
| `pythonsecurity:S8707` | 5 | vulnerability | **pinned** — `soak_verdict.py`'s argparse paths are not an agent's input; it has one caller. *(Lifted 2026-09-15: fixed in code instead — [see below](#s8707-superseded).)* |
| `githubactions:S8233` | 1 | vulnerability | `pull-requests: write` moved from `soak.yaml`'s workflow level to the one job that comments. |
| `rust:S4790` | 1 | vulnerability (critical) | **pinned** — `perf/mock-upstream` computes npm's `dist.shasum`. The *other* weak digest in that file was wrong for its format and is now SHA-256. |
| `python:S3776` | 3 | code smell (critical) | `parse_prometheus`, `plot` and `main` split into named helpers; output byte-identical. |
| `shelldre:S7682` | 15 | code smell | a trailing `return` on every flagged function — `$?` where the status is the assertion, `0` where the last statement is a log line. |
| `shelldre:S7679` | 14 | code smell | positional parameters bound to locals in eight functions, three files. |
| `shelldre:S1481` | 3 | code smell | one unused local removed from the signature outright, two documented as unused. |
| `python:S3358` | 3 | code smell | three nested conditional expressions extracted (`axis_gutter`, `check_growth_cell`, `check_verdict`). |
| `python:S3457` | 1 | code smell | an `f` prefix on a string with no replacement field. |
| `javascript:S4624` | 1 | code smell | `docs/build/rfc.mjs`'s `count()` lifts the inner template into a `unit` variable. |
| `typescript:S4624` | 1 | code smell | `registryTypes.ts`'s rustup snippet lifts `${reg}/rustup` into `updateRoot`. |
| `rust:S1612` | 1 | code smell | `.and_then(\|p\| p.parent())` → `.and_then(std::path::Path::parent)`. |

And the gate's second condition, which is not an issue at all:

| Condition | Before | After | What changed |
| --- | --- | --- | --- |
| `new_coverage` | 79.3% | ~83.0% | `tests/heavy/**` added to `sonar.coverage.exclusions`. |

---

## `new_security_rating` D — the seven vulnerabilities

### `githubactions:S8233` — a workflow-level write token

`.github/workflows/soak.yaml` declared `pull-requests: write` at the workflow
level, so all three jobs held it. Two of them (`k6-soak`, `heavy-soak`) check
out the tree and run a release build of the workspace against it; neither uses
the token for anything. Only `comment` does, and it says so — it resolves a pull
request from the branch when a dispatch has none.

The scope moved to that job. The workflow level is `contents: read`, and the
comment job re-declares `contents: read` beside its `pull-requests: write`,
because a job-level `permissions:` block replaces the workflow's rather than
adding to it.

### `rust:S4790` — SHA-1 in the mock upstream

`perf/mock-upstream/src/main.rs` had two weak digests, and only one of them was
required by a format.

**Kept and pinned** — the npm packument's `dist.shasum`, which is SHA-1 by
definition. This mock *is* the upstream the soak load pulls from, and
`crates/core/src/services/integrity.rs` verifies what an upstream advertises
against the bytes it serves: a packument whose `shasum` is not the SHA-1 of the
tarball has every artifact read refused with a 502. The `sha1` dependency's own
note in `perf/mock-upstream/Cargo.toml` records that this happened once already,
silently, until the soak harness tried to seed a warm cache. Same argument as
`hashNpmShasum` and `hashUpstreamDirShasum` in the register, from the third side
of the wire.

**Removed** — the RubyGems compact-index `|checksum:` field, which is *not*
SHA-1: the format defines it as the SHA-256 of the gem. The mock was emitting a
synthetic SHA-1 there, which was wrong about the format as well as weak. It
emits `sha256_hex` now (`sha2 = "0.11"`, matching the workspace root's version
line), so the pin covers one call site rather than two, and covers it for a
reason that is checkable against a specification. This is the same recheck that
deleted four of the original thirteen weak-hash sites on 2026-08-31.

### `pythonsecurity:S8707` ×5 — "agentic workflows should not be vulnerable to path injection"

The rule treats a script's `argparse` arguments as attacker-controlled on the
premise that an LLM supplies them, so a prompt injection reaches the path. The
five sinks are the files `perf/scripts/soak_verdict.py` reads and writes: the
samples CSV, the marks file, the SVG chart and the report, plus the metrics
expositions.

The premise does not hold here. `grep -rn soak_verdict` finds exactly one
caller, `perf/scripts/soak.sh`, which passes paths it built itself from its own
`mktemp -d` work directory and the `REPORT` the Taskfile sets. The script is not
registered as a tool anywhere, has no MCP wrapper, and is not on an agent's PATH
by intent.

There is also no boundary available to enforce. The script's whole contract is
"read the samples at this path, write the report to that one", and both sides
are named by the harness; rooting them under a fixed directory would not
sanitise an input, it would delete the interface. Pinned to the one file. A path
built from a *request* is a real finding and a different rule
(`pythonsecurity:S2083`), which stays on everywhere.

#### Superseded, 2026-09-15 — the pin is gone and the paths are validated {#s8707-superseded}

The next round of the same analysis raised S8707 again, on the two scripts
added since: `perf/scripts/perf_report.py` (one at Blocker, two at High) and
`perf/scripts/record_run.py` (one at High). Fixing those two made the second
half of the argument above wrong, so it is retracted rather than re-applied.

**There is a boundary, and it does not delete the interface.** Every path any
of the three scripts is given resolves inside one of three known directories:
the repository the run is measuring, the working directory it was launched
from, and the temporary directory the samplers write into (`mktemp -d` for
`soak.sh`, `mktemp` for `run_with_metrics.sh`). A `measurement_path` validator
on the `type=` of each argument resolves the value — which collapses `..`,
follows a symlink and normalises an absolute path in one step — and refuses
anything outside those three roots. The contract survives intact: every real
call site still passes, checked against `soak.sh`'s work directory plus a
report under `perf/results/`, and `--report ~/.ssh/authorized_keys` is now a
usage error. `perf_report.py` gets the same validator with the tighter root set
its own arguments justify (the working directory alone).

The first half of the argument still stands — the script is nobody's tool and
has one caller — which is why this is a cheap check rather than an urgent fix.
One caller today is not a boundary, and the check costs a resolve per argument.

`soakVerdictPaths` is removed from `sonar-project.properties`. If the next
analysis still reports the five — the Python analyzer may no more model
`resolve()` + a containment test as a sanitiser than the JavaScript one does,
which is exactly why `stubServerPath` is pinned — then it is re-pinned on *that*
claim, with the guard named in the comment. That is a different statement from
this one, and it is the only form the pin should come back in.

**It did, and it is.** The same analysis re-raised all five at their new line
numbers, and raised the two rules on the other two scripts as well.
[`sonar-triage-2026-09-15.md`](./sonar-triage-2026-09-15.md) records the
re-pin: four criteria naming the guard, the probes that show it refuses an
escape, and the reason the code fix stays even though the findings are
suppressed.

---

## `new_coverage` 79.3% — three Python files in the heavy tree

The shortfall was 2 996 new lines to cover with 621 uncovered. 136 of those
621 — every one of them uncovered — are three files under `tests/heavy/`:

| File | New lines | Uncovered |
| --- | ---: | ---: |
| `closed_proxy.py` | 106 | 106 |
| `http_tap.py` | 28 | 28 |
| `patch_product_json.py` | 2 | 2 |

They are harness, not product: `closed_proxy.py` is the air-gapped forward
proxy, `http_tap.py` is the wire recorder every closed-world assertion reads,
`patch_product_json.py` is the editor fixture. They *are* executed, by the heavy
suites in CI — which run outside the `cargo llvm-cov` report Sonar reads. So
their lines were being counted as uncovered by a run that never invoked them,
which measures nothing.

`tests/heavy/**` is now in `sonar.coverage.exclusions`, beside `perf/**`, which
is there for exactly the same reason. The entry is Sonar-only: `COVERAGE_EXCLUDE`
in `.tasks/coverage.yaml` is a cargo-llvm-cov list and never sees a `.py`.

New coverage without them is (2 860 − 485) / 2 860 = **83.0%**.

### What this does not fix

The exclusion clears the gate, and it does not clear the gap it was hiding
behind. The largest uncovered blocks left are product code and are a real
follow-up, not a scanner artefact:

| File | New lines | Uncovered |
| --- | ---: | ---: |
| `crates/web/src/handlers/proxy/conda.rs` | 372 | 190 |
| `crates/core/src/services/proxy/handle.rs` | 218 | 70 |
| `crates/web/src/handlers/proxy/jetbrains_marketplace/ide.rs` | 102 | 33 |
| `crates/adapters/src/registry/rustup/client.rs` | 129 | 23 |

`conda.rs` is half a shipped handler with no in-process test over it —
`conda_index_cache.rs` covers the sharded-index path and nothing else. A
`local_conda_registry.rs` in the shape of the other `local_<type>_registry.rs`
files is what that wants.

---

## `python:S3776` ×3 — cognitive complexity in `soak_verdict.py`

Three functions over the limit of 15: `parse_prometheus` (16), `plot` (25) and
`main` (59). All three were split rather than annotated, and the split is
behaviour-preserving — the report and the SVG are byte-identical before and
after, and the exit codes match, checked against
`git show HEAD:perf/scripts/soak_verdict.py` on six inputs: a plain run, one
with a metrics pair so the per-registry cost table renders, one with a
panicking server log and a k6 summary, one with a malformed k6 summary so the
parse fallback runs, one writing the SVG, and one with too few samples so the
inconclusive path runs.

- **`parse_prometheus` → `parse_prometheus_line` + `parse_labels`.** One
  exposition line in, `((name, labels), value)` or `None` out. The loop is three
  lines now and the label parsing is testable on its own.
- **`plot` → `plot_one` + `bucket` + `plot_grid` + `axis_gutter`.** The
  per-series body was the whole function; it is `plot_one` now, and the row
  arithmetic that appeared twice (once for the point, once for the join to the
  previous point) is a local `row_of`.
- **`main` → thirteen helpers.** `parse_args`, `resource_checks`,
  `read_panics`, `read_k6`, `read_costs` for the inputs;
  `exempt_rss_during_warmup` for the one rule that mutates a check;
  `header_lines`, `checks_table` (with `check_verdict` and `check_growth_cell`),
  `slope_row`, `chart_section`, `svg_section`, `costs_section`, `tail_lines`
  for the report. `main` is now the three windows, the verdict, and the
  assembly of sections in order — about six branch points.

  `exempt_rss_during_warmup` is the one worth naming: it is why a short soak
  does not fail on "idle RSS grew", and inside `main` it read as four lines of
  mutation in the middle of the verdict.

`python:S3358` ×3 and `python:S3457` fell out of the same pass: the three nested
conditional expressions are `axis_gutter`, `check_growth_cell` and
`check_verdict`, and the `f` prefix with no replacement field was one line of
the report's opening sentence.

---

## `shelldre:S7682` ×15 — an explicit return

Same treatment as the 18 on 2026-09-11, and the same caution: a bash function
that falls off its end returns the status of its last command, so an appended
`return 0` where the status is the assertion turns a failing suite green. That
has cost this project a green heavy run before, for the neighbouring reason (a
`| tail` reporting tail's status).

So `return $?` on the eleven whose last statement is the thing being measured —
a fixture write (`authz_cargo_home`, `authz_mvn_settings*`, `authz_live_netrc`,
`authz_read_rows`), an assertion (`authz_read_row`), a measurement
(`soak_sample`, `soak_window`, `soak_round`), a value (`authz_live_cred`,
`toolchain_dir`) — and `return 0` on the four whose last statement is a log line
after the assertions have already run (`authz_live_pair`,
`authz_check_kinds_covered`, `phase_reads`) or a `case` whose every arm either
succeeds or calls `heavy_fail`, which exits (`phase_live`).

## `shelldre:S7679` ×14 and `shelldre:S1481` ×3 — positionals and one dead local

Eight functions bind their positionals at the top now: `cargo_arm` and `mvn_arm`
in `authz.sh`, the three `live_run_{github,forgejo,gitlab}` forge runners,
`hits_for` in `rustup.sh`, and `need` and `quiesce` in `perf/scripts/soak.sh`.

The three unused locals were not all the same thing:

- **`reg` in `live_run_mise_forge`** was dead — the rewrite rule its callers
  build already carries the registry. It is gone from the signature, not just
  from the binding, so the three callers stop passing a value nothing reads;
  `rewrite` is `$7` now.
- **`suffix` in `live_run_rpm` and `live_run_pacman`** is part of the runner
  contract every `live_run_*` shares, and these two genuinely do not need it:
  the container is the isolation, so the arm needs no distinct work directory.
  The binding is dropped and the signature comment says why.
