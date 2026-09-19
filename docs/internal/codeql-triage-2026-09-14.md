# CodeQL triage — 2026-09-14

**Scope:** the single `High` Rust alert failing the CodeQL check on
`feat/new-reg`: `rust/uncontrolled-allocation-size` at
`crates/web/src/middleware/proxy_trust.rs:208`, in `trusted_origin`, 25 paths.

**Verdict:** the 2026-08-30 triage called this alert a false positive and
dismissed it. **That verdict was wrong**, and this file supersedes it for this
alert. The query's *wording* is wrong — the allocation it points at is bounded —
but the thing it points at was a real, remotely triggerable amplification DoS.
It is now fixed in code.

**Method:** the taint path was re-read past the allocation the query names, to
what the tainted value is then *used for*. That step is what the previous triage
did not take.

---

## What the earlier triage got right, and where it stopped

`crates/web/src/middleware/proxy_trust.rs:208` is `req.connection_info()`. The
allocation is actix copying header bytes into the four `String`s of
`ConnectionInfo`. The 2026-08-30 note is correct that this is bounded:
`actix_http::h1::decoder::MAX_BUFFER_SIZE` caps a whole HTTP/1 head at 131_072
bytes, and `http2` is off (see the `actix-web` note in `Cargo.toml`), so there is
no HPACK path around it. A client can make that allocation at most 128 KiB.

It concluded: *"There is no attacker-controlled size in the path, only
attacker-controlled content."*

That is true of the allocation **and irrelevant to the outcome**, because the
128 KiB of content does not stay one copy. `trusted_origin` hands the host to
`registry_public_base`, which is by design the prefix of *every* self-referencing
URL in a generated document — and those documents carry one URL per version.
`LocalRegistryService::get_npm_packument` writes `{base}/{name}/{version}/tarball`
once per version into a `serde_json::Map`. Attacker-controlled content, repeated
a number of times the attacker also chooses (by picking a package), is an
attacker-controlled size.

## The measurement

20 versions, a 100 KiB `X-Forwarded-Host`, against the local npm registry:

```
status=200  request_host_bytes=102400  response_bytes=2052228  amplification=20.0x
```

The factor is the version count, so it is chosen by naming a package rather than
bounded by anything: real packuments run to thousands of versions. The document
is also fully materialised as a `serde_json::Value` before serialisation, so peak
heap is a multiple of the response again.

It needs no configuration to reach. `ProxyTrust::default()` is
`LegacyPermissive`, which honours `X-Forwarded-Host` from **any** peer — so this
is the behaviour of a deployment that has never set `[server].trusted_proxies`,
from an unauthenticated client, on any registry kind that generates per-version
URLs (npm, NuGet registration, PyPI simple, Composer, Terraform).

## The fix

`trusted_origin` now bounds both halves of the origin before returning them, so
every caller inherits the bound and no handler has to remember it:

- `MAX_HOST_LEN = 259` — 253 bytes of presentation-form domain name (RFC 1035
  §2.3.4) plus `:65535`. Over that, the server's own configured host is used
  instead, which is what `connection_host` and actix's `ConnectionInfo` already
  do for a request carrying no host at all.
- the scheme must be `http` or `https`, case preserved; anything else falls back
  to the scheme of the underlying connection.

The bound is applied to the *resolved* origin, not to the forwarded branch only:
`Host` is no less client-supplied than `X-Forwarded-Host`, and the untrusted
branch reads it.

This is the same amplification `MAX_FORWARDED_HOPS` already exists to stop on
`X-Forwarded-For` — the difference is that `X-Forwarded-For` is only held, while
the host is echoed back, which makes the host the worse of the two.

Regression tests: `an_over_long_forwarded_host_falls_back_to_the_configured_host`,
`an_over_long_plain_host_header_is_bounded_too`, `a_host_at_the_limit_is_kept_verbatim`,
`an_over_long_forwarded_proto_falls_back_to_the_connection_scheme`,
`a_forwarded_proto_keeps_its_case` and `the_forwarded_header_is_bounded_on_both_halves`
in `crates/web/src/middleware/proxy_trust.rs`, plus the end-to-end
`npm_packument_does_not_amplify_an_over_long_forwarded_host` in
`crates/web/tests/local_npm_registry.rs`, which is the one that fails on the old
code.

## What to do with the alert itself

The alert is likely to keep firing: the allocation CodeQL names is still inside
actix, upstream of the bound, so the taint path it draws is unchanged. Dismissing
it is still the right end state — but the dismissal comment must now say *"the
allocation is bounded at 128 KiB, and the downstream amplification that made that
bound insufficient is fixed by `MAX_HOST_LEN`"*, not the 2026-08-30 reasoning,
which would re-assert a claim that was shown to be wrong.

## The lesson, since this cost a month

`rust/uncontrolled-allocation-size` fires on the allocation, and the allocation
was genuinely innocent. Reading the query's own wording and stopping when it does
not hold is what produced a wrong dismissal. For any taint-based alert the
question is not "is the sink the query named actually dangerous" but "where does
this value go next" — here, three call hops later, into a loop.
