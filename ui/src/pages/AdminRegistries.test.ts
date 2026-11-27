import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { adminStatsMock, registryHealthMock, listRegistriesMock } = vi.hoisted(() => ({
  adminStatsMock: vi.fn(),
  registryHealthMock: vi.fn(),
  listRegistriesMock: vi.fn(),
}));
vi.mock("@/client/sdk.gen", () => ({
  adminStats: adminStatsMock,
  registryHealth: registryHealthMock,
  listRegistries: listRegistriesMock,
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
    listRegistriesMock.mockReset();
    listRegistriesMock.mockResolvedValue({ data: [] });
    holdVerbs(["stats:read", "system:read"]);
  });

  it("links each registry to its page, with its hit rate and upstream state", async () => {
    adminStatsMock.mockResolvedValue({
      data: {
        since_startup: "2026-11-10T00:00:00Z",
        aggregate: { artifact_hits: 0, artifact_misses: 0, cached_bytes: 0 },
        per_registry: [
          row("npm-q", { artifact_misses: 0 }),
          row("a/b", { upstream_degraded: true, hit_rate: null }),
        ],
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

  // What each registry *is*, from the registry list any viewer can read; a
  // registry missing from it keeps its row, with dashes for what it lacks.
  it("shows each registry's type and mode beside its stats", async () => {
    adminStatsMock.mockResolvedValue({
      data: {
        since_startup: "2026-11-10T00:00:00Z",
        aggregate: { artifact_hits: 0, artifact_misses: 0, cached_bytes: 0 },
        per_registry: [row("npm-q"), row("orphan")],
      },
    });
    listRegistriesMock.mockResolvedValue({
      data: [{ name: "npm-q", type: "npm", mode: "hybrid" }],
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
    // A health row with no pull is "Never"; no health row at all is unknown.
    expect(rows[0]!.text()).toContain("Never");
    expect(rows[1]!.text()).toContain("orphan");
    expect(rows[1]!.text()).not.toContain("Never");
    expect(rows[1]!.text()).toContain("—");
  });

  // The registry list follows the legacy access table and can miss a
  // configured registry; the health probe knows them all.
  it("takes type and mode from the health probe when the registry list misses one", async () => {
    adminStatsMock.mockResolvedValue({
      data: {
        since_startup: "2026-11-10T00:00:00Z",
        aggregate: { artifact_hits: 0, artifact_misses: 0, cached_bytes: 0 },
        per_registry: [row("my-gems")],
      },
    });
    registryHealthMock.mockResolvedValue({
      data: [
        { registry: "my-gems", registry_type: "rubygems", mode: "hybrid", last_pull_at: null },
      ],
    });
    const w = await mountPage();
    const text = w.get('[data-testid="registry-row"]').text();
    expect(text).toContain("rubygems");
    expect(text).toContain("hybrid");
  });

  // Without system:read the health probe is skipped: type and mode still show,
  // and no registry is claimed never pulled.
  it("keeps type and mode and claims no pull history without system:read", async () => {
    holdVerbs(["stats:read"]);
    adminStatsMock.mockResolvedValue({
      data: {
        since_startup: "2026-11-10T00:00:00Z",
        aggregate: { artifact_hits: 0, artifact_misses: 0, cached_bytes: 0 },
        per_registry: [row("npm-q")],
      },
    });
    listRegistriesMock.mockResolvedValue({ data: [{ name: "npm-q", type: "npm", mode: "proxy" }] });
    const w = await mountPage();
    const text = w.find('[data-testid="registry-row"]').text();
    expect(text).toContain("npm");
    expect(text).toContain("proxy");
    expect(text).not.toContain("Never");
  });

  // Not degraded is also what a registry the upstream was never asked for
  // reports; green needs a miss, a latency or an error rate behind it.
  it("calls an upstream healthy only once it was asked", async () => {
    adminStatsMock.mockResolvedValue({
      data: {
        since_startup: "2026-11-10T00:00:00Z",
        aggregate: { artifact_hits: 0, artifact_misses: 0, cached_bytes: 0 },
        per_registry: [
          row("fresh", { artifact_hits: 0, artifact_misses: 0, hit_rate: null }),
          row("private", { artifact_misses: 0 }),
          row("used"),
        ],
      },
    });
    listRegistriesMock.mockResolvedValue({
      data: [
        { name: "fresh", type: "npm", mode: "proxy" },
        { name: "private", type: "npm", mode: "local" },
        { name: "used", type: "npm", mode: "proxy" },
      ],
    });
    const w = await mountPage();
    const [fresh, local, used] = w.findAll('[data-testid="registry-row"]');
    expect(fresh!.text()).toContain("No traffic yet");
    expect(fresh!.text()).not.toContain("Healthy");
    expect(local!.text()).not.toContain("Healthy");
    expect(local!.text()).not.toContain("No traffic yet");
    expect(used!.text()).toContain("Healthy");
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
