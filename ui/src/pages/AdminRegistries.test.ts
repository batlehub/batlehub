import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { adminStatsMock, registryHealthMock } = vi.hoisted(() => ({
  adminStatsMock: vi.fn(),
  registryHealthMock: vi.fn(),
}));
vi.mock("@/client/sdk.gen", () => ({
  adminStats: adminStatsMock,
  registryHealth: registryHealthMock,
}));

import AdminRegistries from "./AdminRegistries.vue";
import { useAuth } from "@/composables/useAuth";

/** RFC 0042: the registry page asks system:read; the list only stats:read. */
const holdVerbs = (verbs: string[]) => {
  useAuth().identity.value = { verbs, registries: {} } as never;
};

const row = (registry: string, over: Record<string, unknown> = {}) => ({
  registry,
  artifact_hits: 9,
  artifact_misses: 1,
  hit_rate: 0.9,
  upstream_degraded: false,
  upstream_error_rate: 0,
  upstream_latency_ms: 0,
  ...over,
});

async function mountPage() {
  const w = mount(AdminRegistries, {
    global: {
      stubs: {
        SectionTabs: true,
        RouterLink: { props: ["to"], template: '<a :data-to="to"><slot /></a>' },
      },
    },
  });
  await flushPromises();
  return w;
}

describe("AdminRegistries", () => {
  beforeEach(() => {
    adminStatsMock.mockReset();
    registryHealthMock.mockReset();
    registryHealthMock.mockResolvedValue({ data: [] });
    holdVerbs(["stats:read", "system:read"]);
  });

  it("links each registry to its page, with its hit rate and upstream state", async () => {
    adminStatsMock.mockResolvedValue({
      data: {
        since_startup: "2026-11-10T00:00:00Z",
        aggregate: { artifact_hits: 0, artifact_misses: 0, cached_bytes: 0 },
        per_registry: [row("npm-q"), row("a/b", { upstream_degraded: true, hit_rate: null })],
      },
    });
    const w = await mountPage();
    const links = w.findAll("a[data-to]").map((a) => a.attributes("data-to"));
    expect(links).toEqual(["/admin/registry/npm-q", "/admin/registry/a%2Fb"]);
    expect(w.text()).toContain("90.0%");
    expect(w.text()).toContain("Degraded");
    expect(w.text()).toContain("—");
    // Without system:read the name is text, not a link the router would refuse.
    holdVerbs(["stats:read"]);
    await w.vm.$nextTick();
    expect(w.findAll("a[data-to]")).toHaveLength(0);
    expect(w.text()).toContain("npm-q");
  });

  // What each registry *is*, joined from the health probe by name; a registry
  // the probe does not return keeps its row, with dashes for what it lacks.
  it("shows each registry's type and mode beside its stats", async () => {
    adminStatsMock.mockResolvedValue({
      data: {
        since_startup: "2026-11-10T00:00:00Z",
        aggregate: { artifact_hits: 0, artifact_misses: 0, cached_bytes: 0 },
        per_registry: [row("npm-q"), row("orphan")],
      },
    });
    registryHealthMock.mockResolvedValue({
      data: [{ registry: "npm-q", registry_type: "npm", mode: "hybrid", last_pull_at: null }],
    });
    const w = await mountPage();
    const rows = w.findAll('[data-testid="registry-row"]');
    expect(rows).toHaveLength(2);
    expect(rows[0]!.text()).toContain("npm");
    expect(rows[0]!.text()).toContain("hybrid");
    expect(rows[0]!.text()).toContain("Healthy");
    expect(rows[1]!.text()).toContain("orphan");
    expect(rows[1]!.text()).toContain("—");
  });

  it("says when nothing is configured", async () => {
    adminStatsMock.mockResolvedValue({
      data: {
        since_startup: "2026-11-10T00:00:00Z",
        aggregate: { artifact_hits: 0, artifact_misses: 0, cached_bytes: 0 },
        per_registry: [],
      },
    });
    const w = await mountPage();
    expect(w.text()).toContain("No registry is configured.");
  });
});
