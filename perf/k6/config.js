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

// Package baked into the seed data.
export const SEED_PKG = "perf-pkg";
export const SEED_VER = "1.0.0";
