# CodeQL triage — 2026-09-09

**Scope:** the `rust/uncontrolled-allocation-size` alert Code scanning reports as
*new* on `feat/idk`, three paths onto one sink —
`crates/web/src/services/reload/applier.rs:57`, the
`Vec::with_capacity(self.config_overlays.len() + 1)` in
`ConfigReloadService::load_layers`.
**Verdict:** false positive, and the suggested changeset attached to it should
not be applied.
**Method:** manual read of the taint path from source to sink, plus a check of
whether the sink's *size* input is reachable from an HTTP request at all.

This note continues `codeql-triage-2026-08-30.md` and
`codeql-triage-2026-09-06.md`; read the first for why these reappear on every
pull request and why a dismissal has to happen on the default branch to stick.

---

## Alert — `rust/uncontrolled-allocation-size`

**Location:** `crates/web/src/services/reload/applier.rs:57`, in `load_layers`
**Paths reported:** 3

The alert is new because the code is new: `load_layers` arrived with the layered
`--config` work in `da3797d0`. The taint source is the same one the three
standing `rust/path-injection` alerts on this file already use — the repeatable
`--config` flag. `server/src/main.rs:532` splits the parsed `config_paths` into
the primary and `config_overlays`, and `ConfigReloadParams` carries that
`Vec<String>` into the service.

**Why it is not a finding.** The allocation is sized by
`config_overlays.len()`, the *count of `--config` flags the operator typed*, and
those strings are already resident: they were allocated by `Cli::parse()` before
`load_layers` ever runs. Reserving `n + 1` `String` slots is 24 bytes per
overlay against an argv that already cost more than that per path. An operator
who can make this allocation large has already made a larger one, in their own
process, on purpose.

Nothing request-borne reaches the size. `load_layers` *is* called from an HTTP
path — the config editor's validate and save both go through it, which is the
point of routing every text-to-`AppConfig` conversion here — but the request
supplies `primary`, the edited TOML text, and never `config_overlays`. The
overlay list is fixed at boot and immutable for the life of the process.

The `+ 1` cannot overflow: a `Vec<String>` cannot hold `usize::MAX` elements,
because each element is 24 bytes and the vector already exists.

**Why not apply the suggested changeset.** The suggestion adds a
`MAX_CONFIG_OVERLAYS = 1024` cap and a `checked_add(1)`. Both are dead code
against the argument above, and the cap is worse than dead: it converts a
legitimate operator configuration into a reload failure at a threshold picked to
satisfy a scanner rather than a requirement, in the one code path whose job is
to keep a running server's config loadable. The `checked_add` guards an overflow
that the type system already forbids.

Dropping `with_capacity` for a plain `Vec::new()` would most likely clear the
alert, since there would be no sized allocation for the query to point at. That
is not a fix either — it trades a correct capacity hint for scanner silence, and
leaves the next reader wondering why the one allocation whose length is known in
advance is the one that reallocates.

**Resolution:** dismiss on `main` as *false positive*, with this note as the
reasoning. No `query-filters` block:
`rust/uncontrolled-allocation-size` stays live for the request-facing
allocations it is actually there to watch, for the same reason
`.github/codeql/codeql-config.yaml` records about `rust/path-injection`.

---

## Applying the dismissal

Same mechanics as the 2026-08-30 note — per-alert, on `main`, keyed on the alert
fingerprint so it survives the line moving.

```bash
gh api repos/:owner/:repo/code-scanning/alerts \
  --jq '.[] | select(.state=="open") |
        select(.rule.id=="rust/uncontrolled-allocation-size") |
        "\(.number)\t\(.most_recent_instance.location.path):\(.most_recent_instance.location.start_line)"'

gh api -X PATCH repos/:owner/:repo/code-scanning/alerts/<NUMBER> \
  -f state=dismissed \
  -f dismissed_reason=false\ positive \
  -f dismissed_comment='See docs/internal/codeql-triage-2026-09-09.md'
```
