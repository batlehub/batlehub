# SonarCloud triage — 2026-09-06

Companion to [`codeql-triage-2026-09-06.md`](./codeql-triage-2026-09-06.md), for
the SonarCloud analysis of PR #146. The quality gate failed on one condition,
**security rating on new code: D, required ≤ A**, driven by 11 issues typed
`VULNERABILITY`. One is fixed in code. The other ten are false positives of two
kinds, both of which this codebase will keep producing, and both are resolved in
SonarCloud rather than in the code.

The 96 `CODE_SMELL` issues in the same batch do not affect the gate and are not
covered here.

---

## Fixed in code

| Location | Rule | Fix |
| --- | --- | --- |
| `tests/heavy/vsx_view.mjs:219` | `javascript:S4036` — PATH lookup | `execFileSync("/bin/bash", …)`, an absolute interpreter |

Real, if minor: the hook runs inside a container whose PATH the harness does not
own, so resolving `bash` through it is a lookup the harness cannot vouch for.

---

## Not a finding — `S4790`, weak hash algorithm ×6

| Location | Algorithm | Why it is there |
| --- | --- | --- |
| `crates/core/src/services/listing_synthesis.rs:1959` | MD5 | the field a composed listing must carry |
| `crates/web/src/handlers/proxy/maven/proxy.rs:307` | MD5 | `.md5` sidecar, Maven repository layout |
| `crates/web/src/handlers/proxy/maven/proxy.rs:310` | SHA-1 | `.sha1` sidecar, Maven repository layout |
| `crates/web/tests/air_gap.rs:1613` | SHA-1 | the assertion that the above is emitted |
| `crates/web/tests/air_gap.rs:1697` | MD5 | the assertion that the above is emitted |
| `tests/heavy/upstream_audit.sh:75` | SHA-1 | `dist.shasum`, npm registry metadata |

**These are wire formats, not security choices.** Maven clients fetch
`<artifact>.md5` and `<artifact>.sha1` beside every artifact and compare them;
npm's packument carries `dist.shasum` as SHA-1 and npm verifies against it. A
proxy that emitted SHA-256 in those fields would not be more secure, it would be
broken — every client would reject the response. There is no stronger algorithm
available at these call sites because the algorithm is not ours to pick.

Nothing here is an authentication, signing or integrity decision BatleHub makes:
where BatleHub does choose, it chooses SHA-256 (`artifact_storage_key`, the
bundle manifest, the VSIX signature manifest).

**Resolve in SonarCloud as "won't fix".** This is now the second batch of
protocol-mandated hashes to reach this note — the standing rule is: never
"fix" one of these in code, and never add a suppression comment at the call
site either, because the next reader would have to re-derive why.

---

## Not a finding — `S2612`, file permissions ×4

| Location | Mode | What it sets |
| --- | --- | --- |
| `crates/adapters/src/listing_facts.rs:220` | `0o644` | a `tar::Header` mode in a test fixture builder |
| `crates/adapters/src/scanners/extract.rs:332` | `0o644` | the extracted file, after the sandbox wrote it |
| `crates/adapters/src/scanners/extract.rs:347` | `0o755` | a `tar::Header` mode in a test fixture builder |
| `crates/core/src/services/bundle.rs:322` | `0o644` | the bundle's `tar::Header` mode |

`S2612` fires on any mode granting *others* a bit. None of these grants write to
anyone but the owner; the widest is `0o755`, and it is a mode written into a tar
header inside a `#[cfg(test)]` fixture builder, which never reaches a filesystem.

The one that touches a real file, `extract.rs:332`, is the opposite of the
weakness the rule describes: it runs **after** an artifact from an untrusted
archive has been written, and clamps the result to `0o644` precisely so an entry
claiming `0o777`, setuid, or an execute bit does not get one. Raising it to
`0o600` would not improve anything a reader of the scanner sandbox cares about,
and would obscure that the line is a clamp rather than a grant.

**Resolve in SonarCloud as "won't fix", per location.**

---

## How the gate clears

Marking the ten above removes every `VULNERABILITY` from new code, which is what
the security rating is computed from — the rating goes to A and the condition
passes. The 96 code smells do not gate.
