# CodeQL triage — 2026-08-30

**Scope:** the four `High` Rust alerts standing on `main` and re-reported on every
pull request, one `rust/uncontrolled-allocation-size` and three
`rust/path-injection`; plus the relocation of one of them on `feat/rework-role`
(see the addendum at the end).
**Verdict:** all four are false positives. **(Alert 1: withdrawn on
2026-09-14 — it was a real finding. See `codeql-triage-2026-09-14.md`.)** None is dismissible by editing the
code without making the code worse; all four are resolved in the scanner, with
the reasoning recorded here.
**Method:** manual read of each taint path from source to sink, plus a check of
whether the sink is reachable from an HTTP request at all.

---

## Why these keep coming back

The two files are byte-identical to `main` on the branch that reports them:

```
git diff --stat main...HEAD -- crates/web/src/middleware/proxy_trust.rs \
                               crates/web/src/services/reload/
(empty)
```

Code scanning reports every alert present on the analysed revision, not only the
ones a diff introduces, and the check fails on `High`. So a pre-existing alert on
`main` reappears as a red check on every unrelated PR until it is dismissed **on
the default branch**. Dismissing it on a PR does not carry; the alert lives on
`main`.

---

## Alert 1 — `rust/uncontrolled-allocation-size`

> **Superseded — this section's verdict is wrong.** See
> `codeql-triage-2026-09-14.md`. The allocation is bounded at 128 KiB as stated
> below, but the bounded value is echoed once per version into every generated
> document, which made it a 20x-and-up amplification DoS reachable from an
> unauthenticated request. Fixed by `MAX_HOST_LEN` in `trusted_origin`. The
> reasoning below is kept as written because the mistake in it is the useful
> part.

**Location:** `crates/web/src/middleware/proxy_trust.rs:196` (in `trusted_origin`)
**Paths reported:** 21

The flagged expression is `req.connection_info()`. The "allocation" is actix
copying request headers into `String`s while it parses `Forwarded`,
`X-Forwarded-Host`, `X-Forwarded-Proto`, `X-Forwarded-For` and `Host` — the 21
paths are those headers crossed with the fields `ConnectionInfo` exposes.

**Why it is not a finding.** The size is bounded by the HTTP/1 head limit,
`MAX_BUFFER_SIZE = 131_072` in `actix-http`'s `src/h1/decoder.rs`. A client
cannot make this allocation large; it can make it at most 128 KiB, which is the
same bound that applies to every request this server accepts. There is no
attacker-controlled *size* in the path, only attacker-controlled *content*.

This is the identical root cause already documented in
`.github/codeql/codeql-config.yaml` for the two alerts on
`perf/mock-upstream/src/main.rs` (82, 128), which are dropped by
`paths-ignore: perf/**`. That exclusion is a scope decision about a benchmark, not
a judgement about the query; this alert needs the judgement stated separately,
which is what this section is.

**Worth noting:** this call site is the one *guarded* use of `ConnectionInfo`'s
forwarded-header readers in the workspace. `clippy.toml` disallows them
everywhere else precisely so the trust decision cannot be bypassed, and this line
is reached only after `peer_trust(req).honours_forwarded_origin()` returns true.
CodeQL does not model that guard, so the query fires on the single call site that
was written to be safe. Suppressing the query repo-wide would therefore be
exactly backwards.

**Action:** dismiss as a false positive. Do not change the code — there is
nothing to change, and moving the call would defeat the `clippy.toml` control.

---

## Alerts 2–4 — `rust/path-injection`

**Locations:** `crates/web/src/services/reload/applier.rs:45`, `:120`, `:278`
(in `load_pending`, `config_content`, `apply`)

All three are `&self.config_path`. Its value is set once, at process start, in
`server/src/main.rs:101-104`:

```rust
let config_path = cli
    .config
    .or_else(|| std::env::var("BATLEHUB_CONFIG").ok())
    .unwrap_or_else(|| "config.toml".to_string());
```

CodeQL's Rust taint model treats `std::env::var` and clap-parsed arguments as
remote sources. Here they are operator input at boot, supplied by whoever starts
the process — the same person who decides the working directory, the user the
process runs as, and the contents of the file itself. A path-injection finding
requires an attacker who can choose the path but not already choose the process's
own arguments; that attacker does not exist for this field.

**There is no request-borne route to this value.** The config-editor endpoint
submits *content*, never a path: `load_pending_from_content` takes `&str` TOML and
parses it in memory. Line 278 is the write-back at the end of `apply`, and it
writes to the same operator-set path the process was started with. Nothing in
`crates/web` assigns `config_path`; it is `pub(super)` on the service struct and
populated once from `main.rs:394`.

**Action:** dismiss all three as false positives. Specifically **do not** add
`rust/path-injection` to `query-filters` in the CodeQL config: that query is one
we want live on this codebase, because storage keys are built from
request-supplied package coordinates in several handlers and `ensure_safe_key` is
the backstop for exactly this class. Losing it repo-wide to silence three boot-time
reads of a CLI flag would be a bad trade.

Canonicalising `config_path` into a `PathBuf` at startup was considered and
rejected: `Path::canonicalize` is not a recognised sanitiser for this query, so it
would most likely not clear the alert, and it would add a startup failure mode
(a config path that does not exist yet) to satisfy a scanner rather than a
requirement.

---

## Applying the dismissals

Per-alert dismissal in Code scanning, on `main`, with the reason recorded on the
alert. Dismissals are keyed on the alert fingerprint, so they survive the line
numbers shifting as the surrounding code changes.

```bash
# List the four, with their numbers.
gh api repos/:owner/:repo/code-scanning/alerts \
  --jq '.[] | select(.state=="open") |
        select(.rule.id=="rust/uncontrolled-allocation-size" or .rule.id=="rust/path-injection") |
        "\(.number)\t\(.rule.id)\t\(.most_recent_instance.location.path):\(.most_recent_instance.location.start_line)"'

# Dismiss one (repeat per number).
gh api -X PATCH repos/:owner/:repo/code-scanning/alerts/<NUMBER> \
  -f state=dismissed \
  -f dismissed_reason=false\ positive \
  -f dismissed_comment='See docs/internal/codeql-triage-2026-08-30.md'
```

`dismissed_reason` must be one of `false positive`, `won't fix`, `used in tests`.
Use `false positive` for all four.

---

## Stance

Consistent with the two standing rules this repo already follows:

- Dependency and SBOM scanning keeps `advisories.ignore = []` and an empty
  `.cargo/audit.toml` — **fix or patch, never ignore**. That stance is about
  advisories against third-party code, where "ignore" means accepting a known
  vulnerability.
- Static analysis findings that are wrong about *our* code are resolved **in the
  scanner, with the reason written down** — the same call already made for the
  protocol-mandated MD5/SHA-1 uses in SonarCloud, which are wire-format
  requirements that no amount of code change can remove.

The line between them is whether the tool is reporting a fact about the world
(a published CVE) or an inference about this code. The first is not ours to
dismiss. The second is, provided the reasoning is recorded where the next person
will find it — which is what this file is for.

---

## Addendum — alert 3 relocated by the atomic config write

**Reported as:** `rust/path-injection`, `High`, on
`crates/web/src/services/reload/applier.rs` in `persist_config_to_disk`
(the `tokio::fs::remove_file(&tmp)` cleanup, and the `write_and_sync` /
`set_permissions` / `rename` calls it stages).

**This is alert 3, moved — not a fifth finding.** On `main` the write-back at the
end of `apply` was a single `tokio::fs::write(&self.config_path, text)` on
line 278. `feat/rework-role` replaced it with a temp-file-plus-`rename` so a
process killed mid-save cannot leave a truncated `config.toml` behind. The sink
therefore moved out of `apply` and into the new helper, and CodeQL fingerprints
an alert by rule + location, so the dismissal recorded for line 278 does not
follow it. The alert has to be dismissed again at its new location.

**The taint path is unchanged.** `tmp` is built entirely from values this code
controls:

```rust
let target = std::path::Path::new(&self.config_path);
let dir  = target.parent().unwrap_or_else(|| std::path::Path::new(""));
let name = target.file_name().unwrap_or_else(|| std::ffi::OsStr::new("config.toml"));
let tmp  = dir.join(format!(".{}.{id}.tmp", name.to_string_lossy()));
```

`self.config_path` is the same boot-time operator input analysed in alerts 2–4
(`server/src/main.rs:101-104`, clap `--config` or `BATLEHUB_CONFIG`), and `id` is
a locally generated `Uuid::new_v4()` from the pending reload. The only
request-borne value that reaches this function is `text` — the TOML *content*
submitted to the config editor — and it is written, never joined into a path.

**On the flagged `remove_file` specifically.** It is not a deletion primitive.
It runs only on the failure path of the staged write, and its argument is the
temp name this function constructed three lines earlier — a dotfile beside the
operator's own config, suffixed with a UUID that exists nowhere else. An attacker
who could choose that path would already have had to choose the process's
arguments.

**Action:** dismiss as `false positive`, same command as above, with the comment
pointing at this file. Do not restructure `persist_config_to_disk` to satisfy the
query: dropping the temp file would reintroduce the truncated-config-on-eviction
window the helper exists to close, which is a real failure mode traded away for a
scanner inference that is wrong.
