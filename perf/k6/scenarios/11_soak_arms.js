/**
 * Scenario 11 — the soak's pre-flight: every arm, once, asserted.
 *
 * `perf/scripts/soak.sh` runs this before the warm-up, and a failure stops the
 * run. It exists because of a specific way a soak can be green and worthless:
 * the load's own check is "not 5xx", so an arm whose URL is wrong, whose
 * registry is misconfigured, or whose upstream route was never written answers
 * `404` and *passes*. The report then shows that registry costing nothing,
 * which reads exactly like a cheap registry.
 *
 * So this asks each arm for its first coordinate and requires one of the
 * statuses the arm itself declares in `expect`. It is also the seed: every
 * document the mix will ask for is fetched once here, so the warm-up measures
 * steady behaviour rather than first-touch.
 *
 * Run alone:
 *   BATLEHUB_URL=http://127.0.0.1:8180 k6 run perf/k6/scenarios/11_soak_arms.js
 */
import http from "k6/http";
import { check, fail } from "k6";
import { ADMIN_TOKEN } from "../config.js";
import { ARMS, SOAKED_KINDS } from "../soak_arms.js";

export const options = {
  scenarios: {
    arms: { executor: "per-vu-iterations", vus: 1, iterations: 1, exec: "everyArm" },
  },
  // Every check must pass. This is the whole point of the scenario: a rate of
  // 0.98 means an arm is broken and the soak that follows would not say so.
  thresholds: { checks: ["rate==1.00"] },
};

// The arms declare their own acceptable statuses, so k6's "not 2xx is a
// failure" default would double-count the miss arm's legitimate 404.
http.setResponseCallback(http.expectedStatuses({ min: 200, max: 499 }));

const AUTH = { Authorization: `Bearer ${ADMIN_TOKEN}` };

export function everyArm() {
  const broken = [];

  for (const arm of ARMS) {
    const req = arm.request(0);
    const method = req.method || "GET";
    const headers = { ...AUTH, ...(req.headers || {}) };
    const res = http.request(method, req.url, req.body || null, { headers });

    const ok = arm.expect.includes(res.status);
    check(res, { [`${arm.op} → ${arm.expect.join("|")}`]: () => ok });
    if (!ok) {
      broken.push(`${arm.op} (${arm.kind}): ${method} ${req.url} → ${res.status}`);
    }
  }

  console.log(
    `soak arms: ${ARMS.length} arms over ${SOAKED_KINDS.length} registry kinds ` +
      `(${SOAKED_KINDS.join(", ")})`,
  );

  if (broken.length) {
    fail(
      `${broken.length} of ${ARMS.length} soak arms did not answer as declared:\n  ` +
        broken.join("\n  "),
    );
  }
}
