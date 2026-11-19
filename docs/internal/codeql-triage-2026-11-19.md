# CodeQL triage — 2026-11-19

**Scope:**
- the four alerts Code scanning reports as *new* on `fix/global-review-2026-11-18` (PR #404);
- the fifteen alerts open on `main` that no triage note had covered, the oldest from 2026-08-27.

**Verdict:**
- #2001 is fixed in the code (`docs/build/page-description.mjs`).
- The other eighteen are false positives. Dismiss them on `main` after the merge: a dismissal made on a PR does not carry over.

**Method:** read each sink and its source at `6f61b70f` (main), and on the branch for the four new alerts.

---

## Hard-coded keys, all in tests

| Alert | Where | Why it is a test |
| --- | --- | --- |
| #2092, #2093, #2094 | `crates/web/tests/notifications.rs:157,167,176` | integration-test file; the HMAC key of a signed inbound webhook |
| #2095 | `cli/tests/journeys.rs:301` | integration-test file; the flags push signature |
| #1904 | `crates/web/tests/notifications.rs:123` | the same |
| #1905 | `crates/web/tests/terraform.rs:1087` | integration-test file |
| #1898, #1899 | `crates/core/src/services/signed_url.rs:461,462` | `SECRET` / `OTHER` consts inside `#[cfg(test)] mod tests` (line 457) |
| #1900 | `crates/web/src/services/notification/channels.rs:323` | `hmac_sha256_hex_matches_known_vector`, inside `#[cfg(test)]` (line 290); the key is RFC 4231's |

**Resolution:** dismiss as *used in tests*.

## #1848, #1849 — `rust/cleartext-logging`, `cli/src/cli/admin.rs:1610,1614`

The sink is `println!("Blocked user {user_id}")` (and "Unblocked"): the operator's own CLI echoing the id it was just given on the command line. A user id is not a secret.

**Resolution:** dismiss as a *false positive*.

## #1993 — `rust/request-forgery`, `crates/web/src/handlers/proxy/upstream.rs:165`

`cached_forward_filtered` sends `out.url`. Every `Outbound` is built from a host the request cannot choose:
- the configured upstream (`upstream_for(registry)`);
- the configured vuln-DB base;
- or a hard-coded default.

| Caller | URL built |
| --- | --- |
| composer advisories | `{upstream}/api/security-advisories/?{query}` |
| go vuln query | `{base}/v1/query` |
| go vuln documents, nuget vulnerabilities | `{base}/…` |
| mise python-precompiled | `{base}/data/python-precompiled-{triple}.gz`, where `triple` is validated to `[a-z0-9_]` |
| npm audit | `{upstream}{endpoint}`, where `endpoint` is a fixed literal |

What the request contributes is a path or a query after the authority, which cannot move the authority.

**Resolution:** dismiss as a *false positive*.

## #1994–#1998 — `rust/path-injection`, `crates/web/src/services/reload/applier.rs:470–492`

`persist_config_to_disk` writes `ConfigFile::path()` and a temporary file beside it, named `.{name}.{uuid}.tmp`. Both come from the `--config` argument, split once at startup. The "user-provided value" is the operator's command line, not a request. The editor's payload is the file's *content*, never its path.

**Resolution:** dismiss as a *false positive*.

## #1999 — `js/incomplete-multi-character-sanitization`, `cli/assets/java-engine/batlehub-java.mjs:423`

`clean2` strips `<!-- … -->` from a `pom.xml` before it is parsed with regular expressions for dependency coordinates. Nothing it returns is rendered as HTML, so a leftover `<!--` changes what is parsed, not what a browser runs. The file is a generated bundle of `packages/java-rules`.

**Resolution:** dismiss as a *false positive*.

## #2001 — `js/incomplete-multi-character-sanitization`, `docs/build/page-description.mjs:25`

`plain()` removed tags in one pass, so `<scr<b>ipt>` left `<script>`. The input is this repository's own pages, so nothing was exploitable. It is still fixed:
- `[<>]` is stripped after the tag pass;
- `page-description.test.mjs` pins the nested case;
- all 301 pages' descriptions are unchanged.

**Resolution:** fixed; the alert closes on the next analysis of `main`.

## What this does *not* license

- The test-key dismissals cover literals inside test code only. A literal reaching a `new_from_slice` outside `#[cfg(test)]` or `tests/` is a finding.
- #1993 and #1994–#1998 rest on the host and the path coming from configuration. A handler that ever takes either from the request reopens them.
