# CodeQL triage — 2026-09-06

**Scope:** the three alerts Code scanning reports as *new* on `feat/idk`
(PR #146) — two `rust/path-injection` on `cli/src/contract.rs` and one
`rust/uncontrolled-allocation-size` on `crates/web/src/middleware/proxy_trust.rs`.
**Verdict:** all three are false positives. One is the alert already triaged on
2026-08-30, re-reported because the line moved; the other two are the same
CLI-argument-as-path shape as the `applier.rs` alerts in that note.
**Method:** manual read of each taint path from source to sink, and a check of
whether the sink is reachable from an HTTP request at all.

This note continues `codeql-triage-2026-08-30.md`; read that one first for why
these reappear on every pull request and why a dismissal has to happen on the
default branch to stick.

---

## Alert — `rust/uncontrolled-allocation-size`

**Location:** `crates/web/src/middleware/proxy_trust.rs:208`, in `trusted_origin`

This is alert 1 of the 2026-08-30 note at a new line number. The file grew a
doc comment above `trusted_origin`, so the `req.connection_info()` call moved
from 196 to 208 and Code scanning files the relocated alert as new. Nothing
about the code, the taint path or the bound changed.

The reasoning is unchanged and is not restated here: the "allocation" is actix
copying request headers into `String`s, bounded by `MAX_BUFFER_SIZE = 131_072`
in `actix-http`'s `src/h1/decoder.rs`. There is no attacker-controlled *size* in
the path, only attacker-controlled *content*.

**Resolution:** dismiss on `main` as *false positive*, same as its predecessor.

---

## Alerts — `rust/path-injection` ×2

**Location:** `cli/src/contract.rs:347` and `:351`, both in `ContractFile::try_load`

The two sinks are `path.exists()` and `std::fs::read_to_string(path)`. The
source CodeQL follows is `path`, and `path` reaches `try_load` from exactly
three places, all of them local to the machine running the CLI:

| Source | Where |
| --- | --- |
| `--contract <path>` | `ServeArgs::contract`, a clap `Option<PathBuf>` (`cli/src/cli/proxy.rs:40`) |
| `$VSX_REGISTRY_AUTH_TOKEN_FILE` | `contract::contract_path()` (`cli/src/contract.rs:95`) |
| the default | `~/.batlehub/state/vsx-token.json` |

**Why it is not a finding.** The value is supplied by the person running the
binary, to the binary they are running, and naming the file to read *is what the
flag is for*. There is no privilege boundary between the source and the sink:
whoever sets the flag can already read the file with `cat`. This is the same
shape as the three `applier.rs` alerts in the 2026-08-30 note, where the path is
the server's `--config` read once at boot — with one difference that makes this
case weaker still, not stronger: `batlehub-cli` is not a service, so there is no
request-borne route to reach it at all.

**Why not "fix" it.** The available sanitizers all make the tool worse. Rooting
the path under `~/.batlehub` breaks `--contract` for the case it exists to serve
— pointing the CLI and the editor patch at the same file somewhere else, which
is §4.1.3 of RFC 0011 — and canonicalising first changes nothing CodeQL models,
because a canonical attacker-chosen path is still attacker-chosen.

`rust/path-injection` must stay enabled repo-wide, for the reason
`.github/codeql/codeql-config.yaml` already records: several handlers build
storage keys out of request-supplied package coordinates, and this query is one
of the things watching them. Silencing it with a `query-filters` block to quiet
a CLI flag would trade a live control for a green check.

**Resolution:** dismiss both on `main` as *used in tests / false positive*, with
this note as the reasoning.

---

## What is deliberately *not* in this note

Two other alert classes are open on the branch and are **not** triaged here,
because they are neither new nor false:

- `rust/cleartext-logging` ×2 on `cli/src/cli/admin.rs` (1396, 1400) — `High`,
  standing, and not yet read. They belong in their own pass.
- The `postmortem.dependency-risk` and container-CVE alerts, which are the
  supply-chain scanners' output rather than CodeQL's and are handled by
  `deny.toml`, `.trivyignore.yaml` and the dependency workflows.
