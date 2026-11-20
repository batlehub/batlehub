import { describe, it, expect, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";

const { coverageMock } = vi.hoisted(() => ({ coverageMock: vi.fn() }));
vi.mock("@/client/sdk.gen", () => ({ securityCoverage: coverageMock }));
vi.mock("vue-router", () => ({ RouterLink: { props: ["to"], template: "<a><slot/></a>" } }));

import SecurityCoverage from "./SecurityCoverage.vue";

const row = (over: object) => ({
  registry: "npm-public",
  kind: "npm",
  mode: "proxy",
  protected: true,
  security_mode: "block",
  cve_gate: false,
  license_gate: false,
  deny_at: "high",
  scanned: 14,
  waiting: 1,
  failed: 0,
  ...over,
});

async function mountWith(rows: object[]) {
  coverageMock.mockResolvedValue({ data: { computed_at: "", registries: rows } });
  const w = mount(SecurityCoverage);
  await flushPromises();
  return w;
}

describe("SecurityCoverage (RFC 0050 §4.5)", () => {
  it("names each registry's protection, an unprotected one included", async () => {
    const w = await mountWith([
      row({}),
      row({ registry: "pypi-public", security_mode: "warn", deny_at: "critical" }),
      row({
        registry: "go-public",
        kind: "goproxy",
        protected: false,
        security_mode: null,
        deny_at: null,
      }),
    ]);
    expect(w.get('[data-testid="coverage-summary"]').text()).toContain(
      "3 registries · 2 protected · 1 not protected",
    );
    const rows = w.findAll('[data-testid="coverage-rows"] tbody tr');
    expect(rows[0]!.text()).toContain("refuses high and above");
    expect(rows[0]!.text()).toContain("14 / 15");
    expect(rows[1]!.text()).toContain("warns on critical and above, still refuses known malware");
    expect(rows[1]!.text()).not.toContain("refuses critical");
    expect(rows[2]!.text()).toContain("not protected");
  });

  it("does not count a registry accepted as unprotected", async () => {
    const w = await mountWith([
      row({}),
      row({ registry: "go-public", protected: false, security_mode: null, accepted: true }),
    ]);
    expect(w.get('[data-testid="coverage-summary"]').text()).toContain(
      "2 registries · 1 protected · 0 not protected",
    );
  });

  it("has no scanners column without config:read (the server leaves them out)", async () => {
    const w = await mountWith([row({})]);
    expect(w.text()).not.toContain("Scanners");
    const named = await mountWith([row({ scanners: [{ name: "osv", required: true }] })]);
    expect(named.text()).toContain("osv (required)");
  });
});
