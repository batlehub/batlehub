# TD-0001 — The fixed cost of a request, and the paths no profile has seen

| | |
| --- | --- |
| **Status** | Open |
| **Found** | 2026-10-09, `perf:profile` on PR #195 ([run 37909702910](https://github.com/batlehub/batlehub/actions/runs/37909702910); numbers below from [run 37924927391](https://github.com/batlehub/batlehub/actions/runs/37924927391), the first with a correct wait report) |
| **Area** | request middleware, routing, filesystem storage, `perf/` |

The end goal is a request that pays only for its own work. On a warm cache a
metadata read takes 1.7–3 ms on the server and a cached download 4.5–10 ms.
Three costs make up most of that, and every path pays them: SQL that is not
the request's (§1), unexplained waiting on the download paths (§2), and
the router (§3). §4 is what keeps the answer honest: most paths have never
been profiled.

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
