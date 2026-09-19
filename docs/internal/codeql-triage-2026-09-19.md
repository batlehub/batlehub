# CodeQL triage — 2026-09-19

**Scope:** the two alerts Code scanning reports as *new* on `feat/new-reg`
(PR #167) — `rust/path-injection`, High, on
`crates/web/src/services/reload/applier.rs:103` and `:178`.
**Verdict:** false positives — but **not for the reason the earlier notes give**.
The flow CodeQL actually reports does not pass through `--config` or
`BATLEHUB_CONFIG` at any step. This note supersedes the reasoning in
`codeql-triage-2026-08-30.md` §"Alerts 2–4" and reaches the same verdict by a
different argument.
**Method:** both alerts' own paths — nine steps for `:103`, seven for `:178` —
read step by step against the handlers, the `app_data` registration and
`Data<T>`'s extractor contract. Neither was inferred from the other.

---

## What the earlier notes got wrong

`codeql-triage-2026-08-30.md` says of these alerts:

> CodeQL's Rust taint model treats `std::env::var` and clap-parsed arguments as
> remote sources.

That is not the path. The reported flow for `:103` is:

| Step | Location | What |
| --- | --- | --- |
| 1 (source) | `handlers/back_office/config.rs:72` | `reload_config`, the handler fn |
| 2 | `config.rs:74` | `reload_svc: web::Data<Arc<ConfigReloadService>>` |
| 3 | `config.rs:86` | `reload_svc.reload_immediate(user_id)` |
| 4–5 | `services/reload/mod.rs:379-380` | `SelfParam [&ref]`, `self [&ref]` |
| 6 | `applier.rs:99` | `SelfParam [&ref]` into `load_pending` |
| 7–9 (sink) | `applier.rs:103` | `self.config_path` → `read_to_string` |

**The source CodeQL names is the `web::Data` extractor parameter, not argv and
not the environment.** Neither `std::env::var` nor clap appears anywhere in the
path. An argument built on them cannot be checked against this alert, which is
the thing a triage note exists to let a reader do — so the old reasoning is
retired here rather than repeated.

## Why it is still a false positive

CodeQL's actix model marks **every parameter of a request-handler function** as
a remote flow source. `web::Data<T>` is not request-borne: it is application
state registered once at startup, and `Data::<T>::from_request` clones the `Arc`
out of the request's app-data map without reading a single byte of the request.
Here it is registered at `server/src/server_factory.rs:356`:

```rust
.app_data(web::Data::new(Arc::clone(&reload_svc)))
```

from the service built once at `server/src/main.rs:919`, whose `config_path`
comes from `config_paths(&cli)`. So the value the query calls "user-provided" is
the service handle itself.

Steps 4–6 are the rest of the over-approximation: the taint rides an `&self`
receiver, which taints **every field** of `ConfigReloadService` — `config_path`
included — for any method reachable from a handler. Any field of any app-state
struct that reaches a filesystem call is reportable under this model; the
finding is a property of the source model, not of this field.

**Nothing request-borne is in the path.** `reload_config` takes exactly three
parameters: `identity` (`AuthIdentity`, the only genuinely request-derived one,
and it does not appear in the path), and two `web::Data`. It has no `Path`,
`Query` or `Json` extractor. It calls `reload_immediate(user_id)`, and
`user_id` — the one request-derived value it does pass down — reaches the audit
row, never the path. `config_path` itself is `pub(super)`, written once in
`ConfigReloadService::new`, with no other assignment in `crates/` or `server/`
outside tests.

**`:178` was read too, and is the same shape.** Its path is one hop shorter —
seven steps, with no `reload_immediate` in between:

| Step | Location | What |
| --- | --- | --- |
| 1 (source) | `config.rs:310` | `get_config_content`, the handler fn |
| 2 | `config.rs:312` | `reload_svc: web::Data<Arc<ConfigReloadService>>` |
| 3 | `config.rs:322` | `reload_svc.config_content()` |
| 4 | `applier.rs:177` | `SelfParam [&ref]` into `config_content` |
| 5–7 (sink) | `applier.rs:178` | `self.config_path` → `read_to_string` |

Same source (the `web::Data` parameter), same `&self` hop, same sink shape.
`get_config_content` takes no request parameter at all beyond `identity`, which
again does not appear in the path.

## What this does *not* license

The old note's conclusion that `rust/path-injection` must stay live is
unchanged, but its supporting claim needs correcting too: since `web::Data` is a
source under this model, an alert from this query on this codebase does **not**
by itself mean a request-borne path. Each one has to be read to the step that
names its source. The query stays because several handlers build storage keys
out of `Path`/`Query` coordinates and `ensure_safe_key` is the backstop for that
class — those alerts have real sources, and losing them would be the bad trade.
Do not add the query to `query-filters`.

No code change. There is no sanitiser to add: the tainted value is the service
handle, so nothing done to `config_path` clears it. `Path::canonicalize` was
considered and rejected in the 2026-08-30 note for separate reasons that still
hold.

## Why they surfaced now

The dismissals still do not exist on `main`, and a dismissal filed against a
pull-request analysis does not carry to the default branch — the standing cause
recorded in `codeql-triage-2026-09-13.md`. This branch does touch the file, which
the 09-13 alert's file did not: `995f24dc` wires the apk signer map through
reload, one field in `build_pending`'s `PendingReload` literal and one
`replace_from` in `apply`. Both are below line 217 — under the two alert sites,
whose line numbers this branch does not move — and neither goes near a path.

## Resolution

Dismiss both on `main` as *false positive*, with this note as the comment (not
the 2026-08-30 one, whose reasoning no longer matches the reported flow).

```bash
gh api repos/:owner/:repo/code-scanning/alerts \
  --jq '.[] | select(.state=="open") | select(.rule.id=="rust/path-injection") |
        "\(.number)\t\(.most_recent_instance.location.path):\(.most_recent_instance.location.start_line)"'

gh api -X PATCH repos/:owner/:repo/code-scanning/alerts/<NUMBER> \
  -f state=dismissed \
  -f dismissed_reason=false\ positive \
  -f dismissed_comment='See docs/internal/codeql-triage-2026-09-19.md'
```
