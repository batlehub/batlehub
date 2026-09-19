/**
 * Scenario 12 — What filtering a channel index costs
 *
 * conda's `repodata.json` is the largest document this proxy ever rewrites:
 * `conda-forge/linux-64` is ~424 MiB of JSON across ~1.4 million entries, and
 * building a `serde_json::Value` of it costs ~11.5 GB. That measurement is why
 * `blocking::conda_stream` exists — it filters as it copies, holding one
 * package entry at a time — and why the compressed routes buffer the 55 MiB
 * `.zst` rather than the document it decompresses to.
 *
 * This scenario is what turns that claim back into a number. **One arm per
 * run**, because the thing being measured is peak RSS and a run that mixes two
 * paths cannot say which one the peak belongs to:
 *
 *   plain_unfiltered  GET repodata.json      on a registry with nothing blocked
 *   plain_filtered    GET repodata.json      on a registry with one block
 *   zst_unfiltered    GET repodata.json.zst  on a registry with nothing blocked
 *   zst_filtered      GET repodata.json.zst  on a registry with one block
 *
 * The plain arms go through the parsed path (`repodata_bytes` → `Value` →
 * `strip_repodata`); the `.zst` arms go through the streaming one
 * (`StreamedIndex::Filter` → `conda_stream::filter_repodata`). Comparing
 * `plain_filtered` against `zst_filtered` at the same channel size is the
 * measurement; comparing each against its unfiltered arm is what says how much
 * of the cost is the filter rather than the document.
 *
 * `task perf:run:conda` runs all four and records one table row each, so the
 * report's RSS column *is* the result.
 *
 * Pre-requisites: task perf:conda:upstream, task perf:conda:server,
 *                 task perf:conda:seed.
 * Run: BATLEHUB_CONDA_ARM=zst_filtered k6 run perf/k6/scenarios/12_conda_filter.js
 */
import http from "k6/http";
import { check } from "k6";
import { BASE_URL, ADMIN_TOKEN, CONDA_REGISTRY } from "../config.js";

const FILTERED_REGISTRY =
  __ENV.BATLEHUB_CONDA_FILTERED_REGISTRY || "perf-conda-filtered";
const PLATFORM = __ENV.BATLEHUB_CONDA_PLATFORM || "linux-64";
// Seeded by `perf/scripts/seed_conda.sh`; the filename the block removes.
const BLOCKED_FILE = `${__ENV.BATLEHUB_CONDA_BLOCK_NAME || "perf-conda-0000"}-${
  __ENV.BATLEHUB_CONDA_BLOCK_VERSION || "1.0.0"
}-py311_0.conda`;

const ARMS = {
  plain_unfiltered: { registry: CONDA_REGISTRY, doc: "repodata.json", filtered: false },
  plain_filtered: { registry: FILTERED_REGISTRY, doc: "repodata.json", filtered: true },
  zst_unfiltered: { registry: CONDA_REGISTRY, doc: "repodata.json.zst", filtered: false },
  zst_filtered: { registry: FILTERED_REGISTRY, doc: "repodata.json.zst", filtered: true },
};

const ARM_NAME = __ENV.BATLEHUB_CONDA_ARM || "zst_filtered";
const ARM = ARMS[ARM_NAME];
if (!ARM) {
  throw new Error(
    `BATLEHUB_CONDA_ARM=${ARM_NAME} is not one of ${Object.keys(ARMS).join(", ")}`,
  );
}

export const options = {
  vus: Number(__ENV.BATLEHUB_VUS || 4),
  duration: __ENV.BATLEHUB_DURATION || "60s",
  // No latency threshold. A channel index is tens to hundreds of megabytes and
  // its response time is dominated by how fast the two processes can move it
  // across loopback — a number that says more about the machine than about the
  // code. What this scenario is for is the RSS column beside it.
  thresholds: {
    http_req_failed: ["rate<0.01"],
  },
  // The bodies are the point of the run and their *size* is checked, so they
  // cannot be discarded — but only one arm runs at a time and the VU count is
  // low, which is what keeps k6's own footprint out of the way of the server's.
  discardResponseBodies: false,
};

const URL = `${BASE_URL}/proxy/${ARM.registry}/${PLATFORM}/${ARM.doc}`;
const HEADERS = { Authorization: `Bearer ${ADMIN_TOKEN}` };

export function setup() {
  console.log(`arm=${ARM_NAME}  ${ARM.doc}  registry=${ARM.registry}`);
  const res = http.get(URL, { headers: HEADERS });
  if (res.status !== 200) {
    throw new Error(
      `${URL} answered ${res.status} — run 'task perf:conda:seed' first, and check ` +
        `that the mock upstream is serving ${ARM.doc}`,
    );
  }
  const mib = (res.body.length / 1048576).toFixed(1);
  console.log(`document is ${mib} MiB  (X-BatleHub-Cache: ${res.headers["X-Batlehub-Cache"]})`);
  return { bytes: res.body.length };
}

export default function conda_filter(data) {
  const res = http.get(URL, { headers: HEADERS });
  const ok = check(res, {
    "status 200": (r) => r.status === 200,
    // A truncated index is a 200 with a short body, and a filtered arm that
    // answers the unfiltered document is the failure this whole scenario would
    // otherwise report as a fast result.
    "body is the whole document": (r) => r.body.length >= data.bytes * 0.5,
  });

  // The expensive check — a substring scan over the whole document — runs once
  // per VU rather than per iteration. Once is enough to catch an arm that is
  // not doing what its name says; every iteration would put k6's scan time
  // inside the server's measurement.
  if (ok && __ITER === 0 && ARM.doc === "repodata.json") {
    const present = res.body.includes(BLOCKED_FILE);
    check(res, {
      "arm filters what it claims to": () => present !== ARM.filtered,
    });
  }
}
