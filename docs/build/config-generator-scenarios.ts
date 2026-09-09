// The states the config generator's harness renders (RFC 0020 §13, and the
// reason the generator's pure half lives in configToml.ts): one entry per
// form shape worth holding, each a plain `GeneratorState`. Two consumers:
//
//   config-generator.test.ts   — asserts what each renders (node --test)
//   config-generator-fixtures.ts — writes each rendering to
//                                  crates/config/tests/fixtures/config-generator/,
//                                  where `cargo test -p batlehub-config` loads
//                                  it with the real parser and validator.
//
// Add a scenario for every section the generator learns to emit; the Rust
// side is what says the emitted TOML is a config the server accepts.
import {
  defaultRegistry,
  defaultState,
  type GeneratorState,
  type Registry,
  type RegistryType,
} from "../.vitepress/components/configToml.ts";

export interface Scenario {
  name: string;
  state: GeneratorState;
}

function registry(type: RegistryType, patch: Partial<Registry>): Registry {
  return { ...defaultRegistry(type), ...patch };
}

export function scenarios(): Scenario[] {
  const seed = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
  const out: Scenario[] = [];

  out.push({ name: "default", state: defaultState() });

  {
    const s = defaultState();
    s.registries = [
      registry("openvsx", {
        name: "vsx",
        mode: "local",
        vsx_signing_enabled: true,
        vsx_signing_seed_hex: seed,
        vsx_signing_key_id: "2026-09",
      }),
    ];
    out.push({ name: "openvsx-local-vsx-signing", state: s });
  }

  {
    const s = defaultState();
    s.registries = [
      registry("vscode-marketplace", {
        name: "marketplace",
        mode: "hybrid",
        vsx_signing_enabled: true,
        vsx_signing_seed_hex: seed,
      }),
    ];
    out.push({ name: "marketplace-hybrid-vsx-signing", state: s });
  }

  {
    // The form only offers the section on the two gallery kinds, but a state
    // can say anything: the renderer must not emit it for another kind.
    const s = defaultState();
    s.registries = [
      registry("npm", {
        name: "npm",
        mode: "local",
        vsx_signing_enabled: true,
        vsx_signing_seed_hex: seed,
      }),
    ];
    out.push({ name: "npm-vsx-signing-not-emitted", state: s });
  }

  {
    const s = defaultState();
    s.registries = [
      registry("deb", {
        name: "deb",
        mode: "local",
        repo_signing_enabled: true,
        repo_signing_seed_hex: seed,
        repo_signing_user_id: "BatleHub Repo <repo@example.com>",
        repo_signing_created: "1700000000",
      }),
    ];
    out.push({ name: "deb-local-repo-signing", state: s });
  }

  {
    const s = defaultState();
    s.registries = [
      registry("openvsx", { name: "vsx-proxy", mode: "proxy" }),
      registry("cargo", { name: "cargo", mode: "hybrid" }),
    ];
    out.push({ name: "two-registries-proxy-and-hybrid", state: s });
  }

  return out;
}
