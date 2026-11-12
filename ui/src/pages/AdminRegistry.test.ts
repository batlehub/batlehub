import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { getRegistryViewMock, adminStatsMock, adminStatsHistoryMock, replaceMock } = vi.hoisted(
  () => ({
    getRegistryViewMock: vi.fn(),
    adminStatsMock: vi.fn(),
    adminStatsHistoryMock: vi.fn(),
    replaceMock: vi.fn(),
  }),
);

vi.mock("@/client/sdk.gen", () => ({
  getRegistryView: getRegistryViewMock,
  adminStats: adminStatsMock,
  adminStatsHistory: adminStatsHistoryMock,
}));
vi.mock("vue-router", async () => {
  const { reactive, defineComponent, h } = await import("vue");
  const route = reactive({
    path: "/admin/registry/npm-q",
    params: { name: "npm-q" } as Record<string, string>,
    query: {} as Record<string, string>,
  });
  const RouterLink = defineComponent({
    props: { to: { type: [String, Object], required: true } },
    setup(props, { slots }) {
      return () =>
        h(
          "a",
          { "data-to": typeof props.to === "string" ? props.to : JSON.stringify(props.to) },
          slots.default?.(),
        );
    },
  });
  return { useRoute: () => route, useRouter: () => ({ replace: replaceMock }), RouterLink };
});

const held = { verbs: [] as string[] };
vi.mock("@/composables/useAuth", () => ({
  useAuth: () => ({ can: (verb: string) => held.verbs.includes(verb) }),
}));

import { useRoute } from "vue-router";
import AdminRegistry from "./AdminRegistry.vue";

const view = (over: Record<string, unknown> = {}) => ({
  name: "npm-q",
  registry_type: "npm",
  mode: "proxy",
  monitoring_url: "https://grafana.test/d/x?var-registry=npm-q",
  live: {
    upstream_degraded: false,
    upstream_error_rate: 0.01,
    upstream_latency_ms: 120,
    since: "2026-11-10T09:12:00Z",
  },
  scan: { enabled: true, queued: 14, scanning: 1, failed: 2, sbom_recheck_enabled: false },
  suggestions: [],
  config: {
    upstream_count: 1,
    upstreams: ["https://registry.npmjs.org/"],
    other_urls: [],
    upstream_auth: { state: "configured", kind: "bearer" },
    cache: { metadata_ttl_secs: 300, serve_stale: false },
    rules: ["deny_latest"],
    security: {
      mode: "block",
      scanners: ["osv"],
      max_severity: "high",
      rescan_interval_secs: 604800,
    },
    grant_subjects: 3,
    namespaces: 0,
  },
  ...over,
});

const stats = {
  data: {
    since_startup: "2026-11-10T09:12:00Z",
    aggregate: { artifact_hits: 92, artifact_misses: 8, hit_rate: 0.92, cached_bytes: 0 },
    per_registry: [
      {
        registry: "npm-q",
        artifact_hits: 92,
        artifact_misses: 8,
        hit_rate: 0.92,
        upstream_degraded: false,
        upstream_error_rate: 0,
        upstream_latency_ms: 0,
      },
    ],
  },
  response: { status: 200 },
};

const history = (points: unknown[]) => ({
  data: {
    window_days: 1,
    from: "2026-11-09T00:00:00Z",
    to: "2026-11-10T00:00:00Z",
    trend: { hits: 0, misses: 0 },
    per_registry: points.length ? [{ registry: "npm-q", points }] : [],
  },
  response: { status: 200 },
});

const mounted: { unmount: () => void }[] = [];

async function mountPage(query: Record<string, string> = {}) {
  const route = useRoute() as unknown as { query: Record<string, string> };
  route.query = query;
  const w = mount(AdminRegistry, { global: { stubs: { SectionTabs: true } } });
  mounted.push(w);
  await flushPromises();
  await flushPromises();
  return w;
}

describe("AdminRegistry", () => {
  beforeEach(() => {
    getRegistryViewMock.mockReset().mockResolvedValue({ data: view(), response: { status: 200 } });
    adminStatsMock.mockReset().mockResolvedValue(stats);
    adminStatsHistoryMock.mockReset().mockResolvedValue(history([]));
    replaceMock.mockReset();
  });
  afterEach(() => mounted.splice(0).forEach((w) => w.unmount()));

  it("draws the header, the strip and the monitoring link", async () => {
    const w = await mountPage();
    expect(getRegistryViewMock).toHaveBeenCalledWith({ path: { name: "npm-q" } });
    expect(w.text()).toContain("npm · proxy");
    const a = w.get('[data-testid="monitoring"]');
    expect(a.attributes("href")).toBe("https://grafana.test/d/x?var-registry=npm-q");
    expect(a.attributes("target")).toBe("_blank");
    expect(a.attributes("rel")).toBe("noopener noreferrer");
    expect(w.get('[data-testid="strip-cache"]').text()).toContain("92 %");
    expect(w.get('[data-testid="strip-queue"]').text()).toBe("14 queued · 1 scanning · 2 failed");
    expect(w.get('[data-testid="strip-scan"]').text()).toBe("rescan every 7 d");
    expect(w.get('[data-testid="strip-advice"]').text()).toBe("0");
    expect(w.find('[data-testid="no-suggestions"]').exists()).toBe(true);
  });

  /**
   * RFC 0046 M4: the actions are links to the owning pages with the registry
   * preselected, each drawn only when the guard would open its page — and the
   * strip is absent, not empty, when none is left.
   */
  it("draws the actions the viewer holds, each carrying the registry (M4)", async () => {
    const actions = (w: Awaited<ReturnType<typeof mountPage>>) =>
      w.findAll('[data-testid="actions"] a').map((a) => JSON.parse(a.attributes("data-to") ?? ""));
    try {
      held.verbs = ["system:read", "cache:warm", "config:read"];
      const all = actions(await mountPage());
      expect(all.map((to: { path: string }) => to.path)).toEqual([
        "/admin/registries/upstream",
        "/admin/registries/warming",
        "/admin/system/health",
        "/admin/registries/imports",
        "/admin/system/configuration",
      ]);
      expect(
        all.every((to: { query: { registry: string } }) => to.query.registry === "npm-q"),
      ).toBe(true);
      held.verbs = ["cache:warm"];
      expect(actions(await mountPage()).map((to: { path: string }) => to.path)).toEqual([
        "/admin/registries/warming",
        "/admin/registries/imports",
      ]);
      held.verbs = [];
      expect((await mountPage()).find('[data-testid="actions"]').exists()).toBe(false);
    } finally {
      held.verbs = [];
    }
  });

  /** T11, UI half: a `javascript:` value forced into the response is never bound. */
  it("binds no monitoring link that is not http(s), and none when absent", async () => {
    getRegistryViewMock.mockResolvedValue({
      data: view({ monitoring_url: "javascript:alert(1)" }),
      response: { status: 200 },
    });
    let w = await mountPage();
    expect(w.find('[data-testid="monitoring"]').exists()).toBe(false);
    expect(w.html()).not.toContain("javascript:");
    getRegistryViewMock.mockResolvedValue({
      data: view({ monitoring_url: undefined }),
      response: { status: 200 },
    });
    w = await mountPage();
    expect(w.find('[data-testid="monitoring"]').exists()).toBe(false);
  });

  it("lists the suggestions with their links, and marks the keys in Configuration", async () => {
    const suggestions = [
      { code: "degraded_without_stale" },
      { code: "security_without_rescan" },
      { code: "scan_failures", count: 2 },
    ];
    getRegistryViewMock.mockResolvedValue({
      data: view({ suggestions }),
      response: { status: 200 },
    });
    const w = await mountPage();
    const list = w.get('[data-testid="suggestions"]').text();
    expect(list).toContain("Consider serve_stale = true");
    expect(list).toContain("never rescanned");
    expect(list).toContain("2 versions failed to scan");
    expect(w.get('[data-testid="strip-advice"]').text()).toBe("⚠ 3");
    expect(w.html()).toContain("failures");

    const cfg = await mountPage({ tab: "config" });
    expect(cfg.get('[data-testid="config-stale"]').text()).toContain("⚠");
    expect(cfg.get('[data-testid="config-security"]').text()).toContain("⚠");
  });

  it("shows the configuration, credentials as presence only", async () => {
    const w = await mountPage({ tab: "config" });
    expect(w.get('[data-testid="config-auth"]').text()).toBe("configured (bearer)");
    expect(w.get('[data-testid="config-upstreams"]').text()).toContain("registry.npmjs.org");
    expect(w.text()).toContain("3 subjects · 0 namespaces");
  });

  it("says the URLs are hidden without upstreams:read", async () => {
    const c = view().config;
    getRegistryViewMock.mockResolvedValue({
      data: view({
        config: {
          ...c,
          upstreams: undefined,
          other_urls: undefined,
          upstream_auth: { state: "none" },
        },
      }),
      response: { status: 200 },
    });
    const w = await mountPage({ tab: "config" });
    expect(w.get('[data-testid="config-upstreams"]').text()).toBe(
      "1 upstream(s) (hidden: upstreams:read)",
    );
    expect(w.get('[data-testid="config-auth"]').text()).toBe("none");
  });

  it("renders no Configuration tab without config:read, and falls back to Overview", async () => {
    getRegistryViewMock.mockResolvedValue({
      data: view({ config: undefined }),
      response: { status: 200 },
    });
    const w = await mountPage({ tab: "config" });
    expect(w.find('[data-testid="config"]').exists()).toBe(false);
    expect(w.text()).not.toContain("Configuration\n");
    expect(w.find('[data-testid="no-suggestions"]').exists()).toBe(true);
  });

  it("falls back to Overview on an unknown tab, and moves tabs with replace", async () => {
    const w = await mountPage({ tab: "nope" });
    expect(w.find('[data-testid="no-suggestions"]').exists()).toBe(true);
    const vm = w.findComponent({ name: "Tabs" });
    vm.vm.$emit("update:modelValue", "scan");
    expect(replaceMock).toHaveBeenCalledWith({
      path: "/admin/registry/npm-q",
      query: { tab: "scan" },
    });
  });

  it("says a registry without security does not scan", async () => {
    getRegistryViewMock.mockResolvedValue({
      data: view({ scan: { enabled: false, sbom_recheck_enabled: false } }),
      response: { status: 200 },
    });
    const w = await mountPage({ tab: "scan" });
    expect(w.get('[data-testid="no-scan"]').text()).toBe("This registry does not scan packages.");
    expect(w.get('[data-testid="strip-queue"]').text()).toBe("off");
    expect(w.get('[data-testid="strip-scan"]').text()).toBe("off");
  });

  it("shows the rescan and SBOM clocks on the Scan tab", async () => {
    const c = view().config;
    getRegistryViewMock.mockResolvedValue({
      data: view({
        config: { ...c, security: { ...c.security, rescan_interval_secs: undefined } },
        scan: {
          enabled: true,
          queued: 0,
          scanning: 0,
          failed: 0,
          sbom_recheck_enabled: true,
          sbom_recheck: {
            last_scan_at: "2026-11-10T03:00:00Z",
            artifacts_scanned: 4812,
            findings: 7,
            next_at: "2026-11-11T03:00:00Z",
          },
        },
      }),
      response: { status: 200 },
    });
    const w = await mountPage({ tab: "scan" });
    expect(w.get('[data-testid="scan-rescan"]').text()).toContain(
      "never — no rescan.interval_secs",
    );
    expect(w.get('[data-testid="scan-sbom"]').text()).toContain("4812 artifacts · 7 findings");
    expect(w.get('[data-testid="scan-sbom"]').text()).toContain("next");
    expect(w.html()).toContain("registry");
  });

  it("says the SBOM re-check has not run yet", async () => {
    getRegistryViewMock.mockResolvedValue({
      data: view({ scan: { enabled: true, sbom_recheck_enabled: true } }),
      response: { status: 200 },
    });
    const w = await mountPage({ tab: "scan" });
    expect(w.get('[data-testid="scan-sbom"]').text()).toBe("not yet run");
    expect(w.get('[data-testid="strip-queue"]').text()).toBe("—");
  });

  it("draws the activity history, or says there was no traffic", async () => {
    let w = await mountPage({ tab: "activity" });
    expect(adminStatsHistoryMock).toHaveBeenCalledWith({ query: { window: "1d" } });
    expect(w.find('[data-testid="no-traffic"]').exists()).toBe(true);
    expect(w.get('[data-testid="footnote"]').text()).toContain("Monitoring link");

    adminStatsHistoryMock.mockResolvedValue(
      history([
        { window_start: "2026-11-10T00:00:00Z", hits: 9, misses: 1, cached_bytes: 0 },
        { window_start: "2026-11-10T01:00:00Z", hits: 0, misses: 0, cached_bytes: 0 },
      ]),
    );
    w = await mountPage({ tab: "activity" });
    expect(w.findAll('[data-testid="bar"]')).toHaveLength(3);
    expect(w.findAll('[data-testid="gap"]')).toHaveLength(1);
    const week = w.findAll("button").find((b) => b.text() === "7 d");
    await week!.trigger("click");
    await flushPromises();
    expect(adminStatsHistoryMock).toHaveBeenLastCalledWith({ query: { window: "7d" } });
  });

  it("asks for the monitoring key when none is set, and retries a failed history", async () => {
    getRegistryViewMock.mockResolvedValue({
      data: view({ monitoring_url: undefined }),
      response: { status: 200 },
    });
    adminStatsHistoryMock.mockResolvedValue({
      error: {},
      response: { status: 503, statusText: "Service Unavailable" },
    });
    const w = await mountPage({ tab: "activity" });
    expect(w.get('[data-testid="history-error"]').text()).toContain(
      "Could not load activity: 503 Service Unavailable",
    );
    expect(w.get('[data-testid="footnote"]').text()).toContain("registry_dashboard_url");
    adminStatsHistoryMock.mockResolvedValue(history([]));
    await w.get('[data-testid="history-error"] button').trigger("click");
    await flushPromises();
    expect(w.find('[data-testid="no-traffic"]').exists()).toBe(true);
  });

  it("hides Activity when stats are refused, and the cache cell reads —", async () => {
    adminStatsMock.mockResolvedValue({ error: {}, response: { status: 403 } });
    const w = await mountPage({ tab: "activity" });
    expect(w.find('[data-testid="live"]').exists()).toBe(false);
    expect(w.get('[data-testid="strip-cache"]').text()).toBe("—");
  });

  it("names an unknown registry", async () => {
    getRegistryViewMock.mockResolvedValue({ error: {}, response: { status: 404 } });
    const w = await mountPage();
    expect(w.get('[data-testid="missing"]').text()).toContain("No registry named `npm-q`.");
    expect(w.text()).toContain("Back to Registries");
  });

  it("says system:read is missing on a 403", async () => {
    getRegistryViewMock.mockResolvedValue({ error: {}, response: { status: 403 } });
    const w = await mountPage();
    expect(w.get('[data-testid="denied"]').text()).toBe("You do not have system:read.");
  });

  it("shows any other failure with a retry", async () => {
    getRegistryViewMock.mockResolvedValue({
      error: {},
      response: { status: 503, statusText: "Service Unavailable" },
    });
    const w = await mountPage();
    expect(w.get('[data-testid="error"]').text()).toContain("503 Service Unavailable");
    getRegistryViewMock.mockResolvedValue({ data: view(), response: { status: 200 } });
    await w.get('[data-testid="error"] button').trigger("click");
    await flushPromises();
    expect(w.find('[data-testid="strip"]').exists()).toBe(true);
  });

  it("reports a thrown request as an error", async () => {
    getRegistryViewMock.mockRejectedValue(new Error("network down"));
    const w = await mountPage();
    expect(w.get('[data-testid="error"]').text()).toContain("network down");
  });
});
