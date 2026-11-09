# SonarCloud triage — 2026-11-08

PR 297 (RFC 0041 phases 0–6), analysed at `cb7c0cf1`: quality gate red on
new-code security rating (5 vulnerabilities) and new-code coverage (45.6 %).
Companion to [`sonar-triage-2026-10-13.md`](./sonar-triage-2026-10-13.md).

## `pythonsecurity:S8705` ×1, `S8707` ×3 — false positive, now in `sonar-project.properties`

`required_gate.py:64`, `merge_coverage.py:151`, `postmortem_new_risk.py:100`,
`pr_checklist.py:416`. The same finding as 10-13's ten `S8707`s, which were
resolved by hand in the UI — and so came back on every new gate script. Each
flagged value is a command-line argument the calling workflow or task sets;
none is reachable from a pull request's content. Whoever chooses the argument
already has the filesystem and the `git` the script would reach.

This time the rules are scoped out in `sonar-project.properties`
(`gateScriptCliArgs`, `gateScriptCliPaths`), to `.github/scripts/*.py` only,
so the next gate script does not reopen them. `required_gate.py` also now
accepts only commit ids for `--base`/`--head` and runs everything otherwise,
so a value starting with `-` cannot reach `git diff` as an option.

## `githubactions:S8233` ×1 — fixed

`front-lint.yaml` granted `security-events: write` at workflow level, which
phase 6's new `gate` and `required` jobs inherited. It is now on the `lint`
job, the only one that uploads SARIF.

## New-code coverage — scope

1 807 uncovered new lines, ~1 700 of them Python: the gate scripts, their
tests, and `tests/fixtures/supply-chain-jdd/probe_osv_severity.py`. Sonar
receives no Python coverage and counts the test files as source. RFC 0041
§12 rejected `coverage.py` for these scripts (their risk is their exit
status, which `unittest` asserts — `task test:scripts`, `scripts-test.yaml`),
so `.github/scripts/**` and `tests/fixtures/**` join `sonar.coverage.exclusions`
rather than being measured by a second tool.

## Code smells — open

`python:S3776` ×4 (`postmortem_new_risk.py` ×3, `merge_coverage.py`),
`S5864` ×3, `S5713` ×2, `S7632`, `S1192`, `S5778`, `shell:S7688` ×2,
`githubactions:S7677`, `S131` ×2, `rust:S1612` ×2. None moves the gate
(maintainability A); left for a cleanup pass.
