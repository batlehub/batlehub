import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { adminStatsMock, adminStatsHistoryMock, registryHealthMock, authFetchMock } = vi.hoisted(
  () => ({
    adminStatsMock: vi.fn(),
    adminStatsHistoryMock: vi.fn(),
    registryHealthMock: vi.fn(),
    authFetchMock: vi.fn(),
  }),
);

vi.mock("@/client/sdk.gen", () => ({
  adminStats: adminStatsMock,
  adminStatsHistory: adminStatsHistoryMock,
  registryHealth: registryHealthMock,
}));
vi.mock("@/composables/useAuthFetch", () => ({
  useAuthFetch: () => ({ authFetch: authFetchMock }),
}));

import AdminDashboard from "./AdminDashboard.vue";
import { useAuth } from "@/composables/useAuth";

const stats = {
  since_startup: "2026-08-12T10:00:00Z",
  aggregate: { artifact_hits: 90, artifact_misses: 10, hit_rate: 0.9, cached_bytes: 1024 },
  per_registry: [],
};
const healthy = { registry: "npm", recent_errors: [] };
const warning = { code: "proxy-trust.unconfigured", path: "server.trusted_proxies", message: "x" };

const holdVerbs = (verbs: string[]) => {
  useAuth().identity.value = { verbs, registries: {} } as never;
};

async function mountDashboard() {
  const wrapper = mount(AdminDashboard, {
    global: { stubs: { RouterLink: { template: "<a><slot /></a>" } } },
  });
  await flushPromises();
  return wrapper;
}

/**
 * The figures and the needs-attention block. Config warnings lived only on the
 * Configuration page, so an instance could carry several for weeks under a
 * dashboard that read "all answering".
 */
describe("AdminDashboard figures and attention", () => {
  beforeEach(() => {
    adminStatsMock.mockReset().mockResolvedValue({ data: stats });
    adminStatsHistoryMock.mockReset().mockResolvedValue({ data: null });
    registryHealthMock.mockReset().mockResolvedValue({ data: [healthy] });
    authFetchMock
      .mockReset()
      .mockResolvedValue({ ok: true, json: async () => ({ warnings: [warning, warning] }) });
    holdVerbs(["stats:read", "config:read"]);
  });

  it("states the four figures, the registries one in the ok signal", async () => {
    const w = await mountDashboard();
    const kpis = w.get('[data-testid="dashboard-kpis"]').text();
    expect(kpis).toContain("90.0%");
    expect(kpis).toContain("100");
    expect(kpis).toContain("1.0 KiB");
    expect(w.get('[data-testid="kpi-registries-state"]').classes()).toContain("text-ok");
  });

  it("surfaces configuration warnings where the operator lands", async () => {
    const w = await mountDashboard();
    expect(w.get('[data-testid="dashboard-attention"]').text()).toContain(
      "2 configuration warnings",
    );
  });

  it("asks nothing of a viewer who cannot read the config", async () => {
    holdVerbs(["stats:read"]);
    const w = await mountDashboard();
    expect(authFetchMock).not.toHaveBeenCalled();
    expect(w.find('[data-testid="dashboard-attention"]').exists()).toBe(false);
  });
});
