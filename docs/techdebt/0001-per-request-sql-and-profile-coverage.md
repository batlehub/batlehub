# TD-0001 — The fixed cost of a request, and the paths no profile has seen

| | |
| --- | --- |
| **Status** | Open |
| **Found** | 2026-10-09, `perf:profile` on PR #195 ([run 37909702910](https://github.com/batlehub/batlehub/actions/runs/37909702910); numbers below from [run 37924927391](https://github.com/batlehub/batlehub/actions/runs/37924927391), the first with a correct wait report); re-measured 2026-10-10 at `e9ced0d` (§1, §5) |
| **Area** | request middleware, routing, filesystem storage, database, `perf/` |

The end goal is a request that pays only for its own work. On a warm cache a
metadata read takes 1.7–3 ms on the server and a cached download 4.5–10 ms.
Three costs make up most of that, and every path pays them: SQL that is not
the request's (§1), unexplained waiting on the download paths (§2), and
the router (§3). §4 is what keeps the answer honest: most paths have never
been profiled. §5 is the same costs seen from the throughput side: the server
breaks while using about one core of four.

## 1. Every request runs SQL before it does its job

### What was measured

Statements per request, from the profile's wait report (sqlx's own count, per
request span):

| Path | Server wall ms | Statements | SQL ms |
| --- | ---: | ---: | ---: |
| `composer_root` (a static document) | 0.96 | 1.0 | 0.30 |
| metadata reads (`pypi_simple`, `npm_packument`, `cargo_index`, …) | 1.7–3.1 | 3.0 | 0.8–0.9 |
| cached artifact downloads | 4.5–10.4 | 6–8 | 2.0–3.2 |
| `npm_warm_read` | 5.64 | 7.0 | 2.45 |
| `npm_publish` | 7.76 | 14.8 | 4.55 |

**SQL is about half of the server's wall time on every path.** One statement
in the floor is identified: `user_blocks.is_blocked`
(`crates/adapters/src/db/governance/user_block.rs`, `SELECT EXISTS(… user_blocks
…)`). It runs on every authenticated request through the user-block middleware,
and it is 2.0 % of all CPU sampled (`user_block.rs:85 → __send`). The second
statement that separates a metadata read from `composer_root` is not
identified yet. The first candidate is the token lookup: `UserTokenAuthProvider`
runs `find_by_hash` (`crates/adapters/src/db/auth/user_tokens.rs`) on every
Bearer request, uncached, before the user-block middleware runs.

### What would retire it

1. Name every statement in the floor. `tests/heavy/db_calls.sh` records
   statements per request from sqlx's log. Run it on `composer_root`,
   `pypi_simple` and one artifact download, and diff the three lists.
   Check `find_by_hash` against the list first.
2. For each statement that reads data almost no request changes (blocks,
   grants, registry settings), remove it or serve it from memory. Unlike
   `HotConfig`, these rows have no per-process source: every replica reloads
   its own config file, but a block lives only in the database. "Invalidate on
   write" therefore reaches only the replica that took the admin's write. The
   chart supports `replicaCount > 1` (S3 storage), and nothing in the server
   invalidates across processes. In order of preference:
   - **Fold the check into a statement the request already runs.** For token
     auth, `find_by_hash` can carry `AND NOT EXISTS (SELECT 1 FROM user_blocks
     …)`. That removes a statement and needs no cache, and it is consistent on
     every replica. OIDC identities do not go through that query, so they keep
     the separate check, or get the next option.
   - **Cache, invalidated by `LISTEN/NOTIFY`.** Correct across replicas, at
     the cost of one listener connection per replica and a reconnect path that
     must drop the cache while it is disconnected.
   - **Cache with a TTL plus local invalidation.** The cheapest to build, but
     on the other replicas a block lands up to one TTL late. A user block has
     to take effect at once, so this is acceptable only if the TD states the
     bound and the block's admin page says so.
3. The download paths' extra 3–5 statements are a separate question: access
   log, download counters, cache bookkeeping. Each is a write per read. Decide
   which may be batched or made asynchronous without losing what the audit
   trail promises (RFC 0036).

### Re-measured 2026-10-10

[Profile run 38086662094](https://github.com/batlehub/batlehub/actions/runs/38086662094),
at `e9ced0d`, gives the same statement counts with very different SQL times:

| Path | Statements | SQL ms, 2026-10-09 | SQL ms, 2026-10-10 |
| --- | ---: | ---: | ---: |
| metadata reads (`pypi_simple`, `npm_packument`, `cargo_index`, …) | 3.0 | 0.8–0.9 | 0.5–0.7 |
| cached artifact downloads | 6–8 | 2.0–3.2 | 2.2–18.4 |
| `npm_publish` | 14.8 | 4.55 | 34.24 |

The reads got no slower, at about 0.2 ms a statement. The download paths now
range from 2.2 ms (`apk_package`, `openvsx_vsix`) to 14–18 ms
(`devfile_devfile`, `github_asset`, `nix_narinfo`, `vscode_vspackage`) for the
same 6–8 statements. That is up to 2 ms a statement. The statements a download
runs on top of a metadata read are the writes (item 3 below), so this run
points at **the writes, not the read floor**, as the larger share of §1's
cost. One run cannot tell a regression from a noisy runner, since Postgres
shares the runner's disk with k6 and the server. Re-run before acting. If the
spread holds, item 3 comes before item 2.

`npm_publish` went from 7.76 to 42.37 ms wall, and 23.29 ms of it is the
`publish_policy` stage.

**Paid when** a metadata read on a warm cache runs at most one statement, and
the profile's SQL column shows it.

## 2. A cached download waits, and the wait is not attributed yet

### What was measured

On the download paths, the wait report's *other awaits* column (wall time that
is neither CPU, SQL nor upstream) is 2–6 ms per request, against 0.6–2 ms on
the metadata reads:

| Path | Wall ms | CPU | SQL | Other awaits |
| --- | ---: | ---: | ---: | ---: |
| `conda_package` | 10.42 | 3.03 | 2.90 | 6.37 |
| `nix_nar` | 10.35 | 2.70 | 3.17 | 6.41 |
| `github_asset` / `gitlab_asset` / `forgejo_asset` | 8.1–8.6 | 2.1–2.2 | 2.7–3.0 | 5.0–5.4 |
| `pypi_wheel`, `cargo_download`, `nuget_nupkg`, … | 7.0–7.7 | 2.0–2.3 | 2.0–2.4 | 4.5–5.0 |
| `node_tarball`, `apk_package`, `deb_pool_file`, … | 4.4–5.0 | 1.8–2.1 | 2.0–2.2 | 2.1–2.4 |

*Other awaits* is everything awaited that is neither SQL nor upstream, so
several causes share the column. Three suspects, none confirmed:

- **Filesystem round trips.** The filesystem backend reads through
  `tokio::fs`, `READ_CHUNK` (64 KiB, `crates/adapters/src/storage/mod.rs`) per
  read, and each read is a hop to the blocking pool. This is the weakest
  suspect. The soak serves 256 KiB artifacts (`SOAK_ARTIFACT_KB`), so that is
  about five hops of tens of µs each, not 5 ms. And every kind serves the same
  size, so a per-chunk cost would be flat across kinds, yet the column runs
  from 2.1 ms (`node_tarball`) to 6.4 ms (`conda_package`). The spread points
  at per-kind work.
- **Waiting for a pool connection.** The SQL column times the statements, not
  the acquire before them. A download runs 6–8 statements, so a contended pool
  shows up here, and scales with the statement count rather than the file size.
- **Socket backpressure** while the body drains to the client.

### What would retire it

1. Split the column. Wrap the storage read and the pool acquire in separate
   `batlehub_core::services::stage` spans, so the next run's "Where the steps
   go" table gives each its own number. Diff `conda_package` against
   `node_tarball`: whatever the two do differently is where the extra 4 ms is.
2. Fix what the split names, not before. If it is the pool, the cure is §1's
   (fewer statements per download), not a larger pool. If it is the reads,
   `read_chunked` is shared with the S3 backend, so a chunk-size change moves
   both. `actix-files` is not a better model: `NamedFile` also does one
   `web::block` per 64 KiB chunk (`actix-files` 0.7 `chunked.rs`).

**Paid when** a cached download's *other awaits* is under 1 ms, the same as a
metadata read's.

## 3. Routing tries the routes one after another

### What was measured

Actix's router is the largest BatleHub CPU cost: 11.7 % of all CPU sampled,
and 20–36 % of a cheap read's. It is inlined below `service.call(req)` in the
rate-limit middleware, so the profile names it as
`rate_limit/middleware.rs:128 → regex_automata`. The router tries each
resource's pattern against the path in registration order, and the app
registers about 370 operations, most of them flat under `/proxy/{registry}/…`.

The duplicate of this cost is already paid: until 2026-10-09 the root span ran
a second, pre-routing `match_pattern()` to label `http.route`, at 12.8 % of
CPU. It now reads the route after routing (`server/src/server_factory.rs`,
`on_request_end`).

### What would retire it

Measure the cheap move first. The router stops at the first match, so a
request pays for every resource registered before its own, and today all 373
are flat (`crates/web/src/lib.rs` has no `web::scope`). Registering the hottest
reads earlier may retire most of the cost without restructuring anything. That
holds only where order does not decide between two overlapping patterns, and
`authz_matrix.rs` does not check order.

If that falls short, group the routes so a request is only matched against its own group. A
`web::scope` per URL prefix that cannot overlap (`/api/v1/admin`,
`/api/v1/…`, `/proxy/{registry}`) is matched once by prefix, and only its own
resources are tried after that. Two things make this more than a mechanical
move:

- several registry kinds share path shapes under `/proxy/{registry}/` and are
  told apart by guards or by order. A scope must keep the order that decides
  which handler wins, and `crates/web/tests/authz_matrix.rs`'s two route
  inventories will catch a route that moved or vanished;
- a scope does not fall through. Once its prefix matches, a path that none of
  its resources match gets the scope's 404, never a sibling registered after
  it. So `/proxy/{registry}` takes every `/proxy/…` route in one move, never
  in pieces, and a route that was shadowed rather than removed still appears
  in the inventories. Test a request per kind, not only the route list;
- `match_pattern()` returns the full pattern under a scope, so `http.route`,
  the `batlehub_db_statements_per_request{route}` label and
  `perf/profile_routes.txt` keep their keys. Check it, don't assume it.

**Paid when** the router's share of CPU on `composer_root` is under 10 %.

## 4. 321 of 372 API operations have no profile arm

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
opens on, the admin pages, and every upload but npm's. §1–§3 were all found on
those 51. A fix to any of them is measured on the same 51, and the other 321
change without a number.

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

## 5. The server breaks at one core of four

### What was measured

[Breaking-point run 38086662147](https://github.com/batlehub/batlehub/actions/runs/38086662147),
at `e9ced0d`, on a 4-core runner with the soak mix. The knee doubled since
[run 36281269687](https://github.com/batlehub/batlehub/actions/runs/36281269687)
(2026-09-27, `8846ef2`, 400/s on every arm):

| Arm | Knee | Clean at | p95 at clean | Server CPU at knee | Pool free at knee | Postgres RSS at knee |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `fs-memory` | 1600/s | 800/s | 1221 ms | 107 % | 0/10 | 779 MiB |
| `fs-redis` | 800/s | 400/s | 12 ms | 114 % | 0/10 | 595 MiB |
| `s3-memory` | 800/s | 400/s | 11 ms | 112 % | 0/10 | 594 MiB |
| `s3-redis` | 800/s | 400/s | 10 ms | 110 % | 0/10 | 592 MiB |
| `s3-redis-pool50` | 800/s | 400/s | 19 ms | 113 % | 0/50 | 2638 MiB |

Three readings:

- **The pool is not the ceiling.** The report tells you to raise
  `max_connections` and re-run. The 50-connection arm is that re-run, and its
  knee did not move. It ran out of all 50 connections too, and its Postgres
  grew from 480 MiB to 2 GiB at 400/s. A larger pool costs memory and buys no
  throughput, which fits §1: each request holds a connection for 6–8
  statements.
- **The server does not use the machine.** Every arm breaks at 107–114 % of
  one core, on four. The server is waiting, not computing, and §1 and §2 are
  what it waits on. The Postgres process's CPU is not sampled, so this run
  cannot say whether the database used the other cores.
- **`fs-memory` does not fit the others.** It has the best knee (1600/s) but
  the worst latency below it: p98 299 ms at 400/s against 20–30 ms on the
  other arms, and p95 1221 ms at 800/s. That is either noise or contention in
  the in-memory cache, and one run cannot say which.

### What would retire it

1. Sample the Postgres processes' CPU per step in
   `perf/scripts/breaking_point*`, beside the server's. Until the database's
   share is known, "waiting" cannot be split into "waiting on a busy database"
   and "waiting on round trips".
2. Re-run `fs-memory`. If the latency holds, profile it at 400/s to find the
   contention.
3. Change the report's pool advice. When the pool-50 arm breaks at the same
   rate, the advice should say so rather than repeat "raise
   `max_connections`" five times.

**Paid when** the knee is limited by the server's CPU, meaning at least 300 %
on a 4-core runner, rather than by waiting.
