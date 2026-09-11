# SonarCloud triage — 2026-09-10

The analysis of `main` (commit `745d26d3`, the merge of PR #146): **6 open
issues typed `VULNERABILITY`, all `MAJOR`, all in the two proxy Containerfiles**,
and the quality gate red on `new_security_rating` alone (actual 3, threshold 1).
No hotspots. Companion to
[`sonar-triage-2026-09-09.md`](./sonar-triage-2026-09-09.md);
[`sonar-triage-2026-09-06.md`](./sonar-triage-2026-09-06.md) holds the standing
register of what is ignored and why.

All six are fixed in code, and so are the seven code smells open on `main`
beside them — nothing was resolved as won't-fix, and `sonar-project.properties`
is unchanged by this round. `Containerfile` and
`Containerfile.hardened` are the same file with a different base, and each
finding appears once per file.

| Rule | Count | What changed |
| --- | --- | --- |
| `docker:S8549` | 4 | both `cargo build --release` lines take `--locked`. |
| `docker:S6505` | 2 | `npm install -g pnpm@11.25.0` takes `--ignore-scripts`. |
| `rust:S3776` | 2 | code smell — `handle_grants` (23) and `handle_token_command` (22) in the CLI. |
| `docker:S6570` | 4 | code smell — `"$crate"` quoted in the stub loop of both files. |
| `docker:S7018` | 1 | code smell — the apt package list sorted. |

---

## `docker:S8549` ×4 — the two cargo builds

"Using dependencies without locking resolved versions." The image already copies
`Cargo.lock` before anything else and the lockfile is what CI builds against
(`build.yaml` and `codeql.yaml` both pass `--locked`; so does
`Containerfile.worker`, which was not flagged), so the proxy image was the one
build in the repository that would have accepted a lockfile update silently.
With `--locked`, cargo fails instead of rewriting `Cargo.lock` inside the build
container — where nobody would see the change.

Both lines are changed, not just the second. The first is the dependency-cache
build over stubbed crates (`2>/dev/null; exit 0`, so its failure would be
invisible); resolving there without `--locked` and with it afterwards would
compile the dependency layer against one graph and the binary against another,
which defeats the cache the stub exists for.

Checked without a container runtime: the stubbed manifest tree (the exact set
of files the first `RUN` sees) resolves under `cargo metadata --locked` and
leaves `Cargo.lock` byte-identical, and the real tree does too.

## `docker:S6505` ×2 — the pnpm bootstrap

"Omitting `--ignore-scripts` allows lifecycle scripts to run during package
installation." The flagged line is the global `npm install` of pnpm itself, not
the project install: `pnpm install --frozen-lockfile` two lines later was not
flagged, because pnpm 10+ denies install-time scripts by default and this
repository allow-lists the ones it needs in `ui/pnpm-workspace.yaml`.

`pnpm@11.25.0`'s manifest declares no `preinstall`/`install`/`postinstall`
(checked against the registry document), so the flag changes nothing about
today's build. It is added anyway, and the comment beside it says why: the
CI workflows (`front-lint.yaml`, `front-test.yaml`, `sonar.yaml`) already state
the same policy on their own installs, and a pnpm release that gained a script
should not be the thing that first runs one inside the image build.

Checked by installing `pnpm@11.25.0` with `--ignore-scripts` into a temporary
prefix under node 24: `pnpm --version` answers `11.25.0`.

## `rust:S3776` ×2 — two CLI dispatchers, no behaviour change

Both are `match` dispatchers whose arms each carried a `json`/table branch, and
in both the table branch was the whole of the complexity. The pattern is the one
`admin.rs` already uses everywhere else — `print_namespaces_table`,
`print_flags`, `print_exposure` — a `print_*` helper per output, and the arm
left with the request and the `json` fork.

**`cli/src/cli/admin.rs::handle_grants`** (23). Three helpers, one per
subcommand: `print_grants_table`, `print_grant_set` (whose doc comment now
carries the *what was stored, not what was asked for* note that sat inline) and
`print_grant_removed`. The `Source` column's ownership/granted-by choice is a
`let` before the row instead of an expression inside the array literal.

**`cli/src/cli/auth.rs::handle_token_command`** (22). Two: `print_tokens_table`
and `print_created_token`. Every line printed is the same line in the same
order — the CLI integration suite (`cli/tests/integration.rs`) drives the
binary and reads its output, and it passes unchanged.

## The five Containerfile smells

`docker:S6570` is the unquoted `$crate` in the `for` loop that stubs each crate;
it is quoted now. The loop's values are literals in the same line, so nothing
could ever have split, but the quoted form is what the rule reads and what a
future edit copies. `docker:S7018` is the apt package order on line 4 of
`Containerfile`; the `dnf` line in `Containerfile.hardened` was not flagged but
is sorted the same way so it does not become the next finding.
