# CodeQL triage — 2026-10-25

**Scope:** the three alerts Code scanning reports as *new* on
`feat/rfc-0038-setup-apply` (PR #243), all High, all in the CLI's
`setup apply` path (RFC 0038):

| Alert | Rule | Sink |
| --- | --- | --- |
| #2016 | `rust/non-https-url` | `cli/src/api/mod.rs:130` |
| #2018 | `rust/cleartext-transmission` | `cli/src/api/mod.rs:130` |
| #2017 | `rust/cleartext-logging` | `cli/src/cli/setup.rs:543` |

**Verdict:** false positives, all three.
**Method:** each alert's own code flow, read from the SARIF of analysis
`2022586902` (`analyze-rust`, merge commit `a792ea66` of `3f0f5150`), step by
step against the tree. The check-run annotations name only the sink; the
source is in the SARIF `codeFlows`, fetched with
`Accept: application/sarif+json` on
`GET /repos/{owner}/{repo}/code-scanning/analyses/{id}`.

---

## #2018 and #2017 — the source is a client with a field named `token`

Both start at the same step:

| Step | Location | What |
| --- | --- | --- |
| 1 (source) | `cli/src/cli/setup.rs:191` | `BatleHubClient::for_trusted_document(server)` |
| 2 | `setup.rs:192` | `client.list_registries()` |

#2018 then goes `registry.rs:116` → `mod.rs:144` (`get`) → `mod.rs:130`
(`request`) → `mod.rs:122` (`url`, a `format!` of `base_url` and the path) →
the sink, the URL handed to `reqwest`.

#2017 goes back out of `fetch_registries` instead: `setup.rs:489`
(`load_plan`) → `setup.rs:326` (`plans`) → `run_status` → `print_row` →
`p.detail` → the sink, `println!("{:<18} └ {detail}", "")` at `setup.rs:543`.

**What the source actually holds.** `for_trusted_document`
(`cli/src/api/mod.rs:92`) builds the one client the CLI uses for a document it
fetches *anonymously*, the registry list `setup apply` writes client configs
from (RFC 0038 §4.4). It constructs `Self { inner, base_url, token: None }`:
the `token` field is the literal `None`, `auth_header()` therefore returns
`None`, and `send()` attaches no `Authorization` header. Its only other
property is the redirect policy (https, same host, at most five hops).

**What reaches the sinks.**

- #2018's sink is the request URL: `{server}/api/v1/registries`, the server
  URL the user configured. No credential is in it, and none rides with it.
- #2017's sink is a plan row's `detail`: built from the registry list the
  server returns to an anonymous caller (registry names, types, modes, public
  URLs, upstream hosts) and from local file paths. That list is public by
  construction: it is what any anonymous caller of the endpoint receives.

**Why CodeQL calls it sensitive.** The Rust queries' sensitive-data model is
name-based; `BatleHubClient` has a field named `token`, so a value of that
type is treated as sensitive, and everything derived from what it returns
inherits the taint. Nothing in the flow reads `token`.

## #2016 — the source is a test asserting the URL is refused

| Step | Location | What |
| --- | --- | --- |
| 1 (source) | `cli/src/cli/setup.rs:1095` | `"http://hub.example.com"`, inside `#[cfg(test)]` (module at `:898`) |
| 2–5 | `setup.rs:479`, `:486`, `:447–448` | `load_plan(Some(…))` → `require_server` |
| 6 | `setup.rs:489` | `fetch_registries(server, …)` |
| 7–8 | `setup.rs:191`, `mod.rs:92–93` | `for_trusted_document(server)` |
| 9–… (sink) | `mod.rs:144` → `:130` → `:122` | the request URL |

The test it starts in is the one that checks the URL is **refused**:

```rust
let err = load_plan(Some("http://hub.example.com"), …).await.unwrap_err();
assert!(err.to_string().contains("plain HTTP"), "{err}");
```

`load_plan` calls `check_server_scheme(server, args.insecure_http)` at
`setup.rs:487`, *between* steps 5 and 6, and it bails on a plain-HTTP server
that is not loopback unless `--insecure-http` was passed
(`client_setup.rs:1167–1184`). The flow CodeQL reports never reaches the sink
at run time; the query does not model the early return.

In production the same function is the gate: a plain-HTTP server reaches
`fetch_registries` only when it is loopback or the user passed
`--insecure-http`, the same rule the server itself applies to `public_url`.
That is a deliberate, opt-in path, documented in `docs/use/cli.md`.

## What this does *not* license

- `rust/cleartext-transmission` and `rust/cleartext-logging` stay live. The
  CLI's authenticated client (`BatleHubClient::new` with a token) is a real
  source, and a path that logged it would be a real finding. These two are
  dismissed because the source here is the anonymous client, not because the
  queries are noisy.
- `rust/non-https-url` stays live too: a plain-HTTP URL that reached a request
  without passing `check_server_scheme` would be a real finding.
- Do not add any of the three to `query-filters` in
  `.github/codeql/codeql-config.yaml`.

No code change. Restructuring `fetch_registries` to avoid `BatleHubClient`
would clear #2017 and #2018 by moving away from a field name, not by removing
anything sensitive, and #2016's source is a test either way.

## Resolution

Dismiss all three as *false positive*, with this note as the comment — on the
PR now, and again on `main` after the merge: a dismissal filed against a
pull-request analysis does not carry to the default branch
(`codeql-triage-2026-09-13.md`).

```bash
for n in 2016 2017 2018; do
  gh api -X PATCH repos/:owner/:repo/code-scanning/alerts/$n \
    -f state=dismissed \
    -f dismissed_reason=false\ positive \
    -f dismissed_comment='See docs/internal/codeql-triage-2026-10-25.md'
done
```

On `main` the alert numbers may differ; list them first:

```bash
gh api repos/:owner/:repo/code-scanning/alerts \
  --jq '.[] | select(.state=="open") |
        select(.rule.id|test("cleartext|non-https")) |
        "\(.number)\t\(.rule.id)\t\(.most_recent_instance.location.path):\(.most_recent_instance.location.start_line)"'
```
