# SonarCloud triage — 2026-10-13

`main`, analysed at `9edb2544`: 410 open issues (392 smells, 14
vulnerabilities, 4 bugs). Companion to
[`sonar-triage-2026-10-09.md`](./sonar-triage-2026-10-09.md).

## `cli/assets/**` ×334 — excluded

`cli/assets/java-engine/batlehub-java.mjs` is batlehub-vsx's Java engine, an
esbuild bundle copied in by that repository's `task engine:sync` so that
`cli/src/cli/java.rs` can `include_bytes!` it. A fix here would be overwritten
by the next sync. `cli/assets/**` is in `sonar.exclusions`, and batlehub-vsx
now has its own SonarCloud project (`batleforc_batlehub-vsx`), where the
findings are fixed at their source.

## `pythonsecurity:S8707` ×10, `pythonsecurity:S2083` ×1 — false positive

Resolved as false positive in the UI, with this note quoted in a comment on
each issue. The scripts are `.github/scripts/{vex,mise_cve_report,slim_sbom,
sonar_quality_gate,merge_coverage}.py` and `perf/scripts/breaking_point_report.py`.

Every flagged path is a command-line argument (`--out`, `--doc`,
`--trivy-json`, an lcov `NAME=PATH`) set by the workflow or task file that
runs the script. None of them is reachable from remote input: whoever can
choose the argument already has the filesystem the script would read or write.
The 10-09 approach of taking no paths at all does not apply here, because
these scripts exist to read and write the files the workflow names.

`vex.py:224`'s BLOCKER (S2083) is the same `--out` as its S8707 twin on that
line.

## `rust:S2612` — fixed in code

The `APKINDEX` tar header mode in `crates/adapters/src/repo/apk.rs` is now
`0o640`. apk reads the entry by name and type and never extracts it. The heavy
`apk.sh` suite passed against both generations: `apk update` verifies the
locally signed index, then fetches and blocks.

## Code smells fixed

| Rule | Where | Fix |
| --- | --- | --- |
| `python:S3776` ×7 | `mise_cve_report.py` ×4, `vex.py`, `merge_coverage.py` | Split into helpers; old vs new output compared byte-for-byte |
| `python:S1192`, `S9409` ×4, `S1172`, `S5713` | `mise_cve_report.py`, `pr_checklist.py`, `sonar_quality_gate.py` | Constant, `extend`, unused parameter, `URLError` (an `OSError`) |
| `python:S6395` ×2 | `perf/scripts/profile_report.py` | `OWN` split into a `startswith` prefix and an unanchored path regex. Keeping the group (S5850, 10-09) and removing it (S6395) contradict each other; the split satisfies both |
| `javascript:S8786` ×2, `S6557` ×4 | `docs/build/page-description.mjs` | Linear link/image regexes (bracket and paren classes exclude their own opener), `startsWith`. Same descriptions on all 263 pages |

## `javascript:S9382` ×46 — open

"`await` inside a loop" in the two rendered design gates
(`{ui,docs}/build/design-routes.mjs`), `tests/heavy/{vsx_view,console_fetch}.mjs`,
`docs/build/check-mermaid.mjs` and `ReadmePanel.vue`. Every loop either drives
one shared browser one page at a time (it shares a profile and its
`localStorage`), polls until a condition holds, or calls a function that keeps
global state (`mermaid.parse`, Shiki's lazy grammar load). Not decided yet.

The duplication around those loops was real and is factored out:
`tests/heavy/browser.mjs` now holds what `console_fetch.mjs` and `vsx_view.mjs`
each carried (argument parser, browser and private context, the 500 ms
`poll`); `vsx_view.mjs` answers dialogs, refreshes the view and checks the
signed-in listing through one helper each; `docs/build/design-routes.mjs` sets
up a plan's page (theme, off-box recorder, a seed's request blocking) in
`openPlanPage` instead of twice. The two `design-routes.mjs` stay separate:
they belong to two pnpm projects, each resolving its own `puppeteer-core` and
`axe-core`. This lowers the S9382 count only where a copied loop disappeared;
the remaining loops are sequential on purpose.
