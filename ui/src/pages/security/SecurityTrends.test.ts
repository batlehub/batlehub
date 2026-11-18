import { describe, it, expect, vi, beforeEach } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { reactive } from "vue";

const { trendsMock, coverageMock, replaceMock } = vi.hoisted(() => ({
  trendsMock: vi.fn(),
  coverageMock: vi.fn(),
  replaceMock: vi.fn(),
}));
vi.mock("@/client/sdk.gen", () => ({ securityTrends: trendsMock, securityCoverage: coverageMock }));
const route = reactive({ query: {} as Record<string, string> });
vi.mock("vue-router", () => ({
  useRoute: () => route,
  useRouter: () => ({ replace: replaceMock }),
}));

import SecurityTrends from "./SecurityTrends.vue";

const day = (d: string, extra: object = {}) => ({
  day: d,
  allowed: null,
  warned: null,
  quarantined: null,
  denied: null,
  refusals: null,
  jobs_done: null,
  jobs_failed: null,
  ...extra,
});

async function mountWith(body: object, query: Record<string, string> = {}) {
  route.query = query;
  trendsMock.mockResolvedValue({ data: body });
  coverageMock.mockResolvedValue({ data: { registries: [] } });
  const w = mount(SecurityTrends);
  await flushPromises();
  return w;
}

beforeEach(() => {
  trendsMock.mockReset();
  replaceMock.mockReset();
});

describe("SecurityTrends (RFC 0050 §4.2)", () => {
  it("asks the 30-day window for an unknown one", async () => {
    await mountWith({ window_days: 30, history_enabled: true, days: [] }, { window: "14d" });
    expect(trendsMock).toHaveBeenCalledWith({ query: { window: "30d", registry: undefined } });
  });

  it("gives the same data as a table, a missing day reading 'no data', never zero (§11, 2 and 6)", async () => {
    const w = await mountWith({
      window_days: 7,
      history_enabled: true,
      days: [
        day("2026-11-15"),
        day("2026-11-16", { quarantined: 4, denied: 9, refusals: 3, allowed: 20 }),
      ],
    });
    expect(w.find('[data-testid="trend-charts"]').exists()).toBe(true);
    const toggle = w.findAll("button").find((b) => b.text() === "Show as table")!;
    await toggle.trigger("click");
    const table = w.get('[data-testid="trend-table"]');
    const rows = table.findAll("tbody tr");
    expect(rows[0]!.text()).toContain("no data");
    expect(rows[0]!.text()).not.toMatch(/\b0\b/);
    expect(rows[1]!.text()).toContain("13");
  });

  it("says history is off rather than drawing empty charts", async () => {
    const w = await mountWith({ window_days: 30, history_enabled: false, days: [] });
    expect(w.get('[data-testid="history-off"]').text()).toContain("history_enabled = false");
  });

  it("leaves the flag series out when the server does (no flags:read)", async () => {
    const w = await mountWith({
      window_days: 7,
      history_enabled: true,
      days: [day("2026-11-16", { allowed: 1, refusals: 0 })],
    });
    expect(w.text()).not.toContain("Flags opened");
  });
});
