# SonarCloud triage — 2026-09-11

The analysis of `main` (commit `9b51b3ec`, "new test suit"): **83 new issues**,
and the quality gate red on two ratings — `new_reliability_rating` (actual 3,
threshold 1) from the 23 `BUG`s, and `new_security_rating` (actual 4, threshold
1) from the 7 `VULNERABILITY`s. No hotspots. Companion to
[`sonar-triage-2026-09-10.md`](./sonar-triage-2026-09-10.md);
[`sonar-triage-2026-09-06.md`](./sonar-triage-2026-09-06.md) holds the standing
register of what is ignored and why.

Eighty are fixed in code. Three are pinned in `sonar-project.properties`, each
an instance of a case already in the register (a protocol-mandated SHA-1, and a
tar header mode in a fixture builder).

| Rule | Count | Type | What changed |
| --- | --- | --- | --- |
| `rust:S7493` | 23 | bug | blocking `std::fs` calls in `async fn` → `tokio::fs`; the CLI's download sink becomes an `AsyncWrite`. |
| `shelldre:S7679` | 32 | code smell | positional parameters bound to locals at the top of fourteen functions, seven files. |
| `shelldre:S7682` | 18 | code smell | a trailing `return` on every function, `$?` where the status is the assertion. |
| `shelldre:S7677` | 2 | code smell | `validate.sh`'s failure line and its diff go to stderr; its success line is reworded. |
| `shell:S6505` | 2 | vulnerability | `--ignore-scripts` on the two `npm install`s (and on `hybrid.sh`'s, which was not flagged). |
| `shell:S4790` | 1 | vulnerability (critical) | **pinned** — `upstream_dir.sh` computes npm's `dist.shasum`, which is SHA-1 by definition. |
| `rust:S2612` | 2 | vulnerability | **pinned** — `set_mode(0o644)` in the two fuzz targets' tar fixture builders. |
| `githubactions:S8544` | 1 | vulnerability | `pip install bcrypt` in `test.yaml` pinned to `bcrypt==5.0.0`. |
| `rust:S1612` | 1 | code smell | `.map(\|rd\| rd.count())` → `.map(Iterator::count)` in `fuzz_scanner_extract.rs`. |
| `javascript:S7744` | 1 | code smell | `{ ...(opts.headers \|\| {}) }` → `{ ...opts.headers }` in the editor preload. |

---

## `rust:S7493` ×23 — blocking file I/O in `async fn`

"Use an asynchronous file API instead of this blocking file operation, or
move it to `spawn_blocking`." Twenty-two of the twenty-three are in the CLI,
one is in the server.

**The one that matters** is `crates/adapters/src/scanners/trivy.rs::scan`,
which wrote the SBOM for Trivy with `std::fs::write` on a worker-runtime
thread. The scan worker (RFC 0018) runs several jobs on one runtime, so a slow
disk there stalls every job on that thread, not only the one writing. It is
`tokio::fs::write(..).await` now; the three Trivy unit tests pass.

**The twenty-two in the CLI** are all in command handlers: the eleven
`publish_*` reads of the artifact in `api/publish.rs`, the Kubernetes token
file in `api/auth.rs::resolve_token`, the config file reads and the SBOM
export write in `cli/admin.rs`, the plan/bundle reads and writes in
`cli/mise.rs`, the VSIX and signature reads in `cli/vsx.rs`, and the output
file in `cli/download.rs`. None of these could starve anything — a CLI
invocation is the only task on its runtime — but `tokio::fs` costs nothing
there and the workspace's `tokio` already carries `full`, so every read and
write moved rather than being argued about.

Two are not a plain `std::fs` → `tokio::fs` swap:

- **`cli/download.rs`** — `BatleHubClient::download_to` took a
  `std::io::Write`, so opening the output file on the runtime would only have
  moved the blocking call one line down, into the chunk loop. The sink is a
  `tokio::io::AsyncWrite + Unpin` now: the loop `write_all(..).await`s each
  chunk and flushes at the end; the file is `tokio::fs::File` and stdout is
  `tokio::io::stdout()`. The CLI integration suite does not drive `download`,
  so it was checked by hand against a local HTTP server: a 3 MB random blob
  downloaded to a file and to `-o -` is byte-identical both ways.
- **`cli/mise.rs::run_export`** — the bundle is written by
  `batlehub_core::services::bundle::write_bundle`, the core crate's synchronous
  `std::io::Write` sink shared with the server's import path. The file is
  opened with `tokio::fs::File::create` and handed over with `into_std()`;
  the write itself stays synchronous, and the comment beside it says why.
  Making the writer async would mean an async `tar` — not for one finding.

`cargo test -p batlehub-cli` (389 tests, both suites) and clippy on both
crates pass.

## `shell:S4790` ×1 — pinned, `hashUpstreamDirShasum`

`tests/heavy/upstream_dir.sh` is the served npm upstream the `hybrid` and
`backends` suites install from, and it writes each packument's
`dist.shasum` as the SHA-1 of the tarball, because that is what the field is:
npm verifies the download against it and fails with `EINTEGRITY` on anything
else. Same register entry as `hashUpstreamAuditShasum` (the same field, read
back), pinned to the one file.

## `rust:S2612` ×2 — pinned, `modeFuzzSbomTar` / `modeFuzzScannerTar`

`set_mode(0o644)` on a `tar::Header` in the fixture builders of
`fuzz_sbom_extract.rs` and `fuzz_scanner_extract.rs`. This is the register's
`modeBundleTar` case again: a number written into an in-memory archive
header, never applied to a path, and here specifically the mode the
extractor under fuzz is expected to clamp. The scope note grows from three
pinned files to five; the rule stays on everywhere else.

## `shell:S6505` ×2 — `--ignore-scripts`

- `patches/che-code/validate.sh`: the scratch install of `typescript@5` and
  `@types/node@24` for the type-check. Neither declares a lifecycle script.
- `tests/heavy/backends.sh::npm_install`: the client-under-test installing
  the suite's own fixture (`heavy-backends-probe`). What the suite measures
  is the fetch through the proxy and the cache behind it; a script the
  fixture does not have is not part of that. `hybrid.sh::npm_project` was
  not flagged (its package list is `"$@"`) and takes the flag too, so the
  two suites state the same policy.

## `githubactions:S8544` ×1

The dex bootstrap step of `test.yaml` installed `bcrypt` unpinned, to hash
the dev password at run time rather than commit the hash. `bcrypt==5.0.0`
is the current release (2025-09-25). The workspace task (`.tasks/dex.yaml`)
does the same through `uv run --with bcrypt`, which is not a workflow file
and is not in this rule's scope.

## The shell smells — `S7679` ×32, `S7682` ×18, `S7677` ×2

The same two mechanical rules as on 2026-08-30 and 2026-09-06, on the files
the new suites brought: `hybrid.sh`, `backends.sh`, `upstream_dir.sh`,
`editor_patch_scenarios.sh`, `che_code_patch.sh`, `validate.sh`,
`external_tests.sh`. Every function binds `$1`/`$2` to a named local, and
every function ends in a `return`.

As on 08-30, the `return`s are not all `return 0`. `s3_holds_file`,
`mint_id_token`, `redis_cmd`, `origin_of`, `pip_origin_of`, `upstream_wheel`,
`che_code_version` and the two `*_after` counters return `$?`: their status
is what a caller hangs `||` off, or what `set -e` reads. The builders that
end in `|| heavy_fail` (`npm_publish`, `pypi_publish`, `upstream_npm_package`,
`upstream_pypi_dist`, `upstream_serve`) and the pure output helpers (`snap`,
`delta`, `upstream_requests`) return 0.

One rewrite is structural rather than a rename: `editor()` in
`editor_patch_scenarios.sh` consumed its `VAR=value` prefix with a `while`
over a bare `$1`. It now shifts each argument into a `local arg` first;
`A=1 B=2 -- --list-extensions`, `-- --version`, `C=3` alone and no
arguments at all all reach `editor_cli` with the same tail as before.

`S7677` is `validate.sh`'s verdict on the type-check. The failure line and
the diff that follows it go to stderr now, before the `exit 1`. The success
line — "*N* pre-existing error(s), none added" — is not an error message and
stays on stdout with the rest of the report; it says "diagnostic(s)" now,
which is what `tsc` calls them, so the rule stops reading the word as a
failure.

Checked: `bash -n` on the seven files; `upstream_dir.sh`'s four builders and
its counter run in a stub harness (a packument with the right `shasum`, a
wheel `zipfile` accepts, two requests counted, zero for an unknown name);
`snap`/`delta` and the `editor()` argument loop the same way. The heavy
suites themselves need the sidecar database and were not rerun here.
