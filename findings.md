# Code review findings — `feat/devfile` (`main...HEAD`)

Reviewed 2026-09-27 with `/code-review --fix`. The scope was the branch's diff
against `main`, not the whole codebase.

## 1. Filesystem storage leaves empty staging directories (medium)

`crates/core/src/ports/storage/backend.rs:705`

Staging keys now include the destination key
(`staging:<uuid>/artifact:reg/name/ver`). The filesystem backend maps that key
to nested directories (`staging__<uuid>/artifact__reg/name/ver.dat`). When a
staged file is promoted (renamed away) or deleted, only the file is removed and
its directories stay. Every download staged for integrity verification leaves
one empty `staging__<uuid>/…` tree at the storage root, and these build up
without limit. Before this branch the key contained no `/`, so it mapped to a
single file. S3 is not affected.

**Fix:** `crates/adapters/src/storage/filesystem.rs` now removes the
`staging__<uuid>` directory after `move_key` and `delete` of a staging key.
Test: `a_staging_key_leaves_no_directory_behind`.

## 2. Devfile `latest` / `default` version keywords return 404 (low/medium)

`crates/web/src/handlers/proxy/devfile.rs:184`

The upstream devfile registry (`index/server/pkg/util/util.go`) accepts
`latest` (the highest version) and `default` (the default version) in place of
a version, for example in `GET /devfiles/{stack}/latest`. The proxy returned
404 for both, so a client that works against the real registry failed through
the proxy.

**Fix:** `resolve_version` now maps `latest` to the highest version in the
filtered index (via `best_latest`) and `default` to the stack's default
version. Both still pass the `deny_latest` check (`authorize_unpinned`) first.
Test: `the_latest_and_default_keywords_resolve_like_upstream` in
`crates/web/tests/devfile.rs`.

**Known gap:** in the test's mock index the highest version is also the default
version. The test therefore proves that both keywords now return 200, but not
that `latest` picks the highest version rather than the default.

## Verification

- 19 filesystem storage tests and 17 devfile web tests pass.
- `cargo clippy -- -D warnings` and `cargo fmt --check` are clean.

## Checked, no issues found

- Single-statement rewrites: block lookup, ownership, access logging, SBOM and
  vulnerability batching, cache touch.
- Storage router promotion of staged artifacts.
- Scan worker idle backoff and wake-on-enqueue.
- Per-layer log filters, DB statement metrics, and the tracing layer's position
  in the middleware stack.
- Devfile index filtering and OCI digest lookup.

## Noted, not changed

`lease` skips its low-priority slot whenever the first pick returns fewer rows
than requested. With `SKIP LOCKED`, that can happen because another worker holds
the rows, not only because the queue is empty. The only effect is that the slot
waits one extra pass.

---

# Breaking-point result on PR #195: what it means and how to improve it

Source: the `breaking-point-report` comment on
[#195](https://github.com/batlehub/batlehub/pull/195). The run was on
`195/merge` at `8846ef2`, whose parent is `37e2d15e`, so it includes the
single-statement DB rewrites.

## What the run shows

| arm | clean at | knee | served at knee | never placed | server CPU | pool free |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| fs-memory | 200/s | 400/s | 243/s | 28 % | 75 % | 0/10 |
| fs-redis | 200/s | 400/s | 243/s | 26 % | 60 % | 1/10 |
| s3-memory | 200/s | 400/s | 327/s | 11 % | 56 % | 0/10 |
| s3-redis | 200/s | 400/s | 241/s | 27 % | 61 % | 1/10 |
| s3-redis-pool50 | 200/s | 400/s | 247/s | 27 % | 60 % | 24/50 |

(CPU is a percentage of one core.)

## Why this result

1. **The real capacity is about 240–330 req/s, not "400".** The escalation
   doubles the rate at each step (100 → 200 → 400). The only thing we know is
   that the limit is somewhere between 200 and 400. The *served* column is the
   better number: four arms hit a hard limit near 243 req/s.

2. **The database pool is not the limit, and the report does not say so.**
   Four arms say "pool saturated, raise `max_connections` and re-run". The
   `s3-redis-pool50` arm is exactly that re-run. It had 24 of its 50
   connections free, yet it still broke at the same rate, served the same
   ~247/s, and dropped the same ~27 %. The pool fills up because requests are
   slow; it does not cap throughput. The only effect of the larger pool is
   Postgres memory: 1.9 GiB, against about 460 MiB for the other arms. Each
   report is written per arm, so none of them links its advice to the
   control arm's answer.

3. **The server itself is not CPU-bound either.** It used 0.56–0.75 of a core
   on a 4-core runner while p95 latency rose from about 50 ms to about 1 s.
   High latency with low CPU and free pool connections means requests are
   **waiting** on something. The candidates, most likely first:
   - **The runner.** k6, Postgres, the mock upstream, RustFS, Redis and the
     server all share 4 vCPUs. The harness samples CPU for the server only
     (for the backends it samples RSS only), so this run cannot tell whether
     the machine was saturated. The fact that every backend combination gives
     the same knee points strongly at something they all share, and this is the
     report's own conclusion.
   - **k6 itself.** "Never placed" means no free VU (virtual user, k6's unit of
     concurrency) was available. At 400/s k6 pre-allocates 200 VUs and grows up
     to 1600 while the step runs. Creating each new VU means parsing the
     32 KB `soak_arms.js` again, on a CPU that is already full. Once latency
     rises, k6 falls behind because of its own work, and it counts that as
     dropped iterations.
   - **Postgres.** Every arm uses it, including the `memory` cache arms (for
     access logging, blocks, ownership, packages and SBOMs). The pool-50 arm
     shows that more concurrent connections did not add throughput, which fits
     Postgres or the CPU under it being the shared limit. The workflow's own
     comment says the first run, which was before `37e2d15e`, also broke at
     400/s with the server at 58–67 % of a core. Cutting the number of
     statements per request did not move the knee, so the limit is probably not
     the number of statements.
   - **Something inside the server that makes requests wait on each other.**
     Examples are blocking I/O on actix workers, or a mutex on a hot path. This
     would produce the same pattern: under one core busy, latency climbing.
     It is less likely than the options above, but only a profile can rule it
     out.

4. **The Redis arms are slower before the knee.** At 200/s, `fs-redis` and
   `s3-redis` have a p95 of 138–150 ms, against 45 ms for `fs-memory`, and they
   already fall slightly behind the offered rate (192–194/s against 200). A
   network round trip per metadata lookup costs latency on a shared runner, even
   though it does not change the knee.

In short, this run measures the **harness** at about 240 req/s, not the server.
It is still useful for comparing backends run the same way (memory cache beats
Redis on latency; the pool size has no effect on throughput). It should not be
read as the server's capacity.

## How to improve it

In order of value per effort:

1. **Sample CPU for every process and for the whole machine.** The sampler in
   `perf/scripts/breaking_point.sh` (around line 141) already reads
   `/proc/<pid>` for the server and the RSS of the backends. Add CPU for
   `postgres`, `k6`, `mock-upstream`, `rustfs` and `redis`, plus the runner
   total from `/proc/stat`. If the runner is near 400 %, the knee belongs to the
   harness. This is the single measurement that answers the open question.
2. **Refine the knee between the last clean rate and the knee.** After the
   first failing step, search between 200 and 400 (for example 300, then 250 or
   350) instead of reporting a 2× bracket. It costs one to three extra
   60-second steps within the existing 1200 s budget.
3. **Link the advice across arms in the combined report.**
   `breaking_point_compare.py` (which writes the combined table) sees all arms.
   When a pool-N arm breaks at the same rate with free connections, it should
   print "pool ruled out" and the per-arm "raise `max_connections`" paragraph
   should be dropped. Without this, the report gives advice that its own
   control arm has already disproven.
4. **Report k6's own pressure.** Add `vus_max` and the peak `vus` per step. If
   k6 hit its VU limit, or was still creating VUs, then "never placed" measures
   k6, not the server. Also raise `preAllocatedVUs` to about `RATE`, so VUs are
   not created while the step is running.
5. **Show latency per request type at the knee.** k6 already tags each request
   with its type (`op`). A p95-by-`op` table for the breaking step shows whether
   one request type (a cache miss through the mock, a publish, the SBOM
   endpoint) is what holds the VUs.
6. **Move the load off the machine being measured.** To measure the server
   instead of the runner, give the server its own CPUs: run k6 and the mock
   upstream on a separate runner, or at least pin them with `taskset`/`--cpus`
   away from the server and Postgres. Until then, the numbers only compare
   backends against each other.
7. **If the runner turns out not to be saturated,** profile the server at
   300/s (`perf record` or `tokio-console`), looking for blocking calls on actix
   workers or a lock under contention. Use `pg_stat_statements` to rank the
   queries by total time.
