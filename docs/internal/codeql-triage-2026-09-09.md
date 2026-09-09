# CodeQL triage — 2026-09-09

**Scope:** the two rules Code scanning reports as *new* on `feat/idk` —
`rust/uncontrolled-allocation-size`, three paths onto one sink in
`ConfigReloadService::load_layers`, and `rust/hardcoded-cryptographic-value`,
three literals in two integration-test files.
**Verdict:** neither is a finding, and neither attached changeset should be
applied. The allocation alert is a false positive; the three literals are test
fixtures and are dismissed as such.
**Method:** for the allocation, a manual read of the taint path from source to
sink plus a check of whether the sink's *size* input is reachable from an HTTP
request at all. For the literals, a read of every use of each constant and a
check of whether any of them reaches a shipped binary.

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

## Alert — `rust/hardcoded-cryptographic-value` ×3

**Locations:**

| File | Line | Value |
| --- | --- | --- |
| `crates/web/tests/flags.rs` | 29 | `const SECRET: &str = "a-key-from-the-vault"` |
| `crates/web/tests/flags.rs` | 146 | the inline `"wrong-key"` argument |
| `crates/web/tests/flag_revoke_canonical.rs` | 46 | `const SECRET: &str = "gate-secret"` |

All three are new because both files are new on the branch, and all three are in
`crates/web/tests/` — the in-process integration suite. `cargo build --locked
--workspace`, the command the CodeQL job builds with, does not compile that
directory at all; nothing there is linked into `batlehub-server`,
`batlehub-cli`, or any image.

**Why they are not findings.** In each case the literal *is the subject of the
test*, not a credential the code relies on.

`flags.rs` configures a `FlagSourceConfig` whose `secret` is `SECRET` (line 111)
and then signs its request bodies with the same value, so the suite can assert
that the push endpoint accepts a correct HMAC. The literal on line 146 is the
opposite: `"wrong-key"` is handed to `push_req` precisely so the signature comes
out wrong, and the assertion on the next line is a `404`. A key whose only
contract is to be rejected is not a key.

`flag_revoke_canonical.rs` passes `SECRET` to the heavy harness's
`heavy_flag_sign_revoke` and to this crate's `verify_inbound_hmac`, giving both
implementations the same value so that the assertion is about the *canonical
string* they sign, not about the key. Any value would do; the constant exists so
that a failure names a stable sample, for the same reason the file's own comment
gives for `SOURCE` and `EXTERNAL_ID`.

Neither value appears in `config.example.toml`, the Helm chart, or any manifest.
The real credential is `FlagSourceConfig::secret`, read from the operator's TOML,
and it has no default: a source without a secret is a config error, not a source
with `"a-key-from-the-vault"`.

**Why not a code change.** Two shapes were considered and both are worse than
the literal. Reading the key from an environment variable makes the suite depend
on ambient state and gives CI a value whose only purpose is to not be a literal.
Generating one per run would work — both sides receive the same value — but it
throws away the property the constants are chosen for, and a failure that prints
a different key on every run is harder to reproduce. Neither removes a risk;
both relocate a literal until the query stops matching it, which is the trade
this note already rejects for `with_capacity` above.

**Why not a `paths-ignore` entry for the test tree.** `paths-ignore` is
per-analysis, not per-query: excluding `crates/web/tests/**` would silence every
present and future query over ninety files, to quiet three literals. The `perf/`
exclusion is defensible because nothing under it is built or deployed; this
directory is what gates every shipped handler.

**Resolution:** dismiss on `main` as **`used in tests`**, not as *false
positive*. The query matched what it advertises — these are hard-coded values
used as keys — and the reason it is not a finding is the one GitHub already has
a name for.

**What to expect next.** Two more literals of the same shape are already in the
tree and were not annotated here, because the bot only comments on lines a pull
request touches: `crates/web/tests/notifications.rs:123` (`let secret =
"s3cret"`, into the same `Hmac::new_from_slice`) and
`crates/web/tests/terraform.rs:942` (`SIGNING_SECRET`, into
`SignedUrlService::new`). Whether Code scanning already holds open alerts on
them was not checked — there is no `gh` in the environment this note was written
in. If a later pull request touches those lines and they surface, they get this
verdict and this dismissal reason. Do not widen the config instead.

---

## Applying the dismissal

Same mechanics as the 2026-08-30 note — per-alert, on `main`, keyed on the alert
fingerprint so it survives the line moving. The two rules take **different**
dismissal reasons, so list and patch them separately.

```bash
# The allocation alert — three paths, one sink.
gh api repos/:owner/:repo/code-scanning/alerts --paginate \
  --jq '.[] | select(.state=="open") |
        select(.rule.id=="rust/uncontrolled-allocation-size") |
        "\(.number)\t\(.most_recent_instance.location.path):\(.most_recent_instance.location.start_line)"'

gh api -X PATCH repos/:owner/:repo/code-scanning/alerts/<NUMBER> \
  -f state=dismissed \
  -f dismissed_reason='false positive' \
  -f dismissed_comment='See docs/internal/codeql-triage-2026-09-09.md'

# The three test literals.
gh api repos/:owner/:repo/code-scanning/alerts --paginate \
  --jq '.[] | select(.state=="open") |
        select(.rule.id=="rust/hardcoded-cryptographic-value") |
        "\(.number)\t\(.most_recent_instance.location.path):\(.most_recent_instance.location.start_line)"'

gh api -X PATCH repos/:owner/:repo/code-scanning/alerts/<NUMBER> \
  -f state=dismissed \
  -f dismissed_reason='used in tests' \
  -f dismissed_comment='See docs/internal/codeql-triage-2026-09-09.md'
```

`dismissed_reason` is an enum: `false positive`, `won't fix`, `used in tests`.
Anything else is a `422`, and the alert stays open.
