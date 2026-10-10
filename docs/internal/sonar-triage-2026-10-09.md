# SonarCloud triage — 2026-10-09

PR #195 (`feat/devfile`), analysed at `da50b11d`. The gate was red on
**reliability C** (one bug) and **security C** (four vulnerabilities).
Companion to [`sonar-triage-2026-10-08.md`](./sonar-triage-2026-10-08.md).

## `pythonsecurity:S8707` ×4 — fixed by taking no paths

The 10-08 fix to `deploy/siem/replay.py` (`inside_cwd()`: `realpath` plus a
`commonpath` check) was not modelled as a sanitiser, and both sinks stayed
reported (`load_stream`, `main`). The construction this analyser does accept is
the one 09-19 settled on: nothing path-shaped crosses the command line.

- `replay.py` reads its fixtures by their own location; a recorded stream comes
  in on `--stdin` with its expectations as `--expect-json`. `inside_cwd` is
  gone, and `tests/heavy/authz.sh`'s `audit-replay` step pipes instead of
  writing two files and `cd`-ing.
- `perf/scripts/profile_report.py` lost `--routes` and `--spec`
  (`read_inventory`, `spec_operations`): they were always
  `perf/profile_routes.txt` and `ui/openapi.json`, now derived from `__file__`.

## `python:S5850` — fixed in code

`OWN` in `profile_report.py` is now `(?:^<?batlehub)|(?:\[(?:crates|server|cli)/)`:
the anchor was already meant for the first alternative only; the grouping says so.

## Code smells

| Rule | Where | Disposition |
| --- | --- | --- |
| `shelldre:S7679` | `profile.sh` `need` | fixed, locals |
| `python:S6353` | `profile_report.py` `crate_of` | fixed, `\w` |
| `python:S8786` | `profile_report.py` `crate_of` | open — input is one stack frame of a symbol name |
| `python:S3776` (31) | `profile_report.py` `render` | open — report layout, not in the gate |
