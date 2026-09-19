# CodeQL triage — 2026-09-13

**Scope:** the one alert Code scanning reports as *new* on `feat/new-reg`
(PR #167) — `rust/uncontrolled-allocation-size` on
`crates/web/src/middleware/proxy_trust.rs:208`, in `trusted_origin`.
**Verdict:** false positive, and not a new one: it is the same alert at the same
line as `codeql-triage-2026-09-06.md`, which is itself the 2026-08-30 alert
after the line moved.
**Method:** `git log main..HEAD -- crates/web/src/middleware/proxy_trust.rs`
(empty) against this branch's commits, plus a re-read of the taint path.
Re-verified on 2026-09-13 after the branch grew to four commits.

---

## Alert — `rust/uncontrolled-allocation-size`

**Location:** `crates/web/src/middleware/proxy_trust.rs:208`, in `trusted_origin`

The file is not touched by this pull request. Its most recent commit is
`7bb7ce48`, which is on `main`; none of the four commits this branch adds —
`2531b49f`, `7ab6ce66`, `a9a7ecf3`, `4b0f4b4f` — goes near it, and
`git log main..HEAD -- crates/web/src/middleware/proxy_trust.rs` is empty. The
line number is the same 208 the 2026-09-06 note recorded, so nothing about the
code, the taint path or the bound has changed since that note was written.

The reasoning is unchanged and is not restated here: the "allocation" is actix
copying request headers into `String`s, bounded by `MAX_BUFFER_SIZE = 131_072`
in `actix-http`'s `src/h1/decoder.rs`. The path carries attacker-controlled
*content*, never an attacker-controlled *size*.

**Why it is reported again.** A dismissal filed against a pull-request analysis
does not carry to the default branch, so the alert returns on the next branch
that runs the query — which is the standing note in `codeql-triage-2026-08-30.md`
and the reason these entries repeat. Nothing on this branch caused it.

**Resolution:** dismiss on `main` as *false positive*, as its two predecessors
were. Until that dismissal exists on the default branch, this job will keep
reporting one new alert on every pull request, and the answer will keep being
this note.
