import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { registryHealthMock, clearCacheMock, invalidateMock } = vi.hoisted(() => ({
  registryHealthMock: vi.fn(),
  clearCacheMock: vi.fn(),
  invalidateMock: vi.fn(),
}));
vi.mock("@/client/sdk.gen", () => ({
  registryHealth: registryHealthMock,
  clearRegistryCache: clearCacheMock,
  invalidateExploreCache: invalidateMock,
}));
// RFC 0014's card reads the audit listing; `503` — the audit not running in
// this process — is the default, and the card stays away.
const { authFetchMock } = vi.hoisted(() => ({ authFetchMock: vi.fn() }));
vi.mock("@/composables/useAuthFetch", () => ({
  useAuthFetch: () => ({ authFetch: authFetchMock }),
}));

import AdminHealth from "./AdminHealth.vue";

const health = (over: Record<string, unknown> = {}) => ({
  registry: "npm",
  registry_type: "npm",
  package_count: 3,
  cached_artifact_count: 2,
  total_size_bytes: 1024,
  last_pull_at: "2026-08-12T10:00:00Z",
  pulls_last_hour: 1,
  pulls_last_day: 4,
  recent_errors: [],
  access: { roles: ["admin"], groups: [] },
  mode: "proxy",
  beta_channel_enabled: false,
  ...over,
});

async function mountPage() {
  const wrapper = mount(AdminHealth, {
    global: { stubs: { SectionTabs: true, RouterLink: true, DeleteCachedArtifact: true } },
  });
  await flushPromises();
  return wrapper;
}

/**
 * The page's question: "what state is each registry in right now".
 *
 * It had no test at all, so the RFC 0004 Phase 5 pass had route-matrix coverage
 * and axe and nothing else (RFC 0004-bis §2.3).
 */
describe("AdminHealth", () => {
  beforeEach(() => {
    registryHealthMock.mockReset().mockResolvedValue({ data: [health()] });
    clearCacheMock.mockReset().mockResolvedValue({ data: { cleared: 1 } });
    invalidateMock.mockReset().mockResolvedValue({ data: {} });
    authFetchMock.mockReset().mockResolvedValue({ ok: false, status: 503 });
  });

  /**
   * RFC 0014 §4.6: the policy the instance applies to a confirmed
   * disappearance is on the health page, so a blocked-package question never
   * starts with "check the config file". Without the audit, no card.
   */
  it("shows the upstream audit's policy and counts when the audit runs here", async () => {
    const w = await mountPage();
    expect(w.find('[data-testid="upstream-card"]').exists()).toBe(false);

    authFetchMock.mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({
        items: [],
        total: 3,
        page: 0,
        per_page: 1,
        policy: "block",
        registries: ["npm"],
        counts: [{ registry: "npm", missing: 2, disappeared: 1 }],
      }),
    });
    const w2 = await mountPage();
    const card = w2.find('[data-testid="upstream-card"]');
    expect(card.exists()).toBe(true);
    expect(card.text()).toContain("block");
    expect(card.find('[data-testid="upstream-card-count"]').text()).toContain("npm");
    expect(card.text()).toContain("2 missing");
    expect(card.text()).toContain("1 disappeared");
  });

  /**
   * A2: "0 cached, last pull never" reads identically for a broken proxy and
   * for a healthy `local` registry that has nothing to pull by definition. The
   * mode is what tells them apart, and the console used to fetch a second
   * endpoint to find out.
   */
  it("states each registry's mode", async () => {
    registryHealthMock.mockResolvedValue({
      data: [
        health({ registry: "npm", mode: "proxy" }),
        health({ registry: "priv", mode: "local" }),
      ],
    });
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("proxy");
    expect(wrapper.text()).toContain("local");
  });

  it("flags a registry with a beta channel", async () => {
    registryHealthMock.mockResolvedValue({ data: [health({ beta_channel_enabled: true })] });
    const wrapper = await mountPage();
    expect(wrapper.text()).toMatch(/beta channel/i);
  });

  /**
   * §6.1: the aggregate card is gone. It restated `adminStats().aggregate` —
   * the four numbers `AdminDashboard` states as a sentence — as four tiles
   * without the trend, and its Refresh button did not refresh it.
   */
  it("does not restate the dashboard's aggregate", async () => {
    const wrapper = await mountPage();
    expect(wrapper.text()).not.toMatch(/cache performance since/i);
  });

  /**
   * §4.3: an errors table that reflows without pushing the document.
   *
   * The assertion is on the structure that makes that true — the table is
   * inside its own scroll container, per DESIGN.md's Own-Container Overflow
   * Rule — rather than on a class name, which a `DESIGN.md` migration would
   * re-break while teaching nothing.
   */
  it("keeps the errors table inside its own scroll container", async () => {
    registryHealthMock.mockResolvedValue({
      data: [
        health({
          recent_errors: [
            {
              timestamp: "2026-08-12T09:00:00Z",
              user_id: "alice",
              package_name: "left-pad",
              version: "1.0.0",
              error_type: "denied",
              reason: "blocked",
            },
          ],
        }),
      ],
    });
    const wrapper = await mountPage();
    await wrapper
      .findAll("button")
      .find((b) => /error/i.test(b.text()))!
      .trigger("click");
    await flushPromises();

    const table = wrapper.find("table");
    expect(table.exists()).toBe(true);
    // Some ancestor between the table and the card must clip horizontally.
    let node: HTMLElement | null = table.element.parentElement;
    let clipped = false;
    while (node && !clipped) {
      clipped = /overflow-x-auto|overflow-auto|overflow-x-scroll/.test(node.className ?? "");
      node = node.parentElement;
    }
    expect(clipped, "the errors table must scroll in its own box").toBe(true);
  });

  it("says a registry has no errors rather than showing an empty table", async () => {
    const wrapper = await mountPage();
    expect(wrapper.text()).toMatch(/no errors/i);
  });

  it("surfaces a load error rather than an empty page", async () => {
    registryHealthMock.mockResolvedValue({ error: { message: "db unreachable" } });
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("db unreachable");
  });
});
