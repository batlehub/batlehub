/**
 * The soak's mix, as data: one entry per request shape, for every registry kind.
 *
 * **Why this is a table and not a function per arm.** The soak's value is that
 * it drives every code path a leak could live in, and that claim is only worth
 * anything if it is checkable. Three things read this list:
 *
 *   - `scenarios/10_soak.js` — the load itself, which picks an arm by weight;
 *   - `scenarios/11_soak_arms.js` — one request per arm, each asserted against
 *     its `expect`. `perf/scripts/soak.sh` runs it *before* the warm-up, so an
 *     arm that silently 404s fails the run instead of contributing a zero to
 *     the report. This is not hypothetical: the load's own check is
 *     "not 5xx", which a 404 passes;
 *   - `crates/web/tests/soak_kind_coverage.rs` — which refuses a registry kind
 *     that is neither named here nor given a written reason.
 *
 * Each arm is `{op, kind, registry, weight, expect, request(n)}`:
 *
 *   - `kind`     the registry kind, as `RegistryKind::as_str` spells it. This
 *                is what the coverage test counts, so it is not decoration.
 *   - `weight`   relative share of the offered rate. Not a percentage: the mix
 *                divides by the total, so adding an arm does not mean rewriting
 *                every other number.
 *   - `expect`   the statuses that mean "this arm works" for the pre-flight.
 *                Mostly `[200]`; the miss arm is allowed its `404`.
 *   - `request`  `(n) => {url, method?, body?, headers?}`, where `n` is a
 *                rotation index bounded by the arm's own coordinate space.
 *
 * **Every coordinate space is bounded, and that is load-bearing.** A space that
 * grows with the run adds a cache entry, a metadata row and a stored object per
 * request forever, so resident memory climbs for as long as the run lasts and
 * every long soak "fails" — growth that is the workload's, not the server's,
 * and that a gate cannot tell from a leak.
 *
 * A leak test needs a **stationary** workload: one whose steady state exists,
 * so that anything still climbing after the caches have filled is the process's
 * doing. The two spaces that were found the hard way carry their measurement
 * below; the rest are small enough to plateau within the warm-up.
 */
import {
  APK_REGISTRY,
  BASE_URL,
  CARGO_REGISTRY,
  COMPOSER_REGISTRY,
  CONDA_REGISTRY,
  DEB_REGISTRY,
  DEVFILE_REGISTRY,
  FORGEJO_REGISTRY,
  GALAXY_REGISTRY,
  GEMS_REGISTRY,
  GENERIC_REGISTRY,
  GITHUB_REGISTRY,
  GITLAB_REGISTRY,
  GO_REGISTRY,
  JBMARKET_REGISTRY,
  JETBRAINS_REGISTRY,
  LOCAL_NPM_REGISTRY,
  MAVEN_REGISTRY,
  NIX_REGISTRY,
  NODE_REGISTRY,
  NPM_REGISTRY,
  NUGET_REGISTRY,
  OPENVSX_REGISTRY,
  PACMAN_REGISTRY,
  PYPI_REGISTRY,
  RPM_REGISTRY,
  RUSTUP_REGISTRY,
  SDKMAN_REGISTRY,
  SEED_PKG,
  SEED_VER,
  TERRAFORM_REGISTRY,
  TERRAFORM_UPSTREAM_HOST,
  VSCODE_REGISTRY,
} from "./config.js";
import { npmPublishPayload } from "./helpers.js";

const p = (path) => `${BASE_URL}${path}`;

/// A well-formed store hash for arm index `n`.
///
/// 32 characters of **Nix's** base32 alphabet, which omits `e`, `o`, `u` and
/// `t`. The proxy validates this at the edge and answers `400` for anything
/// else, so a hash built from a plain hex or base64 alphabet would make these
/// arms measure the validator instead of the document path.
const NIX32 = "0123456789abcdfghijklmnpqrsvwxyz";
const nixHash = (n) => {
  let out = "";
  let v = n + 1;
  for (let i = 0; i < 32; i++) {
    out += NIX32[v % 32];
    v = Math.floor(v / 32) + i;
  }
  return out;
};

const goModule = (n) => {
  const majorVersionPath = n === 0 ? "" : `/v${n + 1}`;
  return `example.com/mod${majorVersionPath}`;
};

const cargoCrateName = (n) => `perf-crate-${n}`;

/**
 * The cache-miss arm's coordinate space.
 *
 * Large enough to keep missing — it outlives the metadata TTL and spills past
 * what the artifact cache keeps — and finite, so it plateaus. An unbounded miss
 * space, which is what this did first, adds a cache entry, a metadata row and a
 * stored object per request forever.
 */
const MISS_SPACE = Number(__ENV.BATLEHUB_SOAK_MISS_SPACE || 500);

/**
 * The publish arm's space, and this one was missed the first time.
 *
 * `1.0.${__ITER}` is a brand-new package version on every publish. Measured on
 * the first real 10-minute run — about 1 800 new versions — and the verdict
 * failed on an RSS trend of 2.21 MiB/min against a 2.00 limit, which is what a
 * slow workload-driven drift looks like and is indistinguishable from a slow
 * leak at that margin.
 *
 * After the first pass over this space the publishes are duplicate-coordinate
 * refusals (`409`, `immutable`) — a weaker exercise than a fresh write, but the
 * only version of this arm whose steady state exists, and it still runs auth,
 * the body parse, the quota check and the existence check on every request.
 */
const PUBLISH_SPACE = Number(__ENV.BATLEHUB_SOAK_PUBLISH_SPACE || 100);

/** The three versions every mock protocol publishes, so an arm can rotate. */
const VERSIONS = ["1.0.0", "1.1.0", "1.2.0"];
const version = (n) => VERSIONS[n % VERSIONS.length];

/**
 * Cargo's sparse-index layout, which the proxy reads back out of the request
 * path: `1/a`, `2/ab`, `3/a/abc`, then `ab/cd/abcdef`.
 */
function sparseIndexPath(name) {
  const lower = name.toLowerCase();
  if (lower.length === 1) return `1/${lower}`;
  if (lower.length === 2) return `2/${lower}`;
  if (lower.length === 3) return `3/${lower[0]}/${lower}`;
  return `${lower.slice(0, 2)}/${lower.slice(2, 4)}/${lower}`;
}

export const ARMS = [
  // ── npm: the shapes the mix was built around ──────────────────────────────
  {
    op: "npm_warm_read",
    kind: "npm",
    registry: NPM_REGISTRY,
    weight: 34,
    expect: [200],
    doc: "an artifact served from storage — the streaming path",
    request: () => ({ url: p(`/proxy/${NPM_REGISTRY}/${SEED_PKG}/${SEED_VER}/tarball`) }),
  },
  {
    op: "npm_packument",
    kind: "npm",
    registry: NPM_REGISTRY,
    weight: 10,
    expect: [200],
    doc: "the listing document: parsed, rewritten and re-serialised per request",
    request: () => ({ url: p(`/proxy/${NPM_REGISTRY}/${SEED_PKG}`) }),
  },
  {
    op: "npm_cache_miss",
    kind: "npm",
    registry: NPM_REGISTRY,
    weight: 6,
    // A miss may legitimately 404 — a name that does not exist, answered
    // correctly. What must not happen is a 5xx, the shape an exhausted pool or
    // a poisoned cache takes.
    expect: [200, 404],
    space: MISS_SPACE,
    doc: "a coordinate not served recently: upstream fetch, cache write, metadata row",
    request: (n) => ({
      url: p(
        `/proxy/${NPM_REGISTRY}/soak-miss-${n % 50}/0.${Math.floor(n / 50)}.${n % 50}/tarball`,
      ),
    }),
  },
  {
    op: "npm_sbom",
    kind: "npm",
    registry: NPM_REGISTRY,
    weight: 3,
    expect: [200],
    doc: "a document built per request rather than served",
    request: () =>
      ({ url: p(`/api/v1/sbom/${NPM_REGISTRY}/${SEED_PKG}/${SEED_VER}?format=spdx`) }),
  },
  {
    op: "admin_read",
    kind: "npm",
    registry: NPM_REGISTRY,
    weight: 3,
    expect: [200],
    doc: "an authenticated API read: one pool connection, held briefly",
    request: () => ({ url: p(`/api/v1/packages?registry=${NPM_REGISTRY}&limit=50`) }),
  },
  {
    op: "npm_publish",
    kind: "npm",
    registry: LOCAL_NPM_REGISTRY,
    weight: 3,
    expect: [200, 201, 409],
    space: PUBLISH_SPACE,
    doc: "the write path: a body held in memory, a row, an object in storage",
    request: (n) => {
      const name = `soak-pub-${n % 10}`;
      return {
        method: "PUT",
        url: p(`/proxy/${LOCAL_NPM_REGISTRY}/${name}`),
        body: npmPublishPayload(name, `1.0.${Math.floor(n / 10)}`, 32),
        headers: { "Content-Type": "application/json" },
      };
    },
  },

  // ── RubyGems: the whole registry in one document ──────────────────────────
  {
    op: "gems_index",
    kind: "rubygems",
    registry: GEMS_REGISTRY,
    weight: 3,
    expect: [200],
    doc: "a compact index is every gem and every version — megabytes where a packument is kilobytes",
    request: () => ({ url: p(`/proxy/${GEMS_REGISTRY}/versions`) }),
  },
  {
    op: "gems_info",
    kind: "rubygems",
    registry: GEMS_REGISTRY,
    weight: 10,
    expect: [200],
    space: 1000,
    doc: "one gem's versions: the document Bundler reads per gem in the graph",
    request: (n) => ({
      url: p(`/proxy/${GEMS_REGISTRY}/info/perf-gem-${String(n % 1000).padStart(7, "0")}`),
    }),
  },

  // ── Go modules ────────────────────────────────────────────────────────────
  {
    op: "go_list",
    kind: "goproxy",
    registry: GO_REGISTRY,
    weight: 6,
    expect: [200],
    space: 8,
    doc: "the module version list: a small document, parsed and filtered",
    request: (n) => ({
      url: p(`/proxy/${GO_REGISTRY}/${goModule(n)}/@v/list`),
    }),
  },
  {
    op: "go_zip",
    kind: "goproxy",
    registry: GO_REGISTRY,
    weight: 8,
    expect: [200],
    space: 8,
    doc: "a module zip: the streaming path for a kind that is not npm",
    request: (n) => ({
      url: p(
        `/proxy/${GO_REGISTRY}/${goModule(n)}/@v/v1.2.0.zip`,
      ),
    }),
  },

  // ── Maven ─────────────────────────────────────────────────────────────────
  {
    op: "maven_metadata",
    kind: "maven",
    registry: MAVEN_REGISTRY,
    weight: 4,
    expect: [200],
    space: 12,
    doc: "an XML listing, which no other arm exercises",
    request: (n) => ({
      url: p(`/proxy/${MAVEN_REGISTRY}/maven2/com/example/lib-${n}/maven-metadata.xml`),
    }),
  },
  {
    op: "maven_jar",
    kind: "maven",
    registry: MAVEN_REGISTRY,
    weight: 5,
    expect: [200],
    space: 12,
    doc: "a jar, which is the multi-file storage-key path",
    request: (n) => ({
      url: p(
        `/proxy/${MAVEN_REGISTRY}/maven2/com/example/lib-${n}/1.2.0/lib-${n}-1.2.0.jar`,
      ),
    }),
  },

  // ── The five path-shaped kinds: one client, five types ────────────────────
  {
    op: "generic_file",
    kind: "generic",
    registry: GENERIC_REGISTRY,
    weight: 5,
    expect: [200],
    space: 20,
    doc: "the kind whose protocol is 'a URL is a file', allowlist and all",
    request: (n) => ({ url: p(`/proxy/${GENERIC_REGISTRY}/generic/dist/tool-${n}.tar.gz`) }),
  },
  {
    op: "deb_pool_file",
    kind: "deb",
    registry: DEB_REGISTRY,
    weight: 2,
    expect: [200],
    space: 16,
    doc: "a pool file by path — the index files are fetched the same way",
    request: (n) => ({
      url: p(`/proxy/${DEB_REGISTRY}/deb/pool/main/d/demo/demo_1.${n}_amd64.deb`),
    }),
  },
  {
    op: "rpm_package",
    kind: "rpm",
    registry: RPM_REGISTRY,
    weight: 2,
    expect: [200],
    space: 16,
    doc: "an .rpm by path",
    request: (n) => ({ url: p(`/proxy/${RPM_REGISTRY}/rpm/Packages/demo-1.${n}.x86_64.rpm`) }),
  },
  {
    op: "pacman_package",
    kind: "pacman",
    registry: PACMAN_REGISTRY,
    weight: 2,
    expect: [200],
    space: 16,
    doc: "a .pkg.tar.zst by path",
    request: (n) => ({
      url: p(`/proxy/${PACMAN_REGISTRY}/pacman/demo-1.${n}-1-x86_64.pkg.tar.zst`),
    }),
  },
  // Two arms, where the other three OS kinds get one each. `apk` is the only
  // path kind that is not a pure byte path: a `.apk` request carries a real
  // coordinate, so it pays a split, a `resolve_metadata` that reads the cached
  // index, and the whole rule chain. Whether that is free in the steady state
  // is a measurement, and these are where it is taken.
  {
    op: "apk_index",
    kind: "apk",
    registry: APK_REGISTRY,
    weight: 2,
    expect: [200],
    space: 1,
    doc: "APKINDEX.tar.gz — relayed byte-exact, on the hot path of every `apk update`",
    request: () => ({
      url: p(`/proxy/${APK_REGISTRY}/apk/v3.22/main/x86_64/APKINDEX.tar.gz`),
    }),
  },
  {
    op: "apk_package",
    kind: "apk",
    registry: APK_REGISTRY,
    weight: 2,
    expect: [200],
    space: 16,
    doc: "a .apk — the coordinate split, the index read for its build date, and the rules",
    request: (n) => ({
      url: p(`/proxy/${APK_REGISTRY}/apk/v3.22/main/x86_64/demo-1.${n}-r0.apk`),
    }),
  },
  // Ansible Galaxy: four documents on the way to one artifact, and the listing
  // is the expensive one — the proxy walks upstream's 100-entry pages and
  // assembles them into the single page it serves, so the cost of that walk is
  // on this arm and nowhere else. The collection document is its own arm
  // because `ansible-galaxy` re-reads it *uncached* on every resolve, which
  // makes it the hottest of the four in a real estate.
  {
    op: "galaxy_versions",
    kind: "galaxy",
    registry: GALAXY_REGISTRY,
    weight: 2,
    expect: [200],
    space: 4,
    doc: "the versions list — three upstream pages assembled into one served document",
    request: (n) => ({
      url: p(`/proxy/${GALAXY_REGISTRY}/galaxy/api/v3/collections/acme/util${n}/versions/`),
    }),
  },
  {
    op: "galaxy_collection",
    kind: "galaxy",
    registry: GALAXY_REGISTRY,
    weight: 2,
    expect: [200],
    space: 4,
    doc: "the collection document — re-read on every resolve, and repaired against the listing",
    request: (n) => ({
      url: p(`/proxy/${GALAXY_REGISTRY}/galaxy/api/v3/collections/acme/util${n}/`),
    }),
  },
  {
    op: "galaxy_artifact",
    kind: "galaxy",
    registry: GALAXY_REGISTRY,
    weight: 2,
    expect: [200],
    space: 16,
    doc: "a collection tarball — the coordinate parsed back out of the filename",
    request: (n) => ({
      url: p(
        `/proxy/${GALAXY_REGISTRY}/galaxy/api/v3/artifacts/collections/acme-util0-0.${n}.0.tar.gz`,
      ),
    }),
  },
  // A Nix binary cache. The narinfo arm is the one that matters: it is fetched
  // once per store path in a closure — hundreds per system build, which makes
  // it the hottest document in this whole file in a real estate — and the proxy
  // parses it, blocks on it, rewrites one line, re-serialises it and writes a
  // reverse-index entry, all per request. Every one of those steps allocates.
  //
  // `space: 64` and not more: the reverse index writes one cache entry per
  // *distinct* NAR basename, so an unbounded hash space would grow the cache
  // for as long as the run lasts and the gate would fail on its own load rather
  // than on a leak (the stationary-workload rule this file opens with).
  {
    op: "nix_narinfo",
    kind: "nix",
    registry: NIX_REGISTRY,
    weight: 3,
    expect: [200],
    space: 64,
    doc: "a narinfo — parsed, blocked on, one line rewritten, re-serialised and indexed",
    request: (n) => ({
      url: p(`/proxy/${NIX_REGISTRY}/nix/${nixHash(n)}.narinfo`),
    }),
  },
  {
    op: "nix_nar",
    kind: "nix",
    registry: NIX_REGISTRY,
    weight: 2,
    expect: [200],
    space: 64,
    doc: "a NAR under the rewritten coordinate — the store hash re-read from the narinfo",
    request: (n) => ({
      url: p(
        `/proxy/${NIX_REGISTRY}/nix/nar/${nixHash(n)}/${nixHash(n)}.nar.zst`,
      ),
    }),
  },
  {
    op: "nix_cache_info",
    kind: "nix",
    registry: NIX_REGISTRY,
    weight: 1,
    expect: [200],
    space: 1,
    doc: "nix-cache-info — registry-wide, one cache entry, read once per substituter",
    request: () => ({
      url: p(`/proxy/${NIX_REGISTRY}/nix/nix-cache-info`),
    }),
  },
  {
    op: "jetbrains_archive",
    kind: "jetbrains",
    registry: JETBRAINS_REGISTRY,
    weight: 2,
    expect: [200],
    space: 8,
    doc: "an IDE archive on the download CDN",
    request: (n) => ({ url: p(`/proxy/${JETBRAINS_REGISTRY}/jetbrains/idea/idea-2026.${n}.tar.gz`) }),
  },

  // ── Cargo ─────────────────────────────────────────────────────────────────
  {
    op: "cargo_index",
    kind: "cargo",
    registry: CARGO_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "the sparse index entry: newline-delimited JSON, filtered for blocked versions",
    request: (n) => ({
      url: p(`/proxy/${CARGO_REGISTRY}/registry/${sparseIndexPath(cargoCrateName(n))}`),
    }),
  },
  {
    op: "cargo_download",
    kind: "cargo",
    registry: CARGO_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "a .crate file",
    request: (n) => ({
      url: p(`/proxy/${CARGO_REGISTRY}/perf-crate-${n}/${version(n)}/download`),
    }),
  },

  // ── PyPI ──────────────────────────────────────────────────────────────────
  {
    op: "pypi_simple",
    kind: "pypi",
    registry: PYPI_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "the PEP 503 page, every href rewritten to point back at the registry",
    request: (n) => ({ url: p(`/proxy/${PYPI_REGISTRY}/simple/perf_dist_${n}/`) }),
  },
  {
    op: "pypi_wheel",
    kind: "pypi",
    registry: PYPI_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "the wheel the page linked to, verified against the page's sha256",
    request: (n) => ({
      // `perf_dist_0`, not `perf-dist-0`: a wheel filename is
      // `{name}-{version}-{python}-{abi}-{platform}.whl` and the name may not
      // contain `-`, so a hyphen there is parsed as the version boundary. The
      // pre-flight caught this as `perf-dist/0`, a package at version "0".
      url: p(
        `/proxy/${PYPI_REGISTRY}/packages/perf_dist_${n}-${version(n)}-py3-none-any.whl`,
      ),
    }),
  },

  // ── NuGet ─────────────────────────────────────────────────────────────────
  {
    op: "nuget_flat_index",
    kind: "nuget",
    registry: NUGET_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "the flat container's version list",
    request: (n) => ({
      url: p(`/proxy/${NUGET_REGISTRY}/nuget/v3/flat/perf.pkg${n}/index.json`),
    }),
  },
  {
    op: "nuget_nupkg",
    kind: "nuget",
    registry: NUGET_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "a .nupkg — the path that builds its storage key at the edge",
    request: (n) => {
      const v = version(n);
      return {
        url: p(
          `/proxy/${NUGET_REGISTRY}/nuget/v3/flat/perf.pkg${n}/${v}/perf.pkg${n}.${v}.nupkg`,
        ),
      };
    },
  },

  // ── Composer ──────────────────────────────────────────────────────────────
  {
    op: "composer_root",
    kind: "composer",
    registry: COMPOSER_REGISTRY,
    weight: 2,
    expect: [200],
    doc: "packages.json: the root document, rewritten to point p2 at this registry",
    request: () => ({ url: p(`/proxy/${COMPOSER_REGISTRY}/packages.json`) }),
  },
  {
    op: "composer_p2",
    kind: "composer",
    registry: COMPOSER_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "one package's versions, every dist.url rewritten",
    request: (n) => ({ url: p(`/proxy/${COMPOSER_REGISTRY}/p2/acme/demo-${n}.json`) }),
  },

  // ── Conda ─────────────────────────────────────────────────────────────────
  {
    op: "conda_repodata",
    kind: "conda",
    registry: CONDA_REGISTRY,
    weight: 2,
    expect: [200],
    doc: "repodata.json: fetched, parsed, filtered and rewritten per request",
    request: () => ({ url: p(`/proxy/${CONDA_REGISTRY}/linux-64/repodata.json`) }),
  },
  {
    op: "conda_package",
    kind: "conda",
    registry: CONDA_REGISTRY,
    weight: 2,
    expect: [200],
    space: 200,
    doc: "a .conda, looked up in repodata and verified against its sha256",
    request: (n) => ({
      url: p(
        `/proxy/${CONDA_REGISTRY}/linux-64/perf-conda-${String(n).padStart(4, "0")}-1.0.0-py311_0.conda`,
      ),
    }),
  },

  // ── Devfile registries (RFC 0035) ─────────────────────────────────────────
  // The v2 index is registry-wide and filtered on every read; the manifest is
  // an artifact of its stack version, fetched and verified against its own
  // digest; the REST devfile resolves the version from the filtered index and
  // then reads the layer by the digest the manifest names. `space: 16` is the
  // mock's four stacks of four versions (`protocols/devfile.rs`).
  {
    op: "devfile_index",
    kind: "devfile",
    registry: DEVFILE_REGISTRY,
    weight: 2,
    expect: [200],
    doc: "the v2 stack index, filtered registry-wide",
    request: () => ({ url: p(`/proxy/${DEVFILE_REGISTRY}/v2index`) }),
  },
  {
    op: "devfile_manifest",
    kind: "devfile",
    registry: DEVFILE_REGISTRY,
    weight: 2,
    expect: [200],
    space: 16,
    doc: "a stack version's OCI manifest by tag, byte-exact with its digest",
    request: (n) => ({
      url: p(
        `/proxy/${DEVFILE_REGISTRY}/v2/devfile-catalog/acme${n % 4}/manifests/1.${Math.floor(n / 4)}.0`,
      ),
    }),
  },
  {
    op: "devfile_devfile",
    kind: "devfile",
    registry: DEVFILE_REGISTRY,
    weight: 2,
    expect: [200],
    space: 16,
    doc: "a stack version's devfile.yaml — the layer, found through the manifest",
    request: (n) => ({
      url: p(`/proxy/${DEVFILE_REGISTRY}/devfiles/acme${n % 4}/1.${Math.floor(n / 4)}.0`),
    }),
  },

  // ── Node.js distributions ─────────────────────────────────────────────────
  {
    op: "node_index",
    kind: "nodedist",
    registry: NODE_REGISTRY,
    weight: 2,
    expect: [200],
    doc: "index.json: the listing fnm and mise read, filtered",
    request: () => ({ url: p(`/proxy/${NODE_REGISTRY}/nodedist/index.json`) }),
  },
  {
    op: "node_tarball",
    kind: "nodedist",
    registry: NODE_REGISTRY,
    weight: 2,
    expect: [200],
    doc: "a release tarball",
    request: () => ({
      url: p(`/proxy/${NODE_REGISTRY}/nodedist/v22.11.0/node-v22.11.0-linux-x64.tar.gz`),
    }),
  },

  // ── The Rust release tree ─────────────────────────────────────────────────
  {
    op: "rustup_component",
    kind: "rustup",
    registry: RUSTUP_REGISTRY,
    weight: 2,
    expect: [200],
    // Eight, and `perf/mock-upstream/src/protocols/rustup.rs` publishes eight —
    // a version the mock does not list in `manifests.txt` resolves to no dated
    // directory and answers 404, which `status < 500` passes. Change one and
    // change the other.
    space: 8,
    doc: "a dated dist component",
    request: (n) => ({
      url: p(
        `/proxy/${RUSTUP_REGISTRY}/rustup/dist/2024-01-01/rust-std-1.9${n}.0-x86_64-unknown-linux-gnu.tar.gz`,
      ),
    }),
  },

  // ── Terraform ─────────────────────────────────────────────────────────────
  //
  // No discovery arm: `.well-known/terraform.json` is served only on a host
  // bound to a single registry (RFC 0001), and answers `404` with that
  // explanation under `/proxy/{registry}/`. Driving it would mean giving this
  // registry a vanity host and sending a `Host` header, which measures the
  // host router rather than the Terraform path.
  {
    op: "terraform_provider_versions",
    kind: "terraform",
    registry: TERRAFORM_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "a provider's version list: the document the mirror index is built from",
    request: (n) => ({
      url: p(`/proxy/${TERRAFORM_REGISTRY}/v1/providers/acme/demo${n}/versions`),
    }),
  },
  {
    op: "terraform_mirror_index",
    kind: "terraform",
    registry: TERRAFORM_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    // The hostname segment is checked against the registry's own upstream host,
    // so it is the mock's authority and not `registry.terraform.io`: a mirror
    // that answered for any hostname would be mirroring something it is not.
    doc: "the provider network mirror's version index",
    request: (n) => ({
      url: p(`/proxy/${TERRAFORM_REGISTRY}/${TERRAFORM_UPSTREAM_HOST}/acme/demo${n}/index.json`),
    }),
  },

  // ── SDKMAN ────────────────────────────────────────────────────────────────
  {
    op: "sdkman_versions",
    kind: "sdkman",
    registry: SDKMAN_REGISTRY,
    weight: 2,
    expect: [200],
    doc: "the candidate's version list, filtered — the API half of the protocol",
    request: () => ({
      url: p(`/proxy/${SDKMAN_REGISTRY}/sdkman/candidates/java/linuxx64/versions/all`),
    }),
  },
  {
    op: "sdkman_download",
    kind: "sdkman",
    registry: SDKMAN_REGISTRY,
    weight: 2,
    expect: [200],
    doc: "the broker half: a second upstream host, and the archive",
    request: () => ({
      url: p(`/proxy/${SDKMAN_REGISTRY}/sdkman/broker/download/java/21.0.1-tem/linuxx64`),
    }),
  },

  // ── Open VSX ──────────────────────────────────────────────────────────────
  {
    op: "openvsx_extension",
    kind: "openvsx",
    registry: OPENVSX_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "the extension document, every files.* URL rewritten",
    request: (n) => ({ url: p(`/proxy/${OPENVSX_REGISTRY}/api/acme/demo${n}`) }),
  },
  {
    op: "openvsx_vsix",
    kind: "openvsx",
    registry: OPENVSX_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "the VSIX itself",
    request: (n) => ({
      url: p(`/proxy/${OPENVSX_REGISTRY}/acme.demo${n}/${version(n)}/vsix`),
    }),
  },

  // ── The three forges ──────────────────────────────────────────────────────
  {
    op: "github_releases",
    kind: "github",
    registry: GITHUB_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "the release list, with administratively blocked releases removed",
    request: (n) => ({ url: p(`/proxy/${GITHUB_REGISTRY}/acme/demo${n}/releases`) }),
  },
  {
    op: "github_asset",
    kind: "github",
    registry: GITHUB_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "a release asset by tag and filename",
    request: (n) => ({
      url: p(
        `/proxy/${GITHUB_REGISTRY}/acme/demo${n}/releases/download/v1.2.0/demo${n}-v1.2.0.tar.gz`,
      ),
    }),
  },
  {
    op: "forgejo_releases",
    kind: "forgejo",
    registry: FORGEJO_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "the same document through the other forge's spelling",
    request: (n) => ({ url: p(`/proxy/${FORGEJO_REGISTRY}/acme/demo${n}/releases`) }),
  },
  {
    op: "forgejo_asset",
    kind: "forgejo",
    registry: FORGEJO_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "an asset — which on Forgejo is always /attachments/{uuid}, not the API host",
    request: (n) => ({
      url: p(
        `/proxy/${FORGEJO_REGISTRY}/acme/demo${n}/releases/download/v1.2.0/demo${n}-v1.2.0.tar.gz`,
      ),
    }),
  },
  {
    op: "gitlab_releases",
    kind: "gitlab",
    registry: GITLAB_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "GitLab's release document: assets.links rather than assets",
    request: (n) => ({ url: p(`/proxy/${GITLAB_REGISTRY}/acme/demo${n}/-/releases`) }),
  },
  {
    op: "gitlab_asset",
    kind: "gitlab",
    registry: GITLAB_REGISTRY,
    weight: 2,
    expect: [200],
    space: 12,
    doc: "a release link's target",
    request: (n) => ({
      url: p(
        `/proxy/${GITLAB_REGISTRY}/acme/demo${n}/-/releases/v1.2.0/downloads/asset-v1.2.0.tar.gz`,
      ),
    }),
  },

  // ── The two editor marketplaces ───────────────────────────────────────────
  //
  // Neither is addressed like a package registry, so each brings a parser and a
  // rewriter nothing else in this mix exercises: VS Code answers a POSTed query
  // document, JetBrains keys everything on a numeric plugin id.
  {
    op: "vscode_extension",
    kind: "vscode-marketplace",
    registry: VSCODE_REGISTRY,
    weight: 2,
    expect: [200],
    // Not `/vscode/item`: that route is a `302` to the console's own package
    // page and never touches the gallery. This one POSTs the query document
    // upstream, parses the result and rewrites every asset URL — which is the
    // work a soak is looking for.
    doc: "the gallery query, rewritten so every asset URL comes back through this registry",
    request: () => ({ url: p(`/proxy/${VSCODE_REGISTRY}/api/perf/demo`) }),
  },
  {
    op: "vscode_vspackage",
    kind: "vscode-marketplace",
    registry: VSCODE_REGISTRY,
    weight: 2,
    expect: [200],
    doc: "the VSIX, which lives on a different host from the query endpoint",
    request: () => ({
      url: p(
        `/proxy/${VSCODE_REGISTRY}/vscode/gallery/publishers/perf/vsextensions/demo/1.2.0/vspackage`,
      ),
    }),
  },
  {
    op: "jetbrains_plugin",
    kind: "jetbrains-marketplace",
    registry: JBMARKET_REGISTRY,
    weight: 2,
    expect: [200],
    // `{id}` here is the **xmlId**, not the numeric marketplace id: this route
    // is the published coordinate's spelling and resolves through
    // `/plugins/list?pluginId=…`, the plugin-repository XML. The numeric ids
    // belong to the IDE's own `/files/{pluginId}/{updateId}/…` paths.
    doc: "a plugin document, resolved through the plugin-repository XML",
    request: () => ({ url: p(`/proxy/${JBMARKET_REGISTRY}/api/plugins/com.perf.demo`) }),
  },
  {
    op: "jetbrains_updates",
    kind: "jetbrains-marketplace",
    registry: JBMARKET_REGISTRY,
    weight: 2,
    expect: [200],
    doc: "that plugin's versions, filtered",
    request: () => ({ url: p(`/proxy/${JBMARKET_REGISTRY}/api/plugins/com.perf.demo/updates`) }),
  },
];

/** Every registry kind an arm drives, deduplicated. */
export const SOAKED_KINDS = [...new Set(ARMS.map((a) => a.kind))].sort((a, b) =>
  a.localeCompare(b),
);

/** The total of every weight — the mix divides by this rather than by 100. */
export const TOTAL_WEIGHT = ARMS.reduce((sum, a) => sum + a.weight, 0);

/**
 * The arm a rotation slot selects, by cumulative weight.
 *
 * Deterministic on the slot rather than `Math.random()`: two runs of the same
 * duration then issue the same requests in the same proportions, and a
 * difference between their resource curves is the server's.
 */
export function armForSlot(slot) {
  let acc = 0;
  for (const arm of ARMS) {
    acc += arm.weight;
    if (slot < acc) return arm;
  }
  return ARMS.at(-1);
}
