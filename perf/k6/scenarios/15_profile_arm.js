/**
 * Scenario 15 — one soak arm, alone, at a constant rate: the load behind one
 * CPU profile.
 *
 * `perf/scripts/profile.sh` runs this once per arm while the server samples
 * its own CPU (`/debug/pprof/folded`, the `profiling` feature). One arm per
 * window is what attributes the profile to a path: every sample in the window
 * was spent serving this arm, and nothing has to be guessed from a stack.
 *
 * The arm is the soak's own (`soak_arms.js`), with the same bounded rotation,
 * so the profile is of the workload the leak gate already measures.
 *
 * Run alone:
 *   BATLEHUB_URL=http://127.0.0.1:8180 BATLEHUB_PROFILE_ARM=npm-packument \
 *     k6 run perf/k6/scenarios/15_profile_arm.js
 */
import http from "k6/http";
import { check, fail } from "k6";
import { ADMIN_TOKEN } from "../config.js";
import { ARMS } from "../soak_arms.js";

const OP = __ENV.BATLEHUB_PROFILE_ARM;
const DURATION = __ENV.BATLEHUB_PROFILE_DURATION || "15s";
const RATE = Number(__ENV.BATLEHUB_PROFILE_RATE || 50);
const ARM = ARMS.find((a) => a.op === OP);
if (!ARM) {
  fail(`unknown arm '${OP}' — one of: ${ARMS.map((a) => a.op).join(", ")}`);
}

const PRE_ALLOCATED = Math.max(10, Math.ceil(RATE / 2));

export const options = {
  scenarios: {
    arm: {
      executor: "constant-arrival-rate",
      rate: RATE,
      timeUnit: "1s",
      duration: DURATION,
      preAllocatedVUs: PRE_ALLOCATED,
      maxVUs: PRE_ALLOCATED * 4,
      exec: "one",
    },
  },
  // Same rule as the soak: only 5xx and transport errors are failures, so the
  // miss arm's 404 is the answer it is meant to get.
  thresholds: { http_req_failed: ["rate<0.02"] },
};

http.setResponseCallback(http.expectedStatuses({ min: 200, max: 499 }));

const AUTH = { Authorization: `Bearer ${ADMIN_TOKEN}` };

export function one() {
  const n = ARM.space ? (__VU * 7919 + __ITER) % ARM.space : 0;
  const req = ARM.request(n);
  const res = http.request(req.method || "GET", req.url, req.body || null, {
    headers: { ...AUTH, ...req.headers },
  });
  check(res, { [`${ARM.op} → ${ARM.expect.join("|")}`]: (r) => ARM.expect.includes(r.status) });
}
