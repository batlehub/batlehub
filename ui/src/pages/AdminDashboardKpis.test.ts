import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { adminStatsMock, adminStatsHistoryMock, registryHealthMock } = vi.hoisted(() => ({
  adminStatsMock: vi.fn(),
  adminStatsHistoryMock: vi.fn(),
  registryHealthMock: vi.fn(),
}));

vi.mock("@/client/sdk.gen", () => ({
  adminStats: adminStatsMock,
  adminStatsHistory: adminStatsHistoryMock,
  registryHealth: registryHealthMock,
}));

import AdminDashboard from "./AdminDashboard.vue";
import { useAuth } from "@/composables/useAuth";
import { formatDate } from "@/lib/format";

const stats = {
  since_startup: "2026-08-12T10:00:00Z",
  aggregate: { artifact_hits: 90, artifact_misses: 10, hit_rate: 0.9, cached_bytes: 1024 },
  per_registry: [],
};

async function mountDashboard() {
  const wrapper = mount(AdminDashboard, {
    global: { stubs: { RouterLink: { template: "<a><slot /></a>" } } },
  });
  await flushPromises();
  return wrapper;
}

const kpis = (w: Awaited<ReturnType<typeof mountDashboard>>) =>
  w.find('[data-testid="dashboard-kpis"]');
const registriesState = (w: Awaited<ReturnType<typeof mountDashboard>>) =>
  w.find('[data-testid="kpi-registries-state"]');

/** The four figures under the verdict. */
describe("AdminDashboard figures", () => {
  beforeEach(() => {
    adminStatsMock.mockReset().mockResolvedValue({ data: stats });
    adminStatsHistoryMock.mockReset().mockResolvedValue({ data: null });
    registryHealthMock.mockReset().mockResolvedValue({
      data: [{ registry: "npm", recent_errors: [] }],
    });
    useAuth().identity.value = { verbs: ["stats:read"], registries: {} } as never;
  });

  it("shows the hit rate, downloads, cache size and registry count", async () => {
    const w = await mountDashboard();
    expect(kpis(w).text()).toContain("90.0%");
    expect(kpis(w).text()).toContain("100");
    expect(registriesState(w).text()).toBe("all answering");
    expect(registriesState(w).classes()).toContain("text-ok");
  });

  // RFC 0050 §6.7: a restart is dated to the minute, not the day.
  it("dates the restart with its time of day", async () => {
    const w = await mountDashboard();
    expect(kpis(w).text()).toContain(`since restart, ${formatDate(stats.since_startup)}`);
  });

  it("counts registries with upstream errors", async () => {
    registryHealthMock.mockResolvedValue({
      data: [{ registry: "npm", recent_errors: [{ error_type: "error" }] }],
    });
    const w = await mountDashboard();
    expect(registriesState(w).text()).toBe("1 with errors");
    expect(registriesState(w).classes()).toContain("text-destructive");
  });

  // An unread probe is unknown: never "0 registries, all answering" in green.
  it("says the health is unknown when the probe fails", async () => {
    registryHealthMock.mockResolvedValue({ error: { message: "boom" } });
    const w = await mountDashboard();
    expect(registriesState(w).text()).toBe("Health could not be read.");
    expect(registriesState(w).classes()).not.toContain("text-ok");
    expect(kpis(w).text()).not.toContain("all answering");
  });

  // RFC 0050 §6.7: a rate of nothing is not a rate.
  it("shows no hit rate before the first download", async () => {
    adminStatsMock.mockResolvedValue({
      data: {
        ...stats,
        aggregate: { artifact_hits: 0, artifact_misses: 0, hit_rate: 0, cached_bytes: 0 },
      },
    });
    const w = await mountDashboard();
    expect(kpis(w).text()).not.toContain("0.0%");
  });
});
