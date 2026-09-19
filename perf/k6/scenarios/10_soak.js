/**
 * Scenario 10 — Soak: constant load, held, to make a leak visible.
 *
 * Every other scenario here asks "how fast is it". This one asks "does it give
 * anything back", which is a different question and needs a different shape:
 *
 *   - **constant arrival rate**, not constant VUs. Under `constant-vus` a
 *     server that slows down is *offered less work*, so the very degradation a
 *     soak is looking for hides itself by reducing the load that causes it.
 *     `constant-arrival-rate` keeps the offered rate flat and lets the queue
 *     grow, which is what a real client population does.
 *   - **a fixed mix**, rotated deterministically rather than sampled at random,
 *     so two runs of the same duration issue the same requests in the same
 *     proportions and their resource curves are comparable.
 *   - **no latency threshold by default.** A soak that fails on p95 tells you
 *     about the runner it shared a core with. The verdict lives in
 *     `perf/scripts/soak.sh`, which measures the server process itself.
 *
 * The mix covers the allocation paths that differ from one another, because a
 * leak is a property of a code path and not of a request count: an artifact
 * read that streams from storage, a listing document that is parsed and
 * re-serialised, a miss that goes upstream and writes to the cache, a publish
 * that writes to the database, an admin read that takes a pool connection, and
 * an SBOM read that builds a document per request.
 *
 * Environment:
 *   BATLEHUB_SOAK_DURATION  how long to hold the load   (default 10m)
 *   BATLEHUB_SOAK_RATE      requests per second offered (default 100)
 *   BATLEHUB_SOAK_PHASE     tag for the run — `warmup` or `steady`
 *
 * Run through the script, which starts the server and returns a verdict:
 *   task perf:soak DURATION=10m RATE=100
 */
import http from "k6/http";
import { check } from "k6";
import { ADMIN_TOKEN } from "../config.js";
import { armForSlot, ARMS, SOAKED_KINDS, TOTAL_WEIGHT } from "../soak_arms.js";

const DURATION = __ENV.BATLEHUB_SOAK_DURATION || "10m";
const RATE = Number(__ENV.BATLEHUB_SOAK_RATE || 100);
const PHASE = __ENV.BATLEHUB_SOAK_PHASE || "steady";

// One VU can only be in one request at a time, so the pool has to cover the
// rate times the slowest thing in the mix. Sized at 1 s of arrivals with a
// floor, and allowed to grow: k6 warns rather than dropping iterations, and a
// dropped iteration would be a hole in the load the verdict cannot see.
const PRE_ALLOCATED = Math.max(20, Math.ceil(RATE));

export const options = {
  scenarios: {
    soak: {
      executor: "constant-arrival-rate",
      rate: RATE,
      timeUnit: "1s",
      duration: DURATION,
      preAllocatedVUs: PRE_ALLOCATED,
      maxVUs: PRE_ALLOCATED * 4,
      exec: "mixed",
      tags: { phase: PHASE },
    },
  },
  thresholds: {
    // The one thing that is a finding rather than a measurement: requests that
    // failed. A soak whose error rate climbs has already told you something,
    // whatever the memory curve says.
    http_req_failed: ["rate<0.02"],
  },
  // A soak's value is the curve, and summary percentiles hide it. The script
  // reads this file back to report the trend across the run.
  summaryTrendStats: ["min", "med", "p(95)", "p(99)", "max"],
};

// **What counts as a failed request.** k6's default is "not 2xx or 3xx", which
// here would count the cache-miss arm's 404s — a name that does not exist,
// answered correctly — as failures, and put the run's failure rate at roughly
// the share of the mix that asks for one. A soak's error rate has to mean
// "the server could not answer", so only 5xx and transport errors count.
http.setResponseCallback(http.expectedStatuses({ min: 200, max: 499 }));

const AUTH = { Authorization: `Bearer ${ADMIN_TOKEN}` };

/**
 * The mix, from `perf/k6/soak_arms.js`.
 *
 * The arms used to be a function each and their percentages a ladder of `else
 * if`s in this file. They are a table now for one reason: the claim "the soak
 * drives every registry kind" has to be checkable, and three things have to
 * agree for it to be true — the arms, the registries in `perf/config.soak.toml`
 * and the excuses in `crates/web/tests/soak_kind_coverage.rs`. A table can be
 * read by the pre-flight (`11_soak_arms.js`) and by that test. A ladder of
 * `else if`s can only be read by a person.
 *
 * Deterministic rotation on the iteration counter rather than `Math.random()`:
 * two runs then issue the same requests in the same order, and a difference
 * between their resource curves is the server's.
 */
export function mixed() {
  const slot = (__VU * 7919 + __ITER) % TOTAL_WEIGHT;
  const arm = armForSlot(slot);
  // Each arm rotates within its own bounded coordinate space — see the header
  // of `soak_arms.js`, and `MISS_SPACE` / `PUBLISH_SPACE` there, for why every
  // one of them is finite.
  const n = arm.space ? (__VU * 7919 + __ITER) % arm.space : 0;
  const req = arm.request(n);

  const res = http.request(req.method || "GET", req.url, req.body || null, {
    headers: { ...AUTH, ...req.headers },
    // Every request carries the registry it is against, because the report
    // ranks registries and a request with no registry cannot be attributed.
    // The server's own `/metrics` is what the ranking is computed from; these
    // tags are the client-side check on it.
    tags: { op: arm.op, registry: arm.registry, kind: arm.kind },
  });

  check(res, { [`${arm.op} not 5xx`]: (r) => r.status < 500 });
}

export function setup() {
  console.log(
    `soak mix: ${ARMS.length} arms, ${SOAKED_KINDS.length} registry kinds, ` +
      `total weight ${TOTAL_WEIGHT}`,
  );
}
