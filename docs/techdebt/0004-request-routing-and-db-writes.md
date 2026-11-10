# TD-0004 — What a request still pays for: routing, the audit write, the pool

| | |
| --- | --- |
| **Status** | Paid |
| **Found** | 2026-11-09, PR #297: `tests/heavy/db_calls.sh`, the `breaking-point` label ([run 42517429475](https://github.com/batlehub/batlehub/actions/runs/42517429475)) and the `profile` label ([run 42517263643](https://github.com/batlehub/batlehub/actions/runs/42517263643)) |
| **Paid** | `42eb100` (§1), `995e301` (§2), `752c175` (§3), `2cbea74` (§4), 2026-11-10; the gains under load are not re-measured, see each section |
| **Area** | `crates/web/src/lib.rs` (routing), access events, `[database]` defaults, `/healthz` |

PR #297 took the per-download reads off the database (an in-process read
cache kept coherent by `LISTEN/NOTIFY`, migration 066,
`crates/adapters/src/db/read_cache.rs`): a cached download costs 1–2
statements instead of 5–7, and the pullability batch 1 instead of 200. One
replica with the default 10-connection pool still breaks at 400–800 req/s,
on the pool and not the CPU. What is left follows; it continues
[TD-0001](/techdebt/0001-per-request-sql-and-profile-coverage) §1 and §3.

## 1. The router tries every route in turn

`collect_routes` (`crates/web/src/lib.rs`) registers 384 flat services, 291 of them dynamic and 220 under `/proxy/{registry}/`; actix-router 0.5.4 runs each pattern's regex until one matches, so an npm request is tested against ~217 regexes before its own. Measured on the PR #297 profile: 100 % of the `regex_automata` samples are `AppRouting::call → ResourceDef::capture_match_info`, 21–52 % of a request's CPU on the arms registered late (npm, openvsx, jetbrains, composer), 3 % on those registered early (helm, devfile). Fix: one `scope("")` per registry kind, guarded by the kind as `is_helm`/`is_devfile` already are (`utoipa_actix_web::scope` passes `.guard()` through and keeps the OpenAPI paths), each keeping its routes' relative order; the multi-kind handlers (`get_version`/`get_packument` for npm and cargo, the forge routes, openvsx/vscode) registered in each of their kinds' scopes, and a 404 default service per scope so a wrong-kind path does not reach the SPA fallback. `protocol_conformance.rs`, `authz_matrix.rs` and `profile_route_coverage` are the safety net. Switching to axum/matchit would also fix it but means rewriting 384 handlers, the extractors and the utoipa wiring for the same gain.

**Paid** (`perf(web): route a proxy request on its own registry kind`). Each
kind's routes now live in a `scope("")` guarded by the kind, resolved once per
request from `RegistryMap` and kept in the request's extensions (`routed_kind`,
`kind_scope` in `crates/web/src/lib.rs`), in their former relative order;
`get_version`/`get_packument` sit in both the npm and cargo scopes, the forge
routes in each forge's, and openvsx/vscode-marketplace share one scope. A
path none of a kind's routes match gets that scope's JSON 404, and an unknown
registry a last `/proxy/{registry}` scope's. `match_pattern()` returns the
same full patterns, so `http.route` and `profile_routes.txt` keep their keys.

Measured in process (release build, 20 000 requests to a cheap 404 after 500
warm-up, everything else equal — the old `lib.rs` against the new):

| Path | Before | After |
| --- | ---: | ---: |
| helm (`…/helm/api/charts/nope`, registered early) | 12.8 µs | 10.5 µs |
| composer (`…/p2/nope/nope.json`) | 54.6 µs | 11.9 µs |
| npm (`…/nope-pkg`, registered last) | 52.9 µs | 13.4 µs |

A late-registered kind now costs what an early one did. One behaviour moved
with it: a path shaped like another kind's route used to reach that kind's
handler and be refused as "not a X registry"; it now reaches its own kind's
routes (an npm registry answers `…/registry/config.json` as a package version,
not cargo's 404). The `*_wrong_registry_type_*` tests now assert the other
kind's handler is not reached. The profile's router share has not been
re-measured.

## 2. One audit write per read

Every download inserts an `access_events` row (and the first-sight `package_statuses` row) in the request's own statement: ~69 M rows/day at 800 req/s, each holding a pool connection. Batch them (a channel drained every ~100 ms into one multi-row `INSERT` or `COPY`), keeping the "no event lost on a clean shutdown" property; then decide retention against the purge job's cost.

**Paid** (`perf(db): write access events in batches`). `PgPackageRepository`
hands each event to `AccessBatcher` (`crates/adapters/src/db/packages/access_batch.rs`),
whose task writes what arrived in the last 100 ms (at most 1 000 rows) as one
`UNNEST` statement carrying both inserts; `created_at` is the event's own
timestamp, so a row still lands in its seal window (sealed one window late,
RFC 0036 §5.3). No event is dropped on that path: a full queue (10 000) writes
inline as before, a failed batch is retried row by row, and the server flushes
the queue after actix has drained its requests on a clean shutdown. Batching is
started by the server only; a repository built without it (every integration
test) writes inline. `pg_hot_path.rs` checks the batched rows against the
inline ones on a real Postgres, a coordinate repeated in one batch included.

`tests/heavy/db_calls.sh` shows the write leaving the request: on
`proxy-tarball-pat` the request itself runs no statement and the batch is
counted as spawned. The harness sends one request at a time, so its batches
hold one row; what it buys under load, one connection per 100 ms instead of one
per download, has not been measured with `breaking-point`. Retention against
the purge job's cost is left to [TD-0002](/techdebt/0002-feat-devfile-review-leftovers).

## 3. The pool size default of 10

It is the measured ceiling (`s3-redis-pool50` serves 800 req/s clean where the 10-connection arms break). Raise the default to 20–30, document sizing per replica, and recommend PgBouncer (transaction mode) once there are several replicas; the read cache's `LISTEN` holds one connection of the pool.

**Paid** (`perf(config): a 20-connection pool by default`). The default is 20
(`default_max_connections`, `crates/config/src/schema/server.rs`), which keeps
four replicas under PostgreSQL's own 100. The sizing section of the
configuration guide (`docs/guide/configuration.md`, *Sizing
`max_connections`*, and its French twin) now gives the measured reason, the
PgBouncer (transaction mode) recommendation past four replicas with
`listen_url` on a direct connection, and the connection the read cache's
listener holds without it. Not re-measured with `breaking-point` at 20.

## 4. `/healthz` runs 3 statements

`SELECT 1` plus a storage probe that resolves a key through the dedup tables. Keep the storage probe on readiness only; liveness needs no database round trip at all.

**Paid** (`perf(web): /healthz probes the store without the dedup tables`).
Liveness was already `/livez`, which does no I/O, and the chart points its
`livenessProbe` there; `/healthz` is the readiness probe. Its storage probe now
looks up a `staging:` key, which a storage router hands straight to the backend
instead of resolving it through `artifact_dedup_refs`/`artifact_storage`, so
it probes the object store rather than the database a second time.
`db_calls.sh`: `healthz` runs 1 statement (`SELECT 1`), down from 3, and the
budget says so.
