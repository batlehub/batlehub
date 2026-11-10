import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const listVerdicts = vi.fn();
vi.mock("@/client/sdk.gen", () => ({
  listVerdicts: (...args: unknown[]) => listVerdicts(...args),
}));

import FindingsPanel from "./FindingsPanel.vue";

const stubs = { RouterLink: { props: ["to"], template: "<a :href='to'><slot/></a>" } };

const verdict = {
  package: { registry: "npm1", name: "left-pad", version: "1.1.0" },
  state: "denied",
  reason_codes: ["cve_gate"],
  policy_ref: "npm1/default",
  evaluated_at: "2026-11-09T10:00:00Z",
  findings: [
    {
      code: "cve_gate",
      kind: "vulnerability",
      scanner: "osv",
      severity: "critical",
      summary: "prototype pollution",
      reference: "GHSA-xxxx",
    },
  ],
};

describe("FindingsPanel", () => {
  beforeEach(() => listVerdicts.mockReset());

  it("lists the denied versions of the registry with their findings", async () => {
    listVerdicts.mockResolvedValue({ data: { registry: "npm1", items: [verdict] } });
    const w = mount(FindingsPanel, { props: { registry: "npm1" }, global: { stubs } });
    await flushPromises();
    expect(listVerdicts).toHaveBeenCalledWith({ query: { registry: "npm1", state: "denied" } });
    const row = w.find('[data-testid="findings-row"]');
    expect(row.find("a").attributes("href")).toBe("/packages/npm1/left-pad");
    expect(row.find('[data-state="blocked"]').exists()).toBe(true);
    expect(row.text()).toContain("GHSA-xxxx");
  });

  it("says the registry has no security profile on a 404", async () => {
    listVerdicts.mockResolvedValue({ data: undefined, error: {}, response: { status: 404 } });
    const w = mount(FindingsPanel, { props: { registry: "npm1" }, global: { stubs } });
    await flushPromises();
    expect(w.text()).toContain("This registry has no security profile.");
  });

  it("closes on request", async () => {
    listVerdicts.mockResolvedValue({ data: { registry: "npm1", items: [] } });
    const w = mount(FindingsPanel, { props: { registry: "npm1" }, global: { stubs } });
    await flushPromises();
    await w.find('[data-testid="findings-close"]').trigger("click");
    expect(w.emitted("close")).toHaveLength(1);
  });
});
