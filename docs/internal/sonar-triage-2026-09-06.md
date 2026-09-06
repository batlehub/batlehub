# SonarCloud triage — 2026-09-06

Companion to [`codeql-triage-2026-09-06.md`](./codeql-triage-2026-09-06.md), for
the SonarCloud analysis of PR #146: **107 open issues**, of which 11 are typed
`VULNERABILITY` and are the reason the quality gate failed on its one condition,
**security rating on new code: D, required ≤ A**. The other 96 are code smells
and do not gate.

All 107 were read. The disposition:

| | Count | Where it goes |
| --- | --- | --- |
| Fixed in code | 73 | this branch |
| Won't fix, resolved in SonarCloud | 34 | the four sections below |

---

## Fixed in code — 73

| Rule | Count | What changed |
| --- | --- | --- |
| `javascript:S4036` | 1 | `tests/heavy/vsx_view.mjs`: `execFileSync("/bin/bash", …)`, an absolute interpreter. The hook runs inside a container whose PATH the harness does not own. |
| `rust:S1612` | 8 | closures replaced by method references — `ReasonCode::as_str`, `ReasonCode::is_time_bound`, `ToString::to_string`, `PoisonError::into_inner` |
| `rust:S9045` | 3 | `#[cfg(test)] mod tests` moved to the end of `entities/air_gap.rs`, `handlers/air_gap.rs`, `handlers/proxy/openvsx.rs` — each had grown items below it |
| `rust:S8863` | 1 | `entities/notification.rs`: redundant `'static` on a `const` dropped |
| `javascript:S6557` | 6 | `tests/heavy/vsx_view.mjs`: `/^X/.test(s)` → `s.startsWith("X")` |
| `javascript:S7765` | 1 | same file: `.some((a) => a === "Install")` → `.includes("Install")` |
| `typescript:S3358` | 1 | `AdminUpstream.vue`: the nested ternary hoisted to a `noTransition` binding |
| `typescript:S3863` | 2 | `AdminHealth.vue`: two `from "vue"` imports merged |
| `typescript:S6582` | 1 | `vsxRegistryAuth.ts`: `inline && inline.trim() ? … : undefined` → `inline?.trim() \|\| undefined` |
| `shelldre:S7679` | 6 | `lib.sh`, `quarantine.sh`, `upstream_audit.sh`: positional parameters bound to locals at the top of the function |
| `shelldre:S1192` | 12 | repeated literals named — see below |

The `S1192` batch is the one worth a sentence, because the repository already
argues for it in `tests/heavy/authz.sh`'s header: a load-bearing literal spelled
one way in twenty places and another way in one is an assertion that silently
stops matching. What got a name:

- `CLOSED_PROXY` / `LOOPBACK_DIRECT` (`airgap.sh`, `mise.sh`) — the four proxy
  environment variables must all name the *same* closed port; a typo in one of
  them leaves that scheme reaching the real internet and the phase still passes.
- `CURL_CODE` (`mise.sh`, `upstream_audit.sh`) — `%{http_code}` is the whole of
  every status assertion in both suites.
- `HDR_JSON` (`vsx_login.sh`) — as `authz.sh` already names it.
- `MARK_*` (`airgap.sh`, `sdkman.sh`) — each phase mark is spelled in a
  `heavy_mark`, an output filename and every `heavy_wire_*_after` assertion about
  that phase; the three have to agree.

---

## Not a finding — `S4790`, weak hash algorithm ×6

| Location | Algorithm | Why it is there |
| --- | --- | --- |
| `crates/core/src/services/listing_synthesis.rs:1959` | MD5 | the field a composed listing must carry |
| `crates/web/src/handlers/proxy/maven/proxy.rs:307` | MD5 | `.md5` sidecar, Maven repository layout |
| `crates/web/src/handlers/proxy/maven/proxy.rs:310` | SHA-1 | `.sha1` sidecar, Maven repository layout |
| `crates/web/tests/air_gap.rs:1613` | SHA-1 | the assertion that the above is emitted |
| `crates/web/tests/air_gap.rs:1697` | MD5 | the assertion that the above is emitted |
| `tests/heavy/upstream_audit.sh:75` | SHA-1 | `dist.shasum`, npm registry metadata |

**These are wire formats, not security choices.** Maven clients fetch
`<artifact>.md5` and `<artifact>.sha1` beside every artifact and compare them;
npm's packument carries `dist.shasum` as SHA-1 and npm verifies against it. A
proxy that emitted SHA-256 in those fields would not be more secure, it would be
broken — every client would reject the response. There is no stronger algorithm
available at these call sites because the algorithm is not ours to pick.

Nothing here is an authentication, signing or integrity decision BatleHub makes:
where BatleHub does choose, it chooses SHA-256 (`artifact_storage_key`, the
bundle manifest, the VSIX signature manifest).

**Resolve in SonarCloud as "won't fix".** This is now the second batch of
protocol-mandated hashes to reach this note — the standing rule is: never
"fix" one of these in code, and never add a suppression comment at the call
site either, because the next reader would have to re-derive why.

---

## Not a finding — `S2612`, file permissions ×4

| Location | Mode | What it sets |
| --- | --- | --- |
| `crates/adapters/src/listing_facts.rs:220` | `0o644` | a `tar::Header` mode in a test fixture builder |
| `crates/adapters/src/scanners/extract.rs:332` | `0o644` | the extracted file, after the sandbox wrote it |
| `crates/adapters/src/scanners/extract.rs:347` | `0o755` | a `tar::Header` mode in a test fixture builder |
| `crates/core/src/services/bundle.rs:322` | `0o644` | the bundle's `tar::Header` mode |

`S2612` fires on any mode granting *others* a bit. None of these grants write to
anyone but the owner; the widest is `0o755`, and it is a mode written into a tar
header inside a `#[cfg(test)]` fixture builder, which never reaches a filesystem.

The one that touches a real file, `extract.rs:332`, is the opposite of the
weakness the rule describes: it runs **after** an artifact from an untrusted
archive has been written, and clamps the result to `0o644` precisely so an entry
claiming `0o777`, setuid, or an execute bit does not get one. Raising it to
`0o600` would not improve anything a reader of the scanner sandbox cares about,
and would obscure that the line is a clamp rather than a grant.

**Resolve in SonarCloud as "won't fix", per location.**

---

## Not a finding — `shelldre:S7682`, missing explicit `return` ×21

Twenty-one heavy-suite functions are asked to end with an explicit `return`.
**Applying this rule would break the suites**, and it is worth being precise
about how.

A bash function returns the status of its last command. Appending `return 0`
overrides that. The flagged set includes `run_cargo`, `run_go`, `run_mvn`,
`run_nvm`, `run_sdk` and `fetch` — every one of which is called for its status:

```
if run_go    "$HEAVY_WORK/cache2" "$HEAVY_WORK/c2" "$PROXY" mod download; then
if run_cargo "$HEAVY_WORK/home2"  "$HEAVY_WORK/c2" fetch --locked; then
if run_mvn   "$REPO2"             "$HEAVY_WORK/c2" dependency:resolve; then
```

Each of those `if`s is a negative control: the assertion is that the client
*fails* against a blocked or air-gapped registry. `return 0` at the end of the
runner makes every one of them succeed, and the suite goes green having proved
nothing — the failure mode these suites exist to catch, introduced by a
readability rule.

The rest of the set (`consumer`, `publishable`, `fresh_repo`, `make_sdkman_dir`,
`hits_for`, `count_of`, `deploy`) either builds a fixture or echoes a value; an
explicit `return 0` there is inert, and applying the rule to half the functions
and not the other half is worse than not applying it.

**Resolve in SonarCloud as "won't fix", all 21.**

---

## Not a finding — `typescript:S7772`, prefer `node:` specifiers ×3

`patches/che-code/vsxRegistryAuth.ts` imports `fs`, `os` and `path` unprefixed.
This file is not built here. Its README is explicit: it is copied into
`src/vs/platform/extensionManagement/common/` of a che-code checkout and compiled
by *that* build, against whatever `tsconfig` and module resolution the upstream
uses on the day. Rewriting the specifiers to satisfy a rule in this repository,
for a file compiled in another one, trades a working integration for a lint
score.

**Resolve in SonarCloud as "won't fix".**

---

## Fixed in code — `rust:S3776` / `javascript:S3776`, cognitive complexity ×31

Thirty Rust functions and one in `vsx_view.mjs` exceeded the threshold of 15.
All are split, by extraction only: no behaviour changed, and the full workspace
suite is the check that says so.

The pattern in every case was the same — a function that had grown a second and
a third job, each of which reads better with a name. What came out:

| File | Was | Extracted |
| --- | --- | --- |
| `core/services/verdict.rs` | `classify` 17, `evaluate` 17 | one function per reason-code family (`scanner_error_effect`, `install_hook_effect`, `provenance_effect`, `forge_ref_effect`), and a `Folded` struct with `fold` / `maturity_lifts` |
| `core/services/blocking/forge.rs` | `rewrite_one` 31 | `rewrite_archive_urls`, `rewrite_gitlab_sources`, `rewrite_gitlab_links`, `rewrite_github_asset` |
| `core/services/blocking/mod.rs` | `strip` 17 | one `strip_<kind>` per protocol with more than one document shape |
| `core/services/blocking/sdkman.rs` | `strip_vendor_table` 23 | a `VendorTable` walker with `rule` / `row` / `keep` |
| `core/services/flags.rs` | `apply` 17 | `deny_covered`, `deny_one` |
| `core/services/scan_worker.rs` | `run_job` 37, `run_once` 16 | `dated_metadata`, `stored_sbom`, `scan_input`, `run_scanner`, `enrich`, `record`; `report_liveness`, `run_and_close` |
| `core/services/upstream_audit/confirm.rs` | `apply` 24 | `tally`, `present`, `missing_versions` |
| `core/services/upstream_audit/mod.rs` | `act` 20, `apply_block_policy` 20 | `cached_publish_date`, `queue_rescans`; `block_confirmed`, `unblock_reappeared`, `unblock_one` |
| `core/services/vulnerability/mod.rs` | `scan_all` 16 | `scan_and_tally`, `record_scan_states` |
| `config/schema/mod.rs` | 40, 24, 19 | `validate_scanners`, `validate_raw_ceilings`, `validate_security_prerequisites`, `validate_signing_keys`, `validate_no_egress_per_registry`, `validate_audited_registries`, `validate_registry_on_confirmed` |
| `adapters/registry/github/client.rs` | `resolve_metadata` 21 | `ref_metadata`, `tag_of`, `release_list_metadata`, `selected_asset` |
| `web/handlers/air_gap.rs` | `import_bundle` 39 | `read_bundle_body` and an `ImportCtx` with `refuse` / `write_meta` / `write_ref` / `write_verdict` / `import_entry` |
| `web/handlers/proxy/common.rs` | `proxy_stream` 16 | `forge_ref_headers`, `finish_stream` |
| `web/handlers/proxy/maven/proxy.rs` | `maven_get` 16 | `authorize_local_read`, `composed_metadata_checksum` |
| `web/handlers/proxy/vsx/source.rs` | `search_entries` 18 | `local_entries` |
| `web/tests/openapi_contract.rs` | the status lint 32 | `constructed_statuses`, `disagreements`, `compare_one` |
| `server/builders.rs` | 20, 22 | three nested `fn`s hoisted to module scope (`resolve_urls`, `make_one`, `offline_if`) plus `default_upstreams`; `push_flags_rule`, `push_forge_rules` |
| `server/main.rs` | `main` 38 | `run_subcommand`, `parse_roles`, `install_metrics_recorder`, `readme_image_fetcher`, `warn_if_air_gapped_and_empty`, `spawn_stats_rollup_if_enabled`, `spawn_vuln_scan_if_enabled`, `spawn_coherence_sweep_if_enabled`, `start_scan_worker`, `warn_if_no_worker`, `build_upstream_audit` |
| `server/watcher.rs` | `spawn_upstream_audit` 21 | `record_sweep`, `log_transition` |
| `cli/api/suggest.rs` | `render_mise_toml` 19 | `push_mise_registry_rules`, `push_mise_catch_all`, `push_replacement`, `push_note` |
| `cli/cli/admin.rs` | `run` 40 | the six inline arms became `handle_*` functions, as the other arms already were |
| `cli/cli/mise.rs` | `run_export` 21, `run_seed` 38 | `export_one`, `print_export_report`; `seed_one`, `judge_seed_fetch`, `print_seed_report` |
| `cli/cli/security.rs` | `run_verdicts` 19 | `list_verdicts`, `print_verdict_row`, `list_pullers` |
| `tests/heavy/vsx_view.mjs` | `install` 25 | `clickInstall`, `answerTrustPrompt`, `toastMessages`, `settleInstall` |

Two of these are worth a note beyond the table:

- **`server/builders.rs`** was not really 20 and 22 of its own complexity: it
  held three nested `fn` items, and their bodies counted toward the function
  they sat inside. They capture nothing, so hoisting them to module scope is a
  move, not a rewrite — and it is what most of the reduction is.
- **`ScanWorker::run_scanner`** now returns early on the success arm rather than
  matching twice on the same `outcome`. That is the one place where the split
  changed the shape of the code rather than only its location, and it is why the
  histogram label reads `outcome.is_ok()` instead of a second `match`.

---

## How the gate clears

The ten `VULNERABILITY` issues above are the whole of the security rating on new
code. Marking them removes it: the rating goes to A and the condition passes.
The 21 shell smells never gated, and the 31 complexity findings are gone from
the code rather than from the report.
