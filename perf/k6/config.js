// Base URL of the BatleHub server under test.
export const BASE_URL = __ENV.BATLEHUB_URL || "http://localhost:8080";

// Static API token — must be configured in perf/config.perf.toml as:
//   [[auth.tokens]]
//   value = "perf-admin-token"
//   role  = "admin"
//   user_id = "perf-admin"
export const ADMIN_TOKEN = __ENV.BATLEHUB_TOKEN || "perf-admin-token";

// Registry names pre-created by the seed script.
export const NPM_REGISTRY   = "perf-npm";
export const CARGO_REGISTRY = "perf-cargo";
// The RubyGems registry the soak config declares (`perf/config.soak.toml`), for
// the scenario that wants a whole-registry document rather than a packument.
export const GEMS_REGISTRY = "perf-gems";
// One name per registry the soak declares, because a leak lives in a code path
// and not in a request count: every kind brings its own client, its own
// document parser and its own rewriter. `perf/config.soak.toml` declares them,
// `perf/k6/soak_arms.js` drives them, and `crates/web/tests/soak_kind_coverage.rs`
// refuses a kind that is in neither.
export const GO_REGISTRY = "perf-go";
export const MAVEN_REGISTRY = "perf-maven";
export const GENERIC_REGISTRY = "perf-generic";
export const LOCAL_NPM_REGISTRY = "perf-local-npm";
export const DEB_REGISTRY = "perf-deb";
export const RPM_REGISTRY = "perf-rpm";
export const PACMAN_REGISTRY = "perf-pacman";
export const APK_REGISTRY = "perf-apk";
export const JETBRAINS_REGISTRY = "perf-jetbrains";
export const PYPI_REGISTRY = "perf-pypi";
export const NUGET_REGISTRY = "perf-nuget";
export const COMPOSER_REGISTRY = "perf-composer";
export const CONDA_REGISTRY = "perf-conda";
export const NODE_REGISTRY = "perf-node";
export const RUSTUP_REGISTRY = "perf-rustup";
export const TERRAFORM_REGISTRY = "perf-terraform";
export const SDKMAN_REGISTRY = "perf-sdkman";
export const OPENVSX_REGISTRY = "perf-openvsx";
export const GITHUB_REGISTRY = "perf-github";
export const FORGEJO_REGISTRY = "perf-forgejo";
export const GITLAB_REGISTRY = "perf-gitlab";
export const VSCODE_REGISTRY = "perf-vscode";
export const JBMARKET_REGISTRY = "perf-jbmarket";

// The authority of the soak's mock upstream, as the proxy sees it. Terraform's
// network-mirror routes carry the mirrored *hostname* in the path and the
// handler checks it against the registry's own upstream, so the arm cannot
// hard-code `registry.terraform.io`: a mirror answering for a hostname it does
// not mirror would be lying about what it holds.
export const TERRAFORM_UPSTREAM_HOST =
  __ENV.BATLEHUB_SOAK_UPSTREAM_HOST || "127.0.0.1";

// Package baked into the seed data.
export const SEED_PKG = "perf-pkg";
export const SEED_VER = "1.0.0";
