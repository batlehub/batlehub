// The config generator's harness: `node --test docs/build/config-generator.test.ts`
// (types stripped by Node itself, no toolchain). It renders each scenario of
// config-generator-scenarios.ts through the same function the page uses and
// asserts on the TOML; the fixtures the Rust side loads are written by
// config-generator-fixtures.ts from the same scenarios.
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  CONFIG_VERSION,
  defaultRegistry,
  defaultState,
  renderConfigToml,
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
