# TD-0001 — Fixed per-request SQL, and the paths no profile has seen

| | |
| --- | --- |
| **Status** | Open |
| **Found** | 2026-10-09, first `perf:profile` run ([run 37909702910](https://github.com/batlehub/batlehub/actions/runs/37909702910), PR #195) |
| **Area** | request middleware, `perf/` |

## 1. Every request runs SQL before it does its job

### What was measured

Statements per request, from the profile's wait report (sqlx's own count, per
request span):

| Path | Statements | SQL ms |
| --- | ---: | ---: |
| `composer_root` (a static document) | 1.0 | 0.19 |
| metadata reads (`pypi_simple`, `npm_packument`, `cargo_index`, …) | 3.0 | 0.45–0.60 |
| cached artifact downloads | 6–8 | 1.2–1.9 |
| `npm_warm_read` | 7.0 | 3.76 |
| `npm_publish` | 17.8 | 8.56 |

On the cheap paths SQL takes as much time as CPU, and cheap paths are most of
a package manager's traffic. One statement in the floor is identified:
`user_blocks.is_blocked`
(`crates/adapters/src/db/governance/user_block.rs`, `SELECT EXISTS(… user_blocks
…)`). It runs on every authenticated request through the user-block middleware,
and appears on its own in the CPU profile (`user_block.rs:85 → __send`, 0.5 %).
The second statement that separates a metadata read from `composer_root` is not
identified yet.

### What would retire it

1. Name every statement in the floor. `tests/heavy/db_calls.sh` records
   statements per request from sqlx's log. Run it on `composer_root`,
   `pypi_simple` and one artifact download, and diff the three lists.
2. For each statement that reads data almost no request changes (blocks,
   grants, registry settings): serve it from memory and invalidate on write.
   That is the shape `HotConfig` already has. A user block is an admin write
   and has to take effect at once, so its cache must be invalidated by the
   write, not by a TTL.
3. The download paths' extra 3–5 statements are a separate question: access
   log, download counters, cache bookkeeping. Each is a write per read. Decide
   which may be batched or made asynchronous without losing what the audit
   trail promises (RFC 0036).

**Paid when** a metadata read on a warm cache runs at most one statement, and
the profile's SQL column shows it.

## 2. 321 of 372 API operations have no profile arm

### What was measured

`perf/profile_routes.txt` lists every API operation as `profiled` or
`skip: <reason>`. All 321 skips give the same reason, `no arm yet`:

| Prefix | Unprofiled |
| --- | ---: |
| `/proxy/…` (registry protocol routes) | 174 |
| `/api/v1/admin/…` | 112 |
| `/api/v1/explore`, `auth`, `me`, `verdicts`, … | 35 |

The 51 profiled operations are the soak's arms, one per registry kind's hottest
reads plus `npm_publish` and one admin listing. A regression on any other path
has no "vs base" number. That includes search, the explore listings the console
opens on, the admin pages, and every upload but npm's.

### What would retire it

1. Add an arm to `perf/k6/soak_arms.js` for the paths a real client or the
   console calls on its own, in roughly this order: explore and search (the
   console's landing page), each kind's publish, `/api/v1/me` and the auth
   refresh, then the admin reads the console's pages make. An arm added there
   is soaked as well as profiled, and the route-inventory gate keeps
   `profile_routes.txt` honest.
2. Leave as `skip:` with a *real* reason the operations no client calls in a
   loop: one-off admin writes, imports, purges. `no arm yet` means "not
   triaged", and the goal is for no entry to say it.

**Paid when** no line of `perf/profile_routes.txt` reads `no arm yet`.
