import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { adminStatsMock } = vi.hoisted(() => ({ adminStatsMock: vi.fn() }));
vi.mock("@/client/sdk.gen", () => ({ adminStats: adminStatsMock }));

import AdminRegistries from "./AdminRegistries.vue";

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
  beforeEach(() => adminStatsMock.mockReset());

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
    expect(links).toEqual([
      "/admin/observability/registries/npm-q",
      "/admin/observability/registries/a%2Fb",
    ]);
    expect(w.text()).toContain("90.0%");
    expect(w.text()).toContain("degraded");
    expect(w.text()).toContain("—");
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
