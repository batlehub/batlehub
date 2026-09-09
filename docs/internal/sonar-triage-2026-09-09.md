# SonarCloud triage — 2026-09-09

The analysis of PR #146 (`feat/idk`, commit `16bcb6f0`): **18 open issues on new
code**, and unlike the last two rounds the security rating moved — **E**, on two
`BLOCKER` findings typed `VULNERABILITY`. Companion to
[`sonar-triage-2026-09-07.md`](./sonar-triage-2026-09-07.md);
[`sonar-triage-2026-09-06.md`](./sonar-triage-2026-09-06.md) holds the standing
register of what is ignored and why.

All 18 were read. **Every one is fixed in code — nothing was resolved as
won't-fix, and `sonar-project.properties` is unchanged by this round.**

| Rule | Count | Type | What changed |
| --- | --- | --- | --- |
| `secrets:S6698` | 2 | vulnerability | `crates/config/src/lib.rs`: the layered-load fixture's password. |
| `githubactions:S6506` | 1 | vulnerability | `.github/workflows/helm-lint.yaml`: the helm-docs download takes `--proto '=https' --proto-redir '=https'`. |
| `rust:S3776` | 2 | code smell | `pulls.rs::aggregate` was 19, `watcher.rs::run_watcher_thread` 22. |
| `shelldre:S7679` | 9 | code smell | the three flag-signing helpers in `tests/heavy/lib.sh` and the two wrappers in `quarantine.sh` bind their positional parameters. |
| `javascript:S7780` / `S6325` | 3 | code smell | `docs/build/check-i18n.mjs`: `String.raw` for the interpolated frontmatter matcher, a literal for the `sourceHash` rewrite. |
| `javascript:S8786` | 1 | code smell | `docs/build/rfc-meta.mjs`: the `HEADING` capture. |

---

## The security rating — `secrets:S6698` ×2

Both are the same string, in one test: `load_layered_merges_two_files_on_disk`
wrote `postgresql://real:s3cr3t@db/batlehub` into a temporary
`credentials.toml`, once as the layer's content and once as the assertion.

It is a fixture in a `tempdir` that no deployment reads, but the rule is right
to fire on it and the fix is free: `s3cr3t` is leetspeak, which is what a real
password looks like to an entropy check. Two tests earlier in the same file
already write `postgresql://real:secret@db/batlehub`, and that line — new code
in this same pull request, so scanned by this same analysis — is **not**
flagged. The literal word is what the detector reads as a placeholder.

The fixture now says `secret`, and the file has one idiom for a fake credential
instead of two. Nothing else about the test changed; it still asserts that the
later layer's `database.url` replaces the base's.

The other two vulnerabilities SonarCloud counts on this pull request are this
pair. `githubactions:S6506` is typed `VULNERABILITY` too but is `MAJOR`, so the
rating is the two blockers.

## `githubactions:S6506` — the helm-docs download

`curl -fsSL -o /tmp/helm-docs.tar.gz` against a GitHub release URL. A release
answers with a redirect to its object store, and neither hop may be talked out
of TLS. This is the idiom `test.yaml`, `secret-scan.yaml` and `Containerfile.worker`
already use for the same reason, and `helm-lint.yaml` was the last download
without it.

## `rust:S3776` ×2 — two functions, no behaviour change

**`crates/core/src/services/pulls.rs::aggregate`** (19). Two helpers came out of
it: `counted_coordinate`, the three skip conditions the loop opened with, and
`merge_event`, the `and_modify` closure that folds a further event into a row.
The *last seen* rule the closure implemented — only an event at or after
`last_pull` replaces the user agent and the address, and one that recorded
neither does not blank what an earlier one knew — is now an early `return` in a
function whose doc comment says it, rather than a nested `if` inside a closure
inside a loop.

**`server/src/watcher.rs::run_watcher_thread`** (22). Three: `watch_dir_of` (the
`parent()`-is-`Some("")` case for a bare relative path), `attach_directory_watches`
(the per-layer watch loop, returning `false` when not one directory could be
watched) and `forward_relevant_events` (the receive loop and its burst drain).
The long comment about *why the directory and not the file* moved onto
`attach_directory_watches` as a doc comment, which is where a reader now meets
it. What is left in `run_watcher_thread` is the wiring: build the watcher,
attach, log, filter, loop.

## `shelldre:S7679` ×9 — the flag-signing helpers

`heavy_flag_revoke_canonical`, `heavy_flag_sign` and `heavy_flag_sign_revoke`
moved into `tests/heavy/lib.sh` on this branch, and the two wrappers in
`quarantine.sh` that call them are new. Same fix as the six of 2026-09-06:
`local secret="$1" body="$2"` at the top, and the body reads names.

These are the two definitions `crates/web/tests/flag_revoke_canonical.rs` holds
against the server's `revoke_canonical` — it sources `lib.sh` and runs them, so
the gate covers the rewrite. It passes, and the signature over
`DELETE\n/api/v1/flags/soc/abc` is byte-identical to what it was.

## The three JavaScript rules

`check-i18n.mjs` built two regexes with the `RegExp` constructor. The frontmatter
reader interpolates the key, so it stays a constructor and takes `String.raw`;
the `sourceHash` rewrite interpolates nothing and is now a literal, which is
both rules at that line at once.

`rfc-meta.mjs`'s `HEADING` was `/^#{2,4}[ \t]+(.+)$/`. The `+` and the `.` can
match the same spaces, so a `##` followed by whitespace and nothing else made
the engine retry the split from every position. The capture now opens on a
non-space. The input is this repository's own RFCs, so the exposure was never
the point — `task rfc:index:check` and `task docs:i18n` both still pass, over
all 26 documents and all 65 translated pages.
