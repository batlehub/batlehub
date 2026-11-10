import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const verdictSummary = vi.fn();
vi.mock("@/client/sdk.gen", () => ({
  verdictSummary: (...args: unknown[]) => verdictSummary(...args),
}));

import HoldsPanel from "./HoldsPanel.vue";

const stubs = { RouterLink: { props: ["to"], template: "<a :href='to'><slot/></a>" } };

describe("HoldsPanel", () => {
  beforeEach(() => verdictSummary.mockReset());

  it("shows the counts and links each recent hold to its package", async () => {
    verdictSummary.mockResolvedValue({
      data: {
        registries: [
          {
            registry: "npm1",
            counts: { allowed: 4, warned: 0, quarantined: 1, denied: 2 },
            recent: [
              {
                name: "left-pad",
                version: "1.1.0",
                state: "denied",
                reason_codes: ["cve_gate"],
                evaluated_at: "2026-11-09T10:00:00Z",
              },
            ],
          },
        ],
      },
      error: undefined,
    });
    const w = mount(HoldsPanel, { global: { stubs } });
    await flushPromises();
    expect(w.find('[data-testid="holds-counts"]').text()).toContain("npm1");
    const recent = w.find('[data-testid="holds-recent"]');
    expect(recent.text()).toContain("npm1/left-pad@1.1.0");
    expect(recent.text()).toContain("cve_gate");
    expect(recent.find("a").attributes("href")).toBe("/packages/npm1/left-pad");
    expect(recent.find('[data-state="blocked"]').exists()).toBe(true);
    expect(w.find('[data-testid="open-findings-npm1"]').exists()).toBe(false);
  });

  it("offers each registry's findings when the viewer may read them", async () => {
    verdictSummary.mockResolvedValue({
      data: {
        registries: [
          {
            registry: "npm1",
            counts: { allowed: 0, warned: 0, quarantined: 0, denied: 1 },
            recent: [],
          },
        ],
      },
    });
    const w = mount(HoldsPanel, { props: { canOpenFindings: true }, global: { stubs } });
    await flushPromises();
    await w.find('[data-testid="open-findings-npm1"]').trigger("click");
    expect(w.emitted("openFindings")).toEqual([["npm1"]]);
  });

  it("says so when the summary cannot be read", async () => {
    verdictSummary.mockResolvedValue({ data: undefined, error: { message: "403" } });
    const w = mount(HoldsPanel, { global: { stubs } });
    await flushPromises();
    expect(w.text()).toContain("Could not load the verdict summary.");
  });
});
