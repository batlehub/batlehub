// The config generator's harness: `node --test docs/build/config-generator.test.ts`
// (types stripped by Node itself, no toolchain). It renders each scenario of
// config-generator-scenarios.ts through the same function the page uses and
// asserts on the TOML; the fixtures the Rust side loads are written by
// config-generator-fixtures.ts from the same scenarios.
import assert from "node:assert/strict";
import { test } from "node:test";

import { readFileSync } from "node:fs";

import {
  ALL_VERBS,
  CONFIG_VERSION,
  ECOSYSTEM_VERBS,
  addVerb,
  blankAuthProvider,
  defaultRegistry,
  defaultState,
  permsToToml,
  removeVerb,
  renderConfigToml,
  subjectSuggestions,
  verbOptions,
  verbsOutOfPlace,
} from "../.vitepress/components/configToml.ts";
import { scenarios } from "./config-generator-scenarios.ts";

const byName = Object.fromEntries(scenarios().map((s) => [s.name, s.state]));
const render = (name: string) => renderConfigToml(byName[name]);
const section = (toml: string, header: string) => {
  const lines = toml.split("\n");
  const i = lines.indexOf(header);
  if (i < 0) return null;
  const end = lines.findIndex((l, j) => j > i && l.startsWith("["));
  return lines.slice(i + 1, end < 0 ? undefined : end).filter(Boolean);
};

test("the default state renders a config with the version, one npm registry and its rbac", () => {
  const toml = render("default");
  assert.equal(toml.split("\n")[0], `config_version = ${CONFIG_VERSION}`);
  assert.ok(toml.includes("[database]"));
  assert.deepEqual(section(toml, "[[registries]]"), [
    'type = "npm"',
    'name = "npm"',
    'upstreams = ["https://registry.npmjs.org"]',
  ]);
  assert.ok(section(toml, "[registries.rbac]"));
});

test("rendering is pure: the same state renders the same file", () => {
  assert.equal(render("default"), renderConfigToml(defaultState()));
  const a = renderConfigToml(byName["openvsx-local-vsx-signing"]);
  const b = renderConfigToml(byName["openvsx-local-vsx-signing"]);
  assert.equal(a, b);
});

test("[registries.vsx_signing] is emitted for the two gallery kinds, with the key id when given", () => {
  const ovsx = render("openvsx-local-vsx-signing");
  assert.deepEqual(section(ovsx, "[registries.vsx_signing]"), [
    'seed_hex = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"',
    'key_id = "2026-09"',
  ]);
  const mp = render("marketplace-hybrid-vsx-signing");
  assert.deepEqual(section(mp, "[registries.vsx_signing]"), [
    'seed_hex = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"',
  ]);
  assert.ok(mp.includes('type = "vscode-marketplace"'));
  assert.ok(mp.includes('mode = "hybrid"'));
});

test("[registries.vsx_signing] is never emitted for another kind", () => {
  const toml = render("npm-vsx-signing-not-emitted");
  assert.ok(!toml.includes("vsx_signing"), toml);
});

test("[registries.repo_signing] keeps its three fields", () => {
  const toml = render("deb-local-repo-signing");
  assert.deepEqual(section(toml, "[registries.repo_signing]"), [
    'seed_hex = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"',
    'user_id = "BatleHub Repo <repo@example.com>"',
    "created = 1700000000",
  ]);
});

test("every registry the state holds is rendered, in order, with its mode", () => {
  const toml = render("two-registries-proxy-and-hybrid");
  const names = toml.split("\n").filter((l) => l.startsWith("name = "));
  assert.deepEqual(names, ['name = "vsx-proxy"', 'name = "cargo"']);
  assert.ok(toml.includes('mode = "hybrid"'));
  assert.ok(!toml.includes('mode = "proxy"'), "proxy is the default and is not written");
});

test("a fresh registry of every kind renders without throwing", () => {
  const kinds = [
    "npm", "cargo", "pypi", "openvsx", "vscode-marketplace", "deb", "rpm", "maven", "nuget",
    "goproxy", "github", "gitlab", "forgejo", "generic",
  ];
  for (const kind of kinds) {
    const s = defaultState();
    // Kinds the generator does not know are skipped by defaultRegistry's
    // fallback; the point is that none of the known ones throws.
    s.registries = [defaultRegistry(kind as never)];
    assert.ok(renderConfigToml(s).includes("[[registries]]"), kind);
  }
});

// ── The verb picker's vocabulary ────────────────────────────────────────────
//
// The same pin `vocabulary_dead_ends.rs` puts on the operator guide: a closed
// set with a hand-maintained copy has two definitions and only one compiles.
// The picker offers `ALL_VERBS`; this reads the enum's `as_str` arms and says
// they are the same list in the same order, so a verb added to the server
// fails here until the form can offer it, and one the form invents fails too.

test("the picker's vocabulary is Action::as_str, in Action::ALL order", () => {
  const src = readFileSync(
    new URL("../../crates/core/src/entities/permission.rs", import.meta.url),
    "utf8",
  );
  const arms = [...src.matchAll(/Action::[A-Za-z]+ => "([a-z0-9:-]+)"/g)].map((m) => m[1]);
  assert.ok(arms.length > 30, "no as_str arms parsed — the enum's shape changed");
  assert.deepEqual(ALL_VERBS, arms);
  // Every ecosystem verb the form knows the kinds of is in the enum too.
  for (const v of Object.keys(ECOSYSTEM_VERBS)) assert.ok(arms.includes(v), v);
});

test("a picker offers what its tier can hold", () => {
  const flat = (tier: Parameters<typeof verbOptions>[0]) =>
    verbOptions(tier).flatMap((g) => g.verbs);
  // The instance tier: no ecosystem verb, and no `*` either — it expands to
  // every verb there, ecosystem ones included, and the server refuses it.
  const instance = flat("instance");
  assert.ok(!instance.includes("*"));
  assert.ok(!instance.includes("npm:dist-tags:write"));
  assert.ok(instance.includes("config:read"));
  // A registry offers its own ecosystem verbs and nobody else's.
  const npm = flat("npm");
  assert.ok(npm.includes("*") && npm.includes("releases:*"));
  assert.ok(npm.includes("npm:dist-tags:write"));
  assert.ok(!npm.includes("terraform:signing-keys:write"));
  assert.ok(flat("openvsx").includes("openvsx:namespace:claim"));
  // Legacy rbac: the expansions, none of the ecosystem verbs.
  const legacy = flat("legacy");
  assert.ok(legacy.includes("*"));
  assert.ok(!legacy.some((v) => v in ECOSYSTEM_VERBS));
  // Nothing offered anywhere is outside the vocabulary.
  for (const tier of ["instance", "legacy", "npm", "terraform"] as const) {
    for (const v of flat(tier)) assert.ok(v === "*" || v === "releases:*" || ALL_VERBS.includes(v), v);
  }
});

test("adding and removing a verb keeps the comma-separated shape the renderer reads", () => {
  let csv = "";
  csv = addVerb(csv, "releases:read");
  csv = addVerb(csv, "releases:publish");
  csv = addVerb(csv, "releases:read"); // already there — no duplicate
  assert.equal(csv, "releases:read, releases:publish");
  assert.equal(permsToToml(csv), '["releases:read", "releases:publish"]');
  assert.equal(removeVerb(csv, "releases:read"), "releases:publish");
  assert.equal(removeVerb("releases:publish", "releases:publish"), "");
  assert.equal(permsToToml(""), "[]");
});

test("a verb the picker would not offer where it stands is named", () => {
  assert.deepEqual(verbsOutOfPlace("releases:read, npm:dist-tags:write", "npm"), []);
  assert.deepEqual(verbsOutOfPlace("releases:read, npm:dist-tags:write", "cargo"), ["npm:dist-tags:write"]);
  assert.deepEqual(verbsOutOfPlace("*, config:read", "instance"), ["*"]);
  assert.deepEqual(verbsOutOfPlace("made:up", "legacy"), ["made:up"]);
});

test("subjects are proposed from the configured auth providers", () => {
  // No providers: the forms that need none.
  assert.deepEqual(subjectSuggestions([]), ["*", "role:anonymous", "role:user", "role:admin"]);

  const token = { ...blankAuthProvider(), type: "token" as const };
  token.tokens = [
    { id: 1, value: "s3cret", role: "user", user_id: "alice" },
    { id: 2, value: "other", role: "admin", user_id: "" },
  ];
  const oidc = { ...blankAuthProvider(), type: "oidc" as const, oidc_name: "corp" };
  oidc.oidc_role_mappings = [{ id: 1, claim: "eng", role: "user" }];
  const k8s = { ...blankAuthProvider(), type: "kubernetes" as const };
  const s = subjectSuggestions([token, oidc, k8s]);
  // A token with a user id is a `user:` subject; one without is nothing.
  assert.ok(s.includes("user:alice"));
  assert.equal(s.filter((x) => x.startsWith("user:")).length, 1);
  // A mapped claim value reaches the identity bare, so it is `group::<name>`;
  // an unmapped group is prefixed with the provider name, offered as a prefix.
  assert.ok(s.includes("group::eng"));
  assert.ok(s.includes("group:corp:"));
  assert.ok(!s.includes("group:corp:eng"));
  // The unnamed provider gets the server's default name.
  assert.ok(s.includes("group:kubernetes:"));
  assert.ok(s.includes("group:*:"));
  assert.equal(new Set(s).size, s.length, "no duplicate proposal");
});
