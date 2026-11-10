# Technical debt

For someone changing the code who wants to know what is known to be owed.

An RFC records a decision. This records what is owed: something
measured or found that is worth fixing, with nothing decided yet about how. Each
entry says what was observed, where the evidence is, and what would retire it.
When the fix needs a design, it becomes an RFC and the entry links to it. When
it lands, the entry is marked **Paid** with the commit, not deleted. The next
person to see the same symptom should find that it was already seen.

| # | Debt | Found | Status |
| --- | --- | --- | --- |
| [TD-0001](/techdebt/0001-per-request-sql-and-profile-coverage) | Every request pays for SQL, unattributed waits and routing that are not its own work, 321 of 372 API operations have never been profiled, and the server breaks at one core of four | 2026-10-09 | Open (§1 Paid; §3 re-profiled at 10.5 %, bar < 10 %; §2, §4, §5 open) |
| [TD-0002](/techdebt/0002-feat-devfile-review-leftovers) | Six non-urgent findings from the `feat/devfile` review: a slow unsealed audit purge, per-version devfile digest lookups, two config checks, rejected-config attribution and duplicated leader election | 2026-10-11 | Paid |
| [TD-0003](/techdebt/0003-container-build-time) | The release container build takes about 3 h: arm64 under QEMU, a dependency cache every version bump invalidates, the server compiled three times, and the UI stage waiting on Rust | 2026-10-14 | Retired |
| [TD-0004](/techdebt/0004-request-routing-and-db-writes) | What a request still pays for after the read cache: a linear regex router (21–52 % of a late-registered request's CPU), one audit write per read, a 10-connection pool default and a 3-statement `/healthz` | 2026-11-09 | Paid |
| [TD-0005](/techdebt/0005-upstream-url-segment-encoding) | 22 registry clients interpolate the coordinate into their upstream URL unencoded; PR #306 refuses `?` and `#` in front of them, and each client should encode its segments itself | 2026-11-10 | Paid |
