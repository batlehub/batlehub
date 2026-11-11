# SonarCloud triage — 2026-11-11

`main` analysed at `2fbe79e7`: gate red on new-code reliability (7 bugs) and
security (7 vulnerabilities). Companion to
[`sonar-triage-2026-11-08.md`](./sonar-triage-2026-11-08.md).

## Bugs — fixed

- `typescript:S9383` ×6: unawaited `router.push`/`replace` and `load()` are
  now explicitly `void` (FlagsPanel, PackageVersionsTable, AppHeader,
  UserMenu, LoginPage ×3).
- `typescript:S2871`: AdminBulk's registry scope sorts with `localeCompare`.

## Vulnerabilities — pinned in `sonar-project.properties`

- `pythonsecurity:S8707` ×4 / `S2083` ×2 on `perf/scripts/`: the same
  guard-not-modelled and content-as-path findings as 2026-09-15/09-19, which
  were resolved per issue in the UI and are open again on `main`. Pinned as
  `perfScriptCliPaths` (S8707, `perf/scripts/*.py`) and `perfReportContent`
  (S2083, `breaking_point_report.py` only). This reverses 09-15's "not
  pinned": the per-issue resolution did not survive.
- `secrets:S6706` on `oidc.rs`'s test key: jsonwebtoken's public fixture,
  already allowlisted for that file by `gitleaks.toml`. Pinned as `oidcTestKey`.

## Code smells

Fixed: `rust:S1612` ×4, `typescript:` S7755 ×3, S6594 ×3, S5906 ×8, S7773,
S7722, S6582, S3358; `shell:S7688` ×5, `S131` ×5, `S1481`;
`python:` S1481, S5713 ×2, S5864 ×3, S7632.

Left open (refactors that move no gate condition): `S3776` cognitive
complexity (Rust ×4, Python ×5, `router/index.ts`), `shell:S7679` positional
parameters, `S1192` literal duplication (including the migration, which is
append-only), `Web:S6819` role suggestions, `typescript:S4782` on the cva
variant props, `rust:S1612` on `extract.rs`'s `is::<ZipError>`,
`githubactions:S7677` (`::error` workflow commands go to stdout),
`javascript:S7761`, `python:S5778`.
