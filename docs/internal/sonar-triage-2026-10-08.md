# SonarCloud triage — 2026-10-08

PR #195 (`feat/devfile`), analysed at `7b23491c`. The gate was red on
**security rating on new code, C against A**, from one vulnerability.
Companion to [`sonar-triage-2026-09-22.md`](./sonar-triage-2026-09-22.md).

## `pythonsecurity:S8707` — `deploy/siem/replay.py:182` — fixed in code

"Path traversal via faulty LLM-supplied CLI arguments in `replay.load_stream()`".

`replay.py` is a developer and CI check (`task siem:check`, the `SIEM rules`
job), not server code, and it only reads. It still has no reason to open a
file outside the tree it runs in. `--stream` and `--expect` now go through
`inside_cwd()`: `os.path.realpath`, then an `os.path.commonpath` check against
the working directory, and an exit when the path falls outside it. CI and the
task both run from the repo root, so the defaults are unaffected. To replay a
stream captured elsewhere, run the script from a directory that contains it —
`tests/heavy/authz.sh audit` does exactly that for the stream it records.

## Code smells (12), all fixed in code

| Rule | Where | Change |
| --- | --- | --- |
| `python:S3776` (20 > 15) | `replay.py` `main` | split into `field_failures` and `replay_failures` |
| `rust:S3776` (32 > 30) | `devfile.rs` `compose_index` | arch filter moved to `serves_archs`; new test `the_arch_filter_keeps_versions_that_name_no_architectures` |
| `rust:S1612` ×2 | `audit_trail.rs`, `middleware/auth.rs` | `ToString::to_string`, `PoisonError::into_inner` |
| `shelldre:S7679` ×5, `S7682` | `authz.sh` `audit_npmrc`, `audit_pkg`, `audit_rows` | positional parameters bound to locals; explicit `return $?` |
| `python:S9409` ×3 | `soak_verdict.py` `costs_section` | list literal and `extend`; output checked identical against the previous version |
