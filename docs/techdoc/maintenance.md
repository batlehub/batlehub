---
title: Maintenance
description: Health checks against the Weebo Dev Env guideline, last run 2026-10-18
---
<!-- techdoc:begin maintenance#notice -->
> Sections between `techdoc` markers are regenerated; write your own notes outside them.
<!-- techdoc:end -->

<!-- techdoc:begin maintenance#table -->
## Health checks

Run environment for this pass: no `task` binary and no access to `index.crates.io` or the npm registry, so no command could actually execute; rows below are graded from inspection, and every "Last run" says so.

| Check | Status | What's there | What's missing | Last run |
| --- | --- | --- | --- | --- |
| Audit | ⚠️ | `task audit` (`cargo audit`), `task deny` (`cargo deny check`), `task ui:audit` (pnpm), `task security` aggregates them + postmortem, VEX, SBOM, OpenAPI lint; trivy runs in CI (`image-scan.yaml`, `vuln-scan.yaml`) | `task audit` alone covers Rust only; result not verified | 2026-10-18 — not run (task unavailable, no crates index) |
| API docs generation | ⚠️ | utoipa + Scalar (`/scalar`); `task dump-spec` writes `ui/openapi.json`; `pnpm run generate` (HeyApi) builds `ui/src/client`; Spectral lint in `openapi-lint.yaml` | No `task gen:api-docs`; HeyApi uses `client-fetch`, not the axios plugin; staleness of `ui/openapi.json` not verified | 2026-10-18 — not run (no build possible) |
| CI/CD | ✅ | 24 GitHub workflows + `.forgejo/workflows/website.yaml`: `test.yaml` (clippy, fmt, fuzz check, tests, coverage) on push/PR, `front-lint.yaml`, `front-test.yaml`, `back-dep-audit.yaml` (push/PR/nightly), `dep-audit-frontend.yaml`, `build.yaml` (on tags), `helm-lint.yaml`, `image-scan.yaml`, `secret-scan.yaml`, `semgrep.yaml`, `codeql.yaml`, `sonar.yaml` | — | 2026-10-18 — inspected |
| Automated review | ✅ | Sonar, CodeQL and Semgrep on pull requests; `pr-checklist.yaml` on PR events | No `judge doc-review` | 2026-10-18 — inspected |
| Lint | ⚠️ | `task lint` = `cargo clippy --workspace -- -D warnings` + `cargo fmt --all --check`; `task ui:lint` (oxlint, eslint, oxfmt) | Result not verified | 2026-10-18 — not run (task unavailable; `cargo clippy` could not fetch crates) |
| Tests | ⚠️ | `task test`, `task test:all`, `task test:cli`, `task ui:test`, `test:pg-*`, `test:*-heavy`, `coverage-check` (80 %) | Result not verified | 2026-10-18 — not run (no crates/npm registry access) |
| Commit hooks | ❌ | `cog.toml` (tag prefix, `pre_bump_hooks`); gitleaks config `gitleaks.toml` | No `[git_hooks]` in `cog.toml` (no `commit-msg` `cog verify`), no `.hooks/` pre-commit | 2026-10-18 — inspected |
| Containerfile | ⚠️ | `Containerfile` multi-stage (rust builder, node ui-builder, distroless `cc-debian12` runtime), `Containerfile.hardened`, `Containerfile.worker`, `Containerfile.worker-guarddog`; all base images pinned by digest | Scan not possible: trivy not available | 2026-10-18 — trivy not available |
| Dependencies | ⚠️ | Updatecli (`updatecli-compose.yaml`), dependency bumps recorded in `CHANGELOG.md` | `cargo outdated` and `pnpm outdated` not installed / not runnable offline | 2026-10-18 — tools not available |

2 ✅ · 6 ⚠️ · 1 ❌
<!-- techdoc:end -->
