import { describe, it, expect, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";

const { detailMock } = vi.hoisted(() => ({ detailMock: vi.fn() }));
vi.mock("@/client/sdk.gen", () => ({ securityCoverageRegistry: detailMock }));
vi.mock("vue-router", () => ({
  RouterLink: { props: ["to"], template: "<a><slot/></a>" },
  useRoute: () => ({ params: { registry: "npm-public" } }),
}));

import SecurityCoverageRegistry from "./SecurityCoverageRegistry.vue";

async function mountWith(over: object) {
  detailMock.mockResolvedValue({
    data: {
      registry: "npm-public",
      kind: "npm",
      mode: "proxy",
      protected: true,
      security_mode: "block",
      cve_gate: false,
      license_gate: false,
      deny_at: "high",
      ...over,
    },
    response: { status: 200 },
  });
  const w = mount(SecurityCoverageRegistry);
  await flushPromises();
  return w;
}

describe("SecurityCoverageRegistry (RFC 0050 §4.5)", () => {
  it("says an unscanned version is never served when the delay is 0", async () => {
    const w = await mountWith({ serve_unscanned_after_secs: 0 });
    expect(w.get('[data-testid="unscanned-after"]').text()).toContain("never");
    expect(w.text()).not.toContain("0 d");
  });

  it("draws durations in their exact unit, never rounded to zero", async () => {
    const w = await mountWith({
      min_age_secs: 7200,
      serve_unscanned_after_secs: 86_400,
      rescan_interval_secs: 600,
    });
    expect(w.text()).toContain("2 h");
    expect(w.get('[data-testid="unscanned-after"]').text()).toBe("1 d");
    expect(w.text()).toContain("every 10 min");
  });
});
