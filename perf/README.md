# BatleHub — Performance Testing Guide

This directory contains everything needed to measure throughput, latency, and resource usage of the BatleHub API under load.

## Table of contents

1. [Prerequisites](#prerequisites)
2. [Architecture of the test environment](#architecture-of-the-test-environment)
3. [Quick start — filesystem + memory (default)](#quick-start-—-filesystem-memory-default)
4. [Quick start — S3 + Redis](#quick-start-—-s3-redis)
5. [Comparing backends head-to-head](#comparing-backends-head-to-head)
6. [Scenarios](#scenarios) — including [10 — soak / leak detection](#_10-—-soak-leak-detection-perf-soak), [11 — startup and shutdown](#_11-—-startup-and-shutdown-perf-lifecycle) and [14 — breaking point, per backend](#_14-—-breaking-point-per-backend-perf-break)
7. [Tuning the mock upstream](#tuning-the-mock-upstream)
8. [The results table, and the report a release carries](#the-results-table)
9. [Reading the results](#reading-the-results)
9. [Known bottlenecks and what to watch](#known-bottlenecks-and-what-to-watch)
10. [Running against a remote server](#running-against-a-remote-server)

---

## Prerequisites

| Tool | Install |
|------|---------|
| k6 | `mise install k6` · [k6.io/docs/get-started/installation](https://k6.io/docs/get-started/installation/) |
| Podman + podman-compose | already used by existing `task compose:*` tasks |
| Rust toolchain | already present (used to build the mock upstream) |
| PostgreSQL | started via `task compose:db` |

---

## Architecture of the test environment

```
┌─────────────┐   HTTP   ┌────────────────────────┐   HTTP   ┌──────────────────────┐
│    k6       │ ───────► │  BatleHub server        │ ───────► │  mock-upstream       │
│  load gen   │          │  :8080                  │  :9999   │  (npm / cargo mock)  │
└─────────────┘          │  perf/config.perf.toml  │          └──────────────────────┘
                         └──────────┬──────────────┘
                                    │ SQL
                         ┌──────────▼──────────────┐
                         │  PostgreSQL :5432        │
                         └─────────────────────────┘

                    ┌──────────────────────────────────────┐
                    │  Prometheus :9090  ◄── scrapes /metrics every 5 s
                    │  Grafana    :3000  ◄── reads Prometheus
                    └──────────────────────────────────────┘
```

**Registries defined in `perf/config.perf.toml`:**

| Name | Mode | Purpose |
|------|------|---------|
| `perf-npm` | proxy → mock upstream | scenarios 02 (warm read), 03 (cache miss), 06 (SBOM), 07 (eviction) |
| `perf-local-npm` | local (no upstream) | scenarios 04 (upload) and 05 (mixed) |

Both registries have `[registries.sbom]` enabled (`formats = ["spdx", "cyclonedx"]`, `fetch_upstream = false`), so every cache miss (proxy) or publish (local) records an SBOM document — this is what scenario 06 reads back.

`perf-npm` also has `[registries.cache]` set with `artifact_ttl_secs = 3600` and `keep_latest_n = 3`, which enables `POST /api/v1/admin/registries/perf-npm/evict` (404s otherwise) — this is what scenario 07 exercises.

---

## Quick start — filesystem + memory (default)

Run these commands in separate terminals:

```bash
# Terminal 1 — database
task compose:db

# Terminal 2 — mock upstream registry (npm + cargo responses)
task perf:upstream

# Terminal 3 — BatleHub server using the perf config
task perf:server

# Terminal 4 — Prometheus + Grafana
task perf:infra:up
# Open http://localhost:3000  (admin / admin)
# → BatleHub folder → "BatleHub Performance" dashboard

# Terminal 5 — warm cache + verify connectivity
task perf:seed

# Run scenarios (terminal 5, sequentially)
task perf:run:rest     # 60 s baseline
task perf:run:read     # warm-cache ramp test
task perf:run:miss     # cache-miss / proxy-through
task perf:run:upload   # publish / upload
task perf:run:mixed    # 10-minute realistic mix
task perf:run:sbom     # SBOM read + org export
task perf:run:eviction # cache eviction sweep
```

To run the full suite in one shot (all scenarios run even when thresholds are crossed):

```bash
task perf:run:all
```

> **Exit code 99** — when you run a scenario directly (e.g. `task perf:run:read`), k6 exits with code 99 if any threshold is violated. This is intentional: it lets you use individual scenarios as CI latency gates. The `perf:run:all` task passes `--no-thresholds` to k6 so every scenario always runs to completion; threshold results are still printed in the summary, but a violation does not abort the suite.

---

## Quick start — S3 + Redis

Uses RustFS as the S3-compatible object store and Redis as the shared metadata cache. The k6 scenarios and the mock upstream are identical — only the server config changes.

```bash
# Terminal 1 — database (same as before; skip if already running)
task compose:db

# Terminal 2 — mock upstream (same as before; skip if already running)
task perf:upstream

# Terminal 3 — start RustFS (:9200) and Redis (:6380)
task perf:s3:infra:up
# S3 endpoint: http://localhost:9200  (rustfsadmin / rustfsadmin)

# Terminal 4 — BatleHub server with S3 + Redis config
task perf:s3:server

# Terminal 5 — warm cache and verify
task perf:seed

# Run all scenarios
task perf:s3:run:all
```

Individual scenarios follow the same naming convention as the default suite:

```bash
task perf:s3:run:rest
task perf:s3:run:read
task perf:s3:run:miss
task perf:s3:run:upload
task perf:s3:run:mixed
task perf:s3:run:sbom
task perf:s3:run:eviction
```

The bucket (`perf-artifacts`) is created by `task perf:s3:infra:up` itself, with `rc`, once the S3 port answers.

---

## Comparing backends head-to-head

Run both suites back-to-back without changing the k6 scripts or mock upstream. The server is the only variable — same DB, same registries, same load profile.

```bash
# 1. Run filesystem + memory suite
task perf:server        # terminal A
task perf:run:all       # terminal B — save terminal output to fs-results.txt

# 2. Stop the FS server, start the S3+Redis server
# (Ctrl-C in terminal A, then:)
task perf:s3:server     # terminal A
task perf:s3:run:all    # terminal B — save terminal output to s3-results.txt
```

**What to compare:**

| Metric | filesystem + memory | S3 + Redis | Expected winner |
|--------|--------------------|--------------------|-----------------|
| Warm-read P95 latency | — | — | filesystem (local disk < network S3) |
| Cache-miss P95 latency | — | — | similar (both bottlenecked by upstream) |
| Upload P95 latency | — | — | S3 (async multipart vs synchronous fsync) |
| RAM at peak load | — | — | S3+Redis (no in-process metadata map) |
| CPU at peak load | — | — | S3+Redis higher (TLS + ser/deser overhead) |

Fill in the blanks with your measured values. The Grafana dashboard (started with `task perf:infra:up`) stays up across both runs, so you can overlay the two time series.

---

## Scenarios

### 01 — At-rest baseline (`perf:run:rest`)

**Goal:** capture idle resource usage before any load.  
**Profile:** 1 VU, 60 s.  
**Endpoints hit:** `/healthz`, `/metrics`, `/api/v1/me`.

Check Grafana while this runs to record the resting RSS and CPU. This is your baseline for interpreting numbers in later scenarios.

---

### 02 — Warm cache reads (`perf:run:read`)

**Goal:** measure maximum throughput for already-cached artifacts.  
**Profile:** ramp 10 → 50 → 100 → 200 VU over ~4 min.

Every VU hits the same pre-warmed URL:

```
GET /proxy/perf-npm/perf-pkg/1.0.0/tarball
```

Because the artifact is in the filesystem cache after the first request, the server never contacts the mock upstream. This isolates the path: **auth middleware → rate-limit check → DB TTL query → filesystem read → stream to client**.

**Expected thresholds:** P95 < 200 ms, error rate < 1%.

**What degrades first:** the DB connection pool (default 10, raised to 50 in `config.perf.toml`). Watch for `pool_waiting` appearing in traces and latency climbing steeply around 100+ VU.

---

### 03 — Cache miss / proxy-through (`perf:run:miss`)

**Goal:** measure the full proxy pipeline including upstream fetch and cache write.  
**Profile:** 20 VU, 120 s. Each VU uses a unique version string (`0.<VU>.<ITER>`) so every request is a cache miss.

The path per request: **auth → DB check → upstream HTTP GET packument → upstream HTTP GET tarball → filesystem write → DB write → stream to client**.

**Expected thresholds:** P95 < 3 s (tunable by adjusting `--delay-ms` on mock upstream).

**What to tune:** restart `task perf:upstream` with `DELAY_MS=200` to simulate a slow upstream and see how latency distributes:

```bash
DELAY_MS=200 task perf:upstream
```

---

### 04 — Artifact upload (`perf:run:upload`)

**Goal:** measure publish throughput and memory pressure from buffering.  
**Profile:** 10 concurrent VUs, 60 s. Each publish is a uniquely-named version.

The upload path buffers the entire payload in memory before writing to disk. Default test artifact is 64 KB. Use `ARTIFACT_KB` to test larger sizes:

```bash
ARTIFACT_KB=1024 task perf:run:upload     # 1 MiB payloads
ARTIFACT_KB=51200 task perf:run:upload    # 50 MiB payloads — watch RSS carefully
```

**What to watch:** server RSS in Grafana. With 10 concurrent 50 MiB uploads, peak in-memory usage reaches ~500 MiB. This reveals the buffering bottleneck documented in §7.

---

### 05 — Realistic mixed workload (`perf:run:mixed`)

**Goal:** simulate a 10-minute production traffic mix to reveal how bottlenecks interact.  
**Profile (three named k6 scenarios running simultaneously):**

| Scenario | VUs | Type |
|----------|-----|------|
| `warm_read` | ramp 0→80 | cached GET |
| `cache_miss` | 10 constant | proxy-through |
| `upload` | 3 constant | PUT publish |

**Thresholds:** P95 < 200 ms for warm reads, P95 < 3 s for cache misses, error rate < 2%.

This is the most realistic run. The mixed write pressure on the DB (access_events inserts, quota updates, touch_artifact) combined with read load shows how much headroom the DB pool has.

---

### 06 — SBOM retrieval & export (`perf:run:sbom`)

**Goal:** measure the cost of the SBOM read path and the org-level export under load.  
**Profile (two named k6 scenarios running simultaneously, 60 s):**

| Scenario | VUs | Type |
|----------|-----|------|
| `sbom_read` | ramp 0→30 | `GET /api/v1/sbom/{registry}/{name}/{version}` (alternating `spdx`/`cyclonedx`) |
| `sbom_export` | 2 constant | `GET /api/v1/sbom/export?registry=...` (admin, alternating formats) |

`sbom_read` is a single keyed lookup in the `sbom` table (Postgres) — it should behave like a metadata read, similar in cost to scenario 02's DB query without the filesystem stream.

`sbom_export` merges **every** SBOM document recorded for the registry into one response (`SbomService::export_org_sbom`). Its cost grows with how many artifacts have been cached/published, so run scenarios 03-05 first to build up a realistic dataset before measuring export latency — a fresh seed only has one artifact.

**Expected thresholds:** `sbom_read` P95 < 300 ms; `sbom_export` P95 < 5 s; error rate < 1%.

**What to watch:** `sbom_export` latency vs. dataset size (number of cached/published artifact versions). If it grows linearly without bound, the export query/merge in `crates/core/src/services/sbom/mod.rs` has no pagination — this is the path to profile first if export becomes slow on a production-sized cache.

---

### 07 — Cache eviction sweep (`perf:run:eviction`)

**Goal:** measure the cost of `EvictionService::run_all()` while the cache is actively growing.  
**Profile (two named k6 scenarios running simultaneously, 60 s):**

| Scenario | VUs | Type |
|----------|-----|------|
| `cache_growth` | 10 constant | `GET /proxy/perf-npm/evict-pkg-{VU}/0.0.{ITER}/tarball` — new version every iteration (cache miss) |
| `eviction_sweep` | 1 req / 5s | `POST /api/v1/admin/registries/perf-npm/evict` (admin) |

Each `cache_growth` VU repeatedly fetches new versions of its own package (`evict-pkg-{VU}`), so `artifact_meta` accumulates several versions per package. `eviction_sweep` then calls the admin endpoint added in `crates/web/src/handlers/back_office/eviction.rs`, which runs every configured strategy (`run_ttl`, `run_idle`, `run_keep_latest_n`, `run_lru_size_cap`) and returns an `EvictResponse` with per-strategy counts. With `keep_latest_n = 3` (see `perf/config.perf.toml`), each sweep should report `evicted_old_versions > 0` once `cache_growth` has produced more than 3 versions per package.

**Expected thresholds:** `cache_growth` P95 < 3 s (same as scenario 03); `eviction_sweep` P95 < 5 s; error rate < 5%.

**What to watch:** `eviction_sweep` latency as the artifact_meta table grows — `run_keep_latest_n` loads `list_artifacts_by_package()` (all rows, ordered) on every call, so its cost is proportional to total cached versions across *all* registries, not just `perf-npm`. If this scales linearly without bound on a production-sized cache, that query is the first place to add pagination or a per-registry filter.

If `/evict` returns 404, check that `[registries.cache]` for `perf-npm` sets at least one of `artifact_ttl_secs` / `idle_days` / `max_size_bytes` / `keep_latest_n`.

---

### 10 — Soak / leak detection (`perf:soak`)

**Goal:** find out whether the server gives back what it took. Not a
measurement — a **verdict**, with an exit code.

**Profile:** a constant *arrival rate* (default 100 req/s), held for as long as
you ask, between two idle measurement windows:

```
warm-up load ──▶ quiesce ──▶ BASELINE ──▶ steady load ──▶ quiesce ──▶ FINAL
```

Unlike every scenario above, this one starts its own server and mock upstream:

```bash
task perf:soak                        # 10 minutes at 100 req/s
task perf:soak DURATION=1h RATE=200   # overnight
```

It compares idle RSS, the idle **live heap** (jemalloc's `stats.allocated`, via
`batlehub_memory_allocated_bytes`), open file descriptors, OS threads and held
database connections between the two windows, fits the RSS trend across the
sustained load, plots the curves (a text chart in the report, an SVG beside it),
and **ranks the registries by what they cost** — from the server's own
`/metrics`, scraped at both ends of the load and subtracted. That last one is
why `config.soak.toml` declares 25 registries with different shapes rather than
one: a single-registry run cannot answer "which is the worst consumer", and
registries that all cost the same thing rank by traffic rather than by cost.

RSS and live heap are two rows because they answer different questions. RSS is
pages the process holds; the live heap is bytes the *program* holds. When RSS
grows and the heap does not, the allocator is keeping pages — which is a real
thing to know about and is not a leak. Scenarios 02–07 are `constant-vus`, which is right for
throughput and wrong here: a server that slows down is then offered *less*
work, so the degradation hides itself. A constant arrival rate keeps the
offered load flat and lets the queue grow, which is what a real client
population does.

The full rationale — why both windows are idle, why the baseline comes after a
warm-up, and what each of the six signals means — is in
[`docs/contributing/testing.md` § 7-quater](../docs/contributing/testing.md),
and the thresholds are environment variables listed there.

---

### 11 — Startup and shutdown (`perf:lifecycle`)

**Goal:** how long the process takes to become useful, and how long it takes to
stop. **Profile:** not load at all — a handful of start/stop cycles, timed.

Every other scenario here starts a server, waits for `/healthz` and measures
what happens *after* that, so the two ends of a process's life were the two
parts nothing measured. They are what a rolling deployment is made of.

```bash
task perf:lifecycle                              # 1 cold start, 5 warm, 1 draining stop
task perf:lifecycle ITERATIONS=20 IN_FLIGHT=50   # tighter medians, heavier drain
```

Four numbers, in `perf/results/lifecycle.md`:

| | what it decides |
| --- | --- |
| cold start → healthy | whether a fresh replica beats its readiness probe; includes every migration |
| warm start → healthy | what a restart costs once the schema is there — the difference from the row above *is* the migration cost, measured rather than parsed from a log |
| port accepts → healthy | how long anything routing on the port rather than the probe sends traffic into a server that is not ready |
| stop, idle and draining | what `terminationGracePeriodSeconds` has to cover — actix stops accepting on `SIGTERM` and then waits for what is in flight, so this scales with the slowest upstream, not with this process's teardown |

The draining arm points a registry at the mock upstream running with
`--delay-ms 3000` and puts `IN_FLIGHT` uncached reads in flight before the
signal, because a stop with nothing in flight measures the floor and not the
number anyone needs.

There is no verdict and no threshold: a startup budget belongs to a deployment —
a probe's `failureThreshold`, a rollout's `maxUnavailable` — and this repository
does not own those numbers. It owns the measurement, and a number that moves is
visible in the diff of the report.

---

### 12 — What filtering a channel index costs (`perf:run:conda`)

**Goal:** put a number on the RAM cost of rewriting the largest document this proxy handles.
**Profile:** four runs, 4 VU × 60 s each, one arm per run.

conda's `repodata.json` is the outlier among every document here:
`conda-forge/linux-64` is ~424 MiB of JSON across ~1.4 million entries, and building a
`serde_json::Value` of it was measured at ~11.5 GB — past what a client will wait for and past what
a 16 GB runner has. That measurement is why `blocking::conda_stream` exists (it filters as it
copies, holding one package entry at a time) and why the compressed routes buffer the 55 MiB
`.zst` rather than what it decompresses to.

This scenario turns that claim back into a measurement. Two registries with **one difference**
between them — `perf-conda` has nothing blocked, `perf-conda-filtered` has one version blocked —
and two routes, whose four combinations are four different paths through the code:

| Arm | Route | What the server does |
| --- | --- | --- |
| `plain_unfiltered` | `repodata.json` | `StreamedIndex::AsIs` — socket to socket, nothing buffered |
| `plain_filtered` | `repodata.json` | buffers the **uncompressed** document, filters it as it copies, **caches nothing** |
| `zst_unfiltered` | `repodata.json.zst` | the upstream's own `.zst`, buffered once and cached |
| `zst_filtered` | `repodata.json.zst` | buffers the **compressed** document, filters, re-compresses, caches the result per blocked-set fingerprint |

Each arm is its own k6 run and its own row in the results table, because **peak RSS is the result**
and a run that mixed two paths could not say which one the peak belongs to.

```bash
# Terminal 1 — database
task compose:db
# Terminal 2 — a channel-sized index (200 000 entries ≈ 53 MiB of JSON)
task perf:conda:upstream PACKAGES=200000
# Terminal 3
task perf:conda:server
# Terminal 4
task perf:conda:seed          # blocks one version, and proves the two arms differ
task perf:run:conda           # four runs, four rows, then the report
```

`PACKAGES` is the independent variable — that is the whole point of the scenario. Raise it until
the document is the size of the channel you care about (`PACKAGES=1400000` is conda-forge's
`linux-64`) and read the RSS column. `ARTIFACT_KB` is the size of the package bytes each index
entry's sha256 is computed over, and it defaults to 1 KB here rather than the suite's 512 KB: the
digest in the index is the *real* digest of the bytes the mock will serve — the proxy verifies it —
so each entry costs one hash of that size when the index is generated, and 512 KB × 1.4 million
entries is 700 GB of hashing before the first request is answered.

#### What it measured, the first time it ran

200 000 entries — 51.1 MiB of JSON, 8.2 MiB as `.zst` — 2 VU, 30 s an arm, on an 8-core
workstation. Server peak RSS over the run, and CPU as a percentage of one core:

| Arm | req/s | p95 | p99 | RSS max | RSS median | CPU max | CPU median |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `plain_unfiltered` | 16.0 | 146 ms | 172 ms | 177 MiB | 158 MiB | 72 % | 58 % |
| `plain_filtered` | **0.7** | **2 944 ms** | 3 030 ms | **435 MiB** | 383 MiB | 200 % | 193 % |
| `zst_unfiltered` | 10.7 | 38 ms | 51 ms | 211 MiB | 197 MiB | 15 % | 13 % |
| `zst_filtered` | 8.8 | 42 ms | 1 178 ms | 254 MiB | 180 MiB | 200 % | 13 % |

Three things fall out of that, and only the first was expected:

1. **Filtering the compressed document is nearly free in the steady state.** `zst_filtered` is
   within noise of `zst_unfiltered` on latency (42 ms against 38 ms) and on median CPU (13 % both).
   The cost is real but paid *once*: the p99 of 1 178 ms and the 200 % CPU peak are the first
   request, which filtered the whole channel and cached the result; every request after it is a
   read from storage.
2. **Filtering the plain document is not free at all — and the reason is the cache, not the
   filter.** `plain_filtered` serves **0.7 requests a second** against 16, at 2.9 seconds a request
   and 435 MiB peak against 177. The compressed routes cache the filtered document per blocked-set
   fingerprint; the plain route caches nothing, so it buffers 51 MiB, filters 200 000 entries and
   re-serialises them *on every single request*, pinning two cores to do it.
3. **The memory ceiling is the uncompressed document, times the requests in flight.** 435 MiB at
   2 VUs is the 51 MiB input plus its output, twice over, on top of a 177 MiB baseline. At
   conda-forge's real size (424 MiB, 1.4 M entries) the same arithmetic is why
   `MAX_FILTERABLE_INDEX_BYTES` bounds the buffered form and why the compressed route buffers the
   55 MiB `.zst` rather than what it decompresses to.

The operational reading: a channel with blocks should be reached over `repodata.json.zst` — which
is what conda 23.x and mamba ask for first — and a client pinned to the plain document on a
filtered registry is paying three seconds and a quarter of a gigabyte per request for it.

**What to watch:** peak RSS on `plain_filtered` against `zst_filtered` at the same `PACKAGES`. The
two arms rewrite the same channel and answer the same question; the difference between them is the
streaming filter, which is the thing being measured. `zst_*` after the first request is a cache hit
(`X-BatleHub-Cache: hit`) served out of storage — the filtered document is cached per blocked-set
fingerprint — so what this scenario measures on that route is the **first** request and the
steady-state serving cost, which is exactly the pair worth knowing.

> The seed script waits for `blocked_snapshot_fingerprint` to turn over before it declares the arms
> ready — the blocked set is read from a 30-second snapshot, so a scenario started immediately after
> the block would measure the unfiltered path under a filtered name.

---

### 14 — Breaking point, per backend (`perf:break`)

`perf/k6/scenarios/14_breaking_point.js` + `perf/scripts/breaking_point.sh`. The offered rate doubles
at each step — 100, 200, 400, … — held for a minute apiece, until one of three things happens, and
the report records RAM, CPU and the whole latency distribution at every rate on the way up.

The three failure conditions catch different failures, which is why there are three:

| condition | default | what it catches |
| --- | --- | --- |
| errors | > 5 % | it answered `5xx`, or the connection never completed |
| p95 | > 5 000 ms | it answered, slowly enough that no client would wait |
| iterations never placed | > 5 % | k6 could not even *start* them — the server is refusing the rate while the requests it does answer still look healthy |

That last one is the one a latency-only check misses: a server that accepts 400 req/s and queues the
rest reports a beautiful p95 on the 400 it took.

The workload is the soak mix, deliberately: every registry kind and every shape of request — an
artifact read, a generated document, an upstream miss, a publish. A knee measured on warm cached
reads alone would be a number about the HTTP stack rather than about this server.

**It is a measurement, not a gate.** Only a server that *died* — a panic, an OOM kill, a process
that is no longer there — exits non-zero. Degrading under a rate no deployment will ever see is the
expected result and exits 0; a knee is not a defect, it is the number you wanted.

```bash
task perf:break                                              # filesystem + in-memory cache
task perf:break CONFIG=perf/config.perf-s3.toml LABEL=s3-redis
task perf:break BUDGET=600 STEP=30 START_RATE=200            # a shorter escalation
```

The report is `perf/results/breaking-point-<label>.md`, with the per-second `/proc` samples beside
it and the same numbers as JSON for diffing two backends.

**The matrix belongs to CI.** `.github/workflows/breaking-point.yaml` runs five arms in parallel
— filesystem or S3, in-memory or Redis, plus one repeat of `s3-redis` with a 50-connection pool — each with its own 20-minute budget, and comments one table
per backend on the pull request. Nothing schedules it: add the `breaking-point` label to a pull
request, or dispatch it once the workflow is on the default branch. Locally the S3 and Redis arms
need RustFS and Redis (`task perf:s3:infra:up`, which wants Podman); the filesystem arms need nothing
but the database.

CPU is reported as a percentage of **one** core, so a figure above 100 % means more than one core —
the same convention `record_run.py` uses for the results table.

**The knee is a comparison, not a capacity.** k6, the server, Postgres and the mock upstream share one
machine here, so the load generator competes with the thing it is measuring and the absolute number
moves with the runner: a knee at 400 req/s on an eight-core box says nothing about what a deployed
instance serves on its own hardware. What it *does* say is which backend gives out first, and by how
much — which is why the arms run the same escalation with the same budget on the same runner size.

**The report says where the queue was**, because the first CI run of this test did not and the number
was read as a capacity. All four backends broke at 400 req/s with the server at **58–67 % of one
core** on a four-core runner — four different storage and cache combinations giving the same answer
is the shape of a limit none of them own. So every step now records the database pool alongside RSS
and CPU (`pool free` is the fewest connections available at any sample of that step), and the verdict
names the ceiling it can see:

| what the breaking step shows | what the report says |
| --- | --- |
| pool reached 0 free, CPU below saturation | the knee is the **database pool** — the rate it caps is `connections / mean query time` |
| CPU at 80 % of the runner's cores or more | the knee is **this server's compute**, which is what the test is for |
| neither exhausted | the knee is **somewhere else**, and the first suspect is this harness sharing a machine |

The same capture records **what the backends cost beside it** — Postgres, Redis and RustFS resident
memory, summed per process name once a second and reported **per rate**, beside the server's own RSS.
Per rate and not once at the knee, because they move with what the server absorbs: Postgres grows
with the connections in flight and the work they carry, the object store with the bytes it serves,
Redis with what it is asked to hold. The shape across rates is the point — a backend whose memory
climbs faster than the server's is where the next ceiling will be, and a server that looks cheap
because the object store or the database is doing the work has moved the cost, not saved it. Two limits,
and the report says *empty* rather than zero for both: a backend in its own **PID namespace** is
invisible to `ps` (a Kubernetes sidecar, which is how a Che workspace supplies Postgres — so these
columns stay blank there and are filled in CI and under a local Podman compose), and a second
`postgres` on the same host would be summed in with the one under test.

That middle case is the only one that measures the server. `config.soak.toml` sizes the pool at **10
on purpose** — small enough that a leaked connection shows up inside a single soak — so a throughput
number measured against it is a number about that pool. The `s3-redis-pool50` arm exists to settle
it: the same backend as `s3-redis` with `PROXY_CACHE__DATABASE__MAX_CONNECTIONS=50` and nothing else
changed. If its knee moves, the other four measured the pool.

Locally, pass the same override:

```bash
PROXY_CACHE__DATABASE__MAX_CONNECTIONS=50 task perf:break LABEL=fs-memory-pool50
```

---

## Tuning the mock upstream

`task perf:upstream` accepts two variables:

| Variable | Default | Effect |
|----------|---------|--------|
| `DELAY_MS` | `0` | Simulated upstream response time (ms) |
| `ARTIFACT_KB` | `512` | Size of served artifact bodies (KB) |

Examples:

```bash
# Simulate a 100 ms upstream (CDN-like latency)
DELAY_MS=100 task perf:upstream

# Simulate a slow upstream + large artifacts
DELAY_MS=500 ARTIFACT_KB=4096 task perf:upstream
```

### Its artifacts are deterministic, and that is load-bearing

An artifact's bytes are derived from its coordinate, and the packument
advertises the **real** sha1 of the bytes the mock will serve.

It used to serve random bytes under a made-up `dist.shasum`, which was fine
until the proxy started verifying the digest it is given
(`crates/core/src/services/integrity.rs`). From that day every artifact read in
every scenario was refused with a `502`, and the scenarios that check for a
`200` had been failing ever since — quietly, because nobody reruns a perf suite
to see whether it still passes. The soak harness found it while seeding a warm
cache.

The determinism matters beyond the digest: a cache is supposed to return the
bytes it stored, and an upstream whose answer changes per request makes "the
same artifact" meaningless — a hit and a miss would be distinguishable by
content, which is not a property any real registry has.

---

## The results table {#the-results-table}

Every scenario run through `run_with_metrics.sh` — which is every `task perf:run:*` — appends one
row to `perf/results/runs.jsonl`: **peak and median RSS**, **peak and median CPU**, and k6's own
throughput, latency and error numbers, with the git sha, the backend and the machine it was
measured on. The box printed on the terminal used to be the only record, and the next run scrolled
it away.

```bash
task perf:run:all      # runs every scenario, then writes the report
task perf:report       # rebuild the table from whatever has been recorded so far
```

The report lands in two files that say the same thing to two readers:

Both land in `perf/results/`:

| File | For |
| --- | --- |
| `perf-report.md` | a person — one row per scenario, peak RSS beside the latency |
| `perf-report.json` | the *next* release, which diffs against it |

The latest row per scenario wins, so re-running one scenario replaces its line without invalidating
the rest of the suite. Neither file is committed: they describe one machine on one day. The
`authz-*.json` measurements beside them are committed because they are a *documented* run quoted by
an RFC — a different kind of artefact.

**Peak, not average.** A server that sits at 90 MiB and spikes to 1.4 GiB while filtering a channel
index needs the 1.4 GiB written down: that is the figure a memory limit has to clear, and an average
hides it completely. The median is recorded beside it so a spike can be told from a level shift.

### Comparing two releases

Copy the release's report into `perf/results/` first, and pass its **name**:
every argument of `perf_report.py` is a file name in that one directory, so
there is no path for a caller — or for whatever is driving the caller — to
point somewhere else.

```bash
# Against a previous release's report (downloaded from its GitHub release page)
cp ~/Downloads/perf-report.json perf/results/perf-report-1.2.0.json
task perf:report:compare BASE=perf-report-1.2.0.json

# As a verdict rather than a diff — exits non-zero past the margin
task perf:report:compare BASE=perf-report-1.2.0.json FAIL=1 MARGIN=25
```

The diff adds Δ columns for p95 and peak RSS and marks the direction. It also **refuses to pretend**
two incomparable runs are comparable: a different CPU count, architecture or storage backend
produces a warning above the table, because a number that moved may be the machine rather than the
code.

`.github/workflows/perf-report.yaml` does this on every release tag — runs scenarios 01–07 against a
freshly built server, diffs the result against the previous published release's `perf-report.json`,
and attaches `perf-report.md` and `perf-report.json` to the release. It is a **record, not a gate**:
a shared runner is noisy enough that two runs of the same commit differ by more than most real
regressions, so what it is for is the shape — peak RSS doubling, throughput halving, a scenario that
started erroring — and a person reads it.

---

## Reading the results

### k6 terminal output

After each scenario, k6 prints a summary:

```
✓ status 200
✓ body non-empty

checks.........................: 100.00% ✓ 48312  ✗ 0
data_received..................: 24 GB   40 MB/s
http_req_duration...............: avg=12ms   min=1ms   med=8ms    max=892ms  p(90)=28ms   p(95)=45ms
http_req_failed.................: 0.00%  ✓ 0      ✗ 24156
iterations.....................: 24156   402/s
```

Key columns: `p(95)` latency, `iterations/s` (≈ req/s for single-request VUs), `http_req_failed` rate.

### Grafana dashboard

Open **http://localhost:3000** → BatleHub folder → **BatleHub Performance**.

Panels:

| Panel | What to look for |
|-------|-----------------|
| **Request Rate** | req/s by registry and outcome — should track k6 iterations/s |
| **Latency P50/P95/P99** | Where P95 climbs steeply = bottleneck point |
| **Cache Hit Rate** | Should be ~100% during scenario 02; ~0% during 03 |
| **Upstream Errors** | Non-zero = mock upstream overloaded or mis-configured |
| **Artifact Cache Hits vs Misses** | Cross-check with k6 scenario |
| **Latency Heatmap** | Bimodal distribution = two code paths competing |

### System resource monitoring

While tests run, watch server resources in a separate terminal:

```bash
# CPU and memory of the batlehub process
watch -n1 "ps -o pid,pcpu,pmem,rss,vsz,comm -p \$(pgrep batlehub)"

# Or with pidstat (more detail)
pidstat -u -r -p \$(pgrep batlehub) 1
```

---

## Known bottlenecks and what to watch

These are the code paths identified as likely degradation points, in priority order:

### 1. DB connection pool

**Config:** `max_connections = 50` in `perf/config.perf.toml` (raised from the default 10).  
**Trigger:** scenario 02 at 100+ VU.  
**Signal:** P95 latency climbs non-linearly; sqlx pool queue grows.  
**Location:** `crates/adapters/src/cache/postgres.rs` — every cache hit writes `access_events` and potentially `touch_artifact`.

To observe the default-10 behaviour, edit `config.perf.toml` and set `max_connections = 10`, then rerun scenario 02.

---

### 2. Artifact buffering (upload memory pressure)

**Trigger:** scenario 04 with large `ARTIFACT_KB`.  
**Signal:** server RSS grows proportionally to VU × artifact size.  
**Location:** `crates/web/src/handlers/proxy/npm/write.rs` — the entire publish payload (JSON + base64 tarball) is collected into a `Bytes` before `LocalRegistryService::publish` writes it to storage.

Run scenario 04 with 50 MiB payloads and watch RSS in `ps` or Grafana (node-exporter if added).

---

### 3. Filesystem `exists()` blocking call

**Trigger:** scenario 02 at high VU.  
**Signal:** tokio thread pool CPU spikes; latency tail widens.  
**Location:** `crates/adapters/src/storage/filesystem.rs` — `path.exists()` is a synchronous syscall not wrapped in `spawn_blocking`.

This manifests as higher P99 without a corresponding P95 increase.

---

### 4. `touch_artifact` DB write on every cache hit

**Trigger:** scenario 02 at high sustained RPS.  
**Signal:** DB write rate equals request rate even with 100% cache hits.  
**Location:** `crates/core/src/services/proxy/handle.rs` — `touch_artifact()` is spawned async on every served hit.

Disable artifact TTL in `config.perf.toml` to measure the difference:

```toml
# comment out artifact_ttl_secs under [registries.cache] to skip the touch path
```

---

### 5. Rate-limit middleware lock

**Trigger:** scenario 05 (mixed) at sustained 1k+ req/s.  
**Signal:** CPU increases without proportional throughput gain; latency tail spikes.  
**Location:** `crates/adapters/src/rate_limit/in_memory.rs` — single Mutex/RwLock protecting the token-bucket map.

To disable rate limiting for a clean baseline, remove the `[registries.rate_limit]` blocks from `perf/config.perf.toml`.

---

### 6. Eviction sweep cost (`run_keep_latest_n` / `run_lru_size_cap`)

`crates/core/src/services/eviction` implements TTL, idle-day, keep-latest-N, and LRU size-cap eviction, exposed via `POST /api/v1/admin/registries/{registry}/evict` (`crates/web/src/handlers/back_office/eviction.rs`). Scenario 07 exercises this endpoint while scenario-03-style cache-miss traffic grows `artifact_meta` concurrently.

**Trigger:** scenario 07, or any registry with `[registries.cache] keep_latest_n` / `max_size_bytes` set under sustained cache-miss load.  
**Signal:** `eviction_sweep` P95 latency climbing as the total number of cached artifact versions (across *all* registries) grows.  
**Location:** `run_keep_latest_n` calls `list_artifacts_by_package()`, which has no registry filter or pagination — its cost is proportional to the entire `artifact_meta` table, not just the registry being swept.

If this becomes the dominant cost on a production-sized cache, the fix is to add a `registry` filter (and/or pagination) to `list_artifacts_by_package()`.

---

## Running against a remote server

All k6 scenarios read `BATLEHUB_URL` and `BATLEHUB_TOKEN` from the environment:

```bash
export BATLEHUB_URL=https://batlehub.example.com
export BATLEHUB_TOKEN=your-token-here

task perf:run:read
```

The seed script also accepts a URL argument:

```bash
bash perf/scripts/seed.sh https://batlehub.example.com
```

When testing a remote server, skip `task perf:upstream` (the real upstream is used) and skip `task perf:infra:up` (point Prometheus at the remote `/metrics` endpoint instead by editing `perf/prometheus.yml`).
