# TD-0004 — What a request still pays for: routing, the audit write, the pool

| | |
| --- | --- |
| **Status** | Open |
| **Found** | 2026-11-09, PR #297: `tests/heavy/db_calls.sh`, the `breaking-point` label ([run 42517429475](https://github.com/batlehub/batlehub/actions/runs/42517429475)) and the `profile` label ([run 42517263643](https://github.com/batlehub/batlehub/actions/runs/42517263643)) |
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

## 3. The pool size default of 10

It is the measured ceiling (`s3-redis-pool50` serves 800 req/s clean where the 10-connection arms break). Raise the default to 20–30, document sizing per replica, and recommend PgBouncer (transaction mode) once there are several replicas; the read cache's `LISTEN` holds one connection of the pool.

## 4. `/healthz` runs 3 statements

`SELECT 1` plus a storage probe that resolves a key through the dedup tables. Keep the storage probe on readiness only; liveness needs no database round trip at all.
