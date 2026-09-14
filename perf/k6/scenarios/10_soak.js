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
import {
  ADMIN_TOKEN,
  BASE_URL,
  GEMS_REGISTRY,
  GENERIC_REGISTRY,
  GO_REGISTRY,
  MAVEN_REGISTRY,
  NPM_REGISTRY,
  SEED_PKG,
  SEED_VER,
} from "../config.js";
import { npmPublishPayload } from "../helpers.js";

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
const JSON_AUTH = { ...AUTH, "Content-Type": "application/json" };

const WARM_URL = `${BASE_URL}/proxy/${NPM_REGISTRY}/${SEED_PKG}/${SEED_VER}/tarball`;
const PACKUMENT_URL = `${BASE_URL}/proxy/${NPM_REGISTRY}/${SEED_PKG}`;
const SBOM_URL = `${BASE_URL}/api/v1/sbom/${NPM_REGISTRY}/${SEED_PKG}/${SEED_VER}?format=spdx`;
const PACKAGES_URL = `${BASE_URL}/api/v1/packages?registry=${NPM_REGISTRY}&limit=50`;
const GEMS_INDEX_URL = `${BASE_URL}/proxy/${GEMS_REGISTRY}/versions`;
const GEMS_INFO_URL = (gem) => `${BASE_URL}/proxy/${GEMS_REGISTRY}/info/${gem}`;

// Every request carries the registry it is against, because the report ranks
// registries and a request with no registry cannot be attributed. The server's
// own `/metrics` is what the ranking is actually computed from — these tags are
// the client-side check on it.
const npmTags = (op) => ({ op, registry: NPM_REGISTRY });

// The same deterministic rotation the mix uses, and the same reason every arm
// draws from a **bounded** set: a coordinate space that grows with the run adds
// a row and a stored object per request forever, and the verdict then measures
// the workload rather than the server (see `MISS_SPACE`).
const SLOT = () => (__VU * 7919 + __ITER) % 64;
const MODULE_SUFFIX = () => (SLOT() % 8 === 0 ? "" : `/v${(SLOT() % 8) + 1}`);
const ARTIFACT = () => `lib-${SLOT() % 12}`;

/**
 * The mix, as percentages of the offered rate. Deterministic rotation on the
 * iteration counter rather than `Math.random()`: two runs then issue the same
 * requests in the same order, and a difference between them is the server's.
 */
export function mixed() {
  const slot = (__VU * 7919 + __ITER) % 100;
  if (slot < 34) warmRead();
  else if (slot < 44) packument();
  else if (slot < 50) cacheMiss();
  else if (slot < 53) adminRead();
  else if (slot < 56) sbomRead();
  else if (slot < 59) publish();
  else if (slot < 62) gemsIndex();
  else if (slot < 72) gemsInfo();
  else if (slot < 78) goList();
  else if (slot < 86) goArtifact();
  else if (slot < 90) mavenMetadata();
  else if (slot < 95) mavenArtifact();
  else genericFile();
}

/** 50% — an artifact served from storage. The streaming path. */
function warmRead() {
  const res = http.get(WARM_URL, { headers: AUTH, tags: npmTags("warm_read") });
  check(res, { "warm read 200": (r) => r.status === 200 });
}

/** 14% — the listing document: parsed, rewritten and re-serialised per request. */
function packument() {
  const res = http.get(PACKUMENT_URL, {
    headers: AUTH,
    tags: npmTags("packument"),
  });
  check(res, { "packument 200": (r) => r.status === 200 });
}

/**
 * 8% — a coordinate this process has not served recently: upstream fetch,
 * cache write, metadata row.
 *
 * **Drawn from a bounded set**, which is the difference between a leak test and
 * a growth test. An unbounded miss space — a fresh version string per
 * iteration, which is what this did first — adds a cache entry, a metadata row
 * and a stored object *per request, forever*, so resident memory climbs for as
 * long as the run lasts and every long soak "fails". That growth is the
 * workload's, not the server's, and a gate cannot tell the two apart.
 *
 * A leak test needs a **stationary** workload: one whose steady state exists,
 * so that anything still climbing after the caches have filled is the process's
 * doing. The set is large enough to keep missing (it outlives the metadata TTL
 * and spills past what the artifact cache keeps) and finite, so it plateaus.
 */
const MISS_SPACE = Number(__ENV.BATLEHUB_SOAK_MISS_SPACE || 500);

function cacheMiss() {
  const n = (__VU * 7919 + __ITER) % MISS_SPACE;
  const pkg = `soak-miss-${n % 50}`;
  const version = `0.${Math.floor(n / 50)}.${n % 50}`;
  const res = http.get(
    `${BASE_URL}/proxy/${NPM_REGISTRY}/${pkg}/${version}/tarball`,
    { headers: AUTH, tags: npmTags("cache_miss") }
  );
  // A miss may legitimately 404 — what must not happen is a 5xx, which is the
  // shape an exhausted pool or a poisoned cache takes.
  check(res, { "miss not 5xx": (r) => r.status < 500 });
}

/** 4% — an authenticated API read: one pool connection, held briefly. */
function adminRead() {
  const res = http.get(PACKAGES_URL, {
    headers: AUTH,
    tags: npmTags("admin_read"),
  });
  check(res, { "packages 200": (r) => r.status === 200 });
}

/** 4% — a document built per request rather than served. */
function sbomRead() {
  const res = http.get(SBOM_URL, { headers: AUTH, tags: npmTags("sbom") });
  check(res, { "sbom not 5xx": (r) => r.status < 500 });
}

/**
 * 4% — the whole registry in one document.
 *
 * The shape that costs differently from everything else in this mix: a compact
 * index is every gem and every version, so it is megabytes where a packument is
 * kilobytes, and it is parsed and filtered per request unless it is cached.
 * A soak whose registries all had the same shape would rank them by traffic
 * rather than by cost, which tells an operator nothing they did not already
 * know from their own request counts.
 */
function gemsIndex() {
  const res = http.get(GEMS_INDEX_URL, {
    headers: AUTH,
    tags: { op: "gems_index", registry: GEMS_REGISTRY },
  });
  check(res, { "gems index not 5xx": (r) => r.status < 500 });
}

/** 13% — one gem's versions: the document Bundler reads per gem in the graph. */
function gemsInfo() {
  const gem = `perf-gem-${String((__VU * 31 + __ITER) % 1000).padStart(7, "0")}`;
  const res = http.get(GEMS_INFO_URL(gem), {
    headers: AUTH,
    tags: { op: "gems_info", registry: GEMS_REGISTRY },
  });
  check(res, { "gems info not 5xx": (r) => r.status < 500 });
}

// ── The kinds beyond npm ────────────────────────────────────────────────────
//
// Each of these is a different client, a different document parser and a
// different rewriter inside the proxy. They are here to put those paths under
// load, not for their share of traffic: the percentages only decide how often
// each arm runs.

/** 6% — the Go module version list: a small document, parsed and filtered. */
function goList() {
  const res = http.get(
    `${BASE_URL}/proxy/${GO_REGISTRY}/example.com/mod${MODULE_SUFFIX()}/@v/list`,
    { headers: AUTH, tags: { op: "go_list", registry: GO_REGISTRY } }
  );
  check(res, { "go list not 5xx": (r) => r.status < 500 });
}

/** 8% — a module zip: the streaming path for a kind that is not npm. */
function goArtifact() {
  const res = http.get(
    `${BASE_URL}/proxy/${GO_REGISTRY}/example.com/mod${MODULE_SUFFIX()}/@v/v1.2.0.zip`,
    { headers: AUTH, tags: { op: "go_zip", registry: GO_REGISTRY } }
  );
  check(res, { "go zip not 5xx": (r) => r.status < 500 });
}

/** 4% — `maven-metadata.xml`: an XML listing, which no other arm exercises. */
function mavenMetadata() {
  const res = http.get(
    `${BASE_URL}/proxy/${MAVEN_REGISTRY}/maven2/com/example/${ARTIFACT()}/maven-metadata.xml`,
    { headers: AUTH, tags: { op: "maven_metadata", registry: MAVEN_REGISTRY } }
  );
  check(res, { "maven metadata not 5xx": (r) => r.status < 500 });
}

/** 5% — a jar, which is the multi-file storage-key path. */
function mavenArtifact() {
  const a = ARTIFACT();
  const res = http.get(
    `${BASE_URL}/proxy/${MAVEN_REGISTRY}/maven2/com/example/${a}/1.2.0/${a}-1.2.0.jar`,
    { headers: AUTH, tags: { op: "maven_jar", registry: MAVEN_REGISTRY } }
  );
  check(res, { "maven jar not 5xx": (r) => r.status < 500 });
}

/** 5% — the kind whose protocol is "a URL is a file", allowlist and all. */
function genericFile() {
  const res = http.get(
    `${BASE_URL}/proxy/${GENERIC_REGISTRY}/generic/dist/tool-${SLOT() % 20}.tar.gz`,
    { headers: AUTH, tags: { op: "generic_file", registry: GENERIC_REGISTRY } }
  );
  check(res, { "generic not 5xx": (r) => r.status < 500 });
}

/**
 * 3% — the write path: a body held in memory, a row, an object in storage.
 *
 * **Bounded, for the reason the cache-miss arm is bounded**, and this one was
 * missed the first time: `1.0.${__ITER}` is a brand-new package version on
 * every publish, so the workload adds a row, a stored object and a dedup entry
 * *per request, forever*. Measured on the first real 10-minute run — about
 * 1 800 new versions — and the verdict failed on an RSS trend of 2.21 MiB/min
 * against a 2.00 limit, which is what a slow workload-driven drift looks like
 * and is indistinguishable from a slow leak at that margin.
 *
 * After the first pass over this space the publishes are duplicate-coordinate
 * refusals (`409`, `immutable`), which is a weaker exercise than a fresh write
 * — but it still runs auth, the body parse, the quota check and the existence
 * check on every request, and it is the only version of this arm whose steady
 * state exists. A gate cannot tell a leak from a workload that never stops
 * growing, so the workload is the thing that has to stop growing.
 */
const PUBLISH_SPACE = Number(__ENV.BATLEHUB_SOAK_PUBLISH_SPACE || 100);

function publish() {
  const n = (__VU * 7919 + __ITER) % PUBLISH_SPACE;
  const name = `soak-pub-${n % 10}`;
  const version = `1.0.${Math.floor(n / 10)}`;
  const res = http.put(
    `${BASE_URL}/proxy/perf-local-npm/${name}`,
    npmPublishPayload(name, version, 32),
    { headers: JSON_AUTH, tags: { op: "publish", registry: "perf-local-npm" } }
  );
  check(res, { "publish not 5xx": (r) => r.status < 500 });
}
