import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const exposureReport = vi.fn();
vi.mock("@/client/sdk.gen", () => ({
  exposureReport: (...args: unknown[]) => exposureReport(...args),
}));
vi.mock("@/composables/useAuthFetch", () => ({
  useAuthFetch: () => ({ authFetch: vi.fn() }),
}));

import ExposurePanel from "./ExposurePanel.vue";

function row(overrides: Record<string, unknown> = {}) {
  return {
    consumer: "alice",
    consumer_role: "user",
    registry: "npm",
    package_name: "left-pad",
    version: "1.3.1",
    flag_id: "11111111-1111-1111-1111-111111111111",
    source: "soc",
    external_id: "CASE-7",
    kind: "malware",
    effect: "hard_block",
    summary: "steals tokens",
    flag_first_seen: "2026-09-04T10:00:00Z",
    pulls: 3,
    pulls_before_flag: 2,
    first_pull: "2026-09-04T08:00:00Z",
    last_pull: "2026-09-04T11:00:00Z",
    ...overrides,
  };
}

function page(rows: unknown[], next: string | null = null) {
  return {
    data: {
      rows,
      next,
      coverage: {
        registries_total: 3,
        sbom_configured: 1,
        security_profiles: 1,
        last_scan: [],
        flag_sources: [{ source: "soc", live_flags: 1, last_push_at: null }],
      },
    },
    error: undefined,
  };
}

describe("ExposurePanel", () => {
  beforeEach(() => exposureReport.mockReset());

  it("renders the rows, the retroactive count and the coverage block", async () => {
    exposureReport.mockResolvedValue(page([row()]));
    const w = mount(ExposurePanel);
    await flushPromises();
    const rows = w.find('[data-testid="exposure-rows"]');
    expect(rows.exists()).toBe(true);
    expect(rows.text()).toContain("alice");
    expect(rows.text()).toContain("npm/left-pad@1.3.1");
    expect(rows.text()).toContain("soc:CASE-7");
    const coverage = w.find('[data-testid="exposure-coverage"]');
    expect(coverage.text()).toContain("soc (1)");
    // Never scanned is said, not hidden.
    expect(coverage.text()).toMatch(/never/i);
  });

  it("follows the cursor and appends the next page", async () => {
    exposureReport
      .mockResolvedValueOnce(page([row()], "cursor-1"))
      .mockResolvedValueOnce(page([row({ consumer: "bob", version: "1.3.0" })]));
    const w = mount(ExposurePanel);
    await flushPromises();
    const more = w.find('[data-testid="exposure-more"]');
    expect(more.exists()).toBe(true);
    await more.trigger("click");
    await flushPromises();
    expect(exposureReport).toHaveBeenLastCalledWith(
      expect.objectContaining({ query: expect.objectContaining({ after: "cursor-1" }) }),
    );
    expect(w.find('[data-testid="exposure-rows"]').text()).toContain("bob");
    expect(w.find('[data-testid="exposure-more"]').exists()).toBe(false);
  });

  it("says when nothing matches and when the load failed", async () => {
    exposureReport.mockResolvedValueOnce(page([]));
    const w = mount(ExposurePanel);
    await flushPromises();
    expect(w.find('[data-testid="exposure-empty"]').exists()).toBe(true);
    exposureReport.mockResolvedValueOnce({ data: undefined, error: { error: "x" } });
    await w.find('[data-testid="exposure-apply"]').trigger("click");
    await flushPromises();
    expect(w.text()).toMatch(/could not|impossible/i);
  });
});
