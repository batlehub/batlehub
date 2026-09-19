/**
 * Scenario 14 — where it breaks, and what it costs on the way.
 *
 * One **step** at one arrival rate. The escalation is `perf/scripts/breaking_point.sh`'s
 * job, not k6's: it runs this file once per rate, doubling until something gives, and
 * judges each step from the summary export. That split is deliberate —
 *
 *   - a `ramping-arrival-rate` inside k6 would blur the stages together in the
 *     summary, and the number this test exists to produce is *per rate*: the p95
 *     at 400 req/s means nothing if it is averaged with the p95 at 3 200;
 *   - a k6 `threshold` with `abortOnFail` would stop the run at the first breach,
 *     which is the one moment the measurement is most interesting. The runner
 *     lets the step finish and then decides.
 *
 * The workload is the soak mix (`soak_arms.js`), for the same reason the soak uses
 * it: it is the only workload in this tree that touches every registry kind and
 * every shape of request — an artifact read, a generated document, an upstream
 * miss, a publish. A breaking point measured on warm cached reads alone would be
 * a number about the HTTP stack, not about this server.
 *
 * Bodies are discarded: the load generator shares a machine with the server here,
 * and a client that keeps megabytes of packument per VU is a client that breaks
 * first. k6 still reads them off the wire, so the server pays for the transfer.
 */
import http from "k6/http";
import { check } from "k6";
import { ADMIN_TOKEN } from "../config.js";
import { armForSlot, TOTAL_WEIGHT } from "../soak_arms.js";

const RATE = Number(__ENV.BATLEHUB_BP_RATE || 100);
const STEP = __ENV.BATLEHUB_BP_STEP || "60s";

export const options = {
  discardResponseBodies: true,
  scenarios: {
    step: {
      executor: "constant-arrival-rate",
      rate: RATE,
      timeUnit: "1s",
      duration: STEP,
      // Sized from the rate rather than fixed: too few VUs and k6 reports its
      // own queueing as the server's latency, which is the classic way to
      // measure a load generator and call it a server.
      preAllocatedVUs: Math.max(50, Math.ceil(RATE / 2)),
      maxVUs: Math.max(500, RATE * 4),
    },
  },
  // Empty on purpose: the runner judges. See the header.
  thresholds: {},
};

export default function breaking_point() {
  const slot = (__VU * 7919 + __ITER) % TOTAL_WEIGHT;
  const arm = armForSlot(slot);
  const n = arm.space ? (__VU * 7919 + __ITER) % arm.space : 0;
  const req = arm.request(n);
  const res = http.request(req.method || "GET", req.url, req.body || null, {
    headers: { Authorization: `Bearer ${ADMIN_TOKEN}`, ...req.headers },
    tags: { op: arm.op, registry: arm.registry, kind: arm.kind },
    // Judged against what the arm *declares*, not against 2xx. Several arms in
    // this mix answer a non-2xx by design — the cache-miss arm's `404` is the
    // whole point of it, and the publish arm's `409` is a coordinate that is
    // already there — and `http_req_failed` counts every one of those as a
    // failure by default. Measured: 5.4% "errors" at 100 req/s on a server the
    // soak runs at the same rate with none, which would have named 100 req/s as
    // the breaking point of a server that had not broken.
    responseCallback: http.expectedStatuses(...(arm.expect || [200])),
  });
  check(res, { "not 5xx": (r) => r.status < 500 });
}
