# CodeQL triage — 2026-11-05

**Scope:** the four alerts Code scanning reports as *new* on
`feat/rfc-0040-phase-2-foundations` (PR #280), from RFC 0040 phases 6 and 7:

| Alert | Rule | Sink |
| --- | --- | --- |
| #2072 | `rust/hard-coded-cryptographic-value` | `cli/src/api/journeys.rs:297` |
| #2073 | `rust/hard-coded-cryptographic-value` | `cli/tests/journeys.rs:295` |
| #2074 | `rust/hard-coded-cryptographic-value` | `cli/tests/journeys.rs:370` |
| #2075 | `rust/uncontrolled-allocation-size` | `crates/web/src/handlers/front_office/packages.rs:470` |

**Verdict:** false positives, all four.
**Method:** each alert's code flow, read from the SARIF of analysis
`2099230799` (`analyze-rust`, the merge commit of `4bbf9aee`), step by step
against the tree (`Accept: application/sarif+json` on
`GET /repos/{owner}/{repo}/code-scanning/analyses/{id}`).

---

## #2072, #2073, #2074 — HMAC keys in tests

| Alert | Source | Sink |
| --- | --- | --- |
| #2072 | `"key"`, `cli/src/api/journeys.rs:297` | `hub_signature`'s `new_from_slice(secret.as_bytes())`, `journeys.rs:73` |
| #2073 | `"s3cret"`, `cli/tests/journeys.rs:295` | the test file's own `hub_signature`, `cli/tests/journeys.rs:278` |
| #2074 | `"s3cret"`, `cli/tests/journeys.rs:370` | the same |

- #2072's literal is inside `#[cfg(test)] mod tests` (`journeys.rs:289`):
  `hub_signature_matches_the_servers_known_vector` checks the CLI's
  `X-Hub-Signature-256` against the known vector
  (`"key"`, "The quick brown fox…") that `notification/channels.rs` holds its
  own HMAC to. A known-answer test needs a fixed key.
- #2073 and #2074 are in `cli/tests/journeys.rs`, an integration-test file: the
  mock server recomputes the signature with the same fixed secret the test
  passes to `flag create` / `flag delete`, to prove the CLI signs the exact
  bytes it sends (POST body) and the canonical `DELETE\n<path>`.

In production the secret comes from `--secret`/`BATLEHUB_FLAG_SECRET`, never
from a literal; no non-test caller of `hub_signature` passes a constant.

## #2075 — `Vec::with_capacity(items.len())`, after the 50-item bound

| Step | Location | What |
| --- | --- | --- |
| 1 (source) | `packages.rs:445–447` | `check_access_batch`, `body: web::Json<AccessBatchRequest>` |
| 2–3 | `packages.rs:457` | `body.into_inner().items` |
| 4 (sink) | `packages.rs:470` | `Vec::with_capacity(items.len())` |

Between steps 3 and 4, `packages.rs:458` returns `400` when
`items.len() > MAX_ACCESS_BATCH` (50); the query does not model the early
return. And the size is the length of a `Vec` that already exists — the
request body was deserialized under actix's JSON payload limit before the
handler ran — so the capacity can never exceed an allocation already made.

## What this does *not* license

- Both rules stay live. A literal key reaching a production signer, or a
  request-borne length reaching an allocation without a bound, would be real.
- Do not add either to `query-filters` in `.github/codeql/codeql-config.yaml`.

No code change: clamping a value already bounded, or moving test keys out of
literals, would quiet the query without removing anything.

## Resolution

Dismiss all four — #2072–#2074 as *used in tests*, #2075 as *false positive* —
with this note as the comment. A PR dismissal carried to `main` last time
(RFC 0038, #2016–#2018); check their state on `main` after the merge before
re-dismissing.
