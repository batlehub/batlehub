import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { authFetchMock, listRegistriesMock } = vi.hoisted(() => ({
  authFetchMock: vi.fn(),
  listRegistriesMock: vi.fn(),
}));
vi.mock("@/composables/useAuthFetch", () => ({
  useAuthFetch: () => ({ authFetch: authFetchMock }),
}));
vi.mock("@/client/sdk.gen", () => ({ listRegistries: listRegistriesMock }));

import AdminWarming from "./AdminWarming.vue";

const warmable = (name: string) => ({ name, latest_n: 3, concurrency: 4 });

function routes(warmResult: Record<string, unknown>) {
  // Reset, not just re-implement: the call log is what "nothing was posted" is
  // asserted against, and it otherwise carries over from the previous test.
  authFetchMock.mockReset();
  authFetchMock.mockImplementation((url: string) =>
    Promise.resolve({
      ok: true,
      json: async () =>
        url.endsWith("/warming") ? { registries: [warmable("npm"), warmable("jb")] } : warmResult,
    }),
  );
}

async function mountPage() {
  const wrapper = mount(AdminWarming, { global: { stubs: {} } });
  await flushPromises();
  return wrapper;
}

const warmRow = (w: Awaited<ReturnType<typeof mountPage>>, registry: string) =>
  w.findAll("tbody tr").find((r) => r.text().includes(registry))!;

/**
 * The page's question: "pre-fetch these artifacts now".
 *
 * §4.3's assertion is that a warm failure names the registry; §6.1 adds that
 * the fields follow `registry_type` rather than every registry offering both a
 * package box and a path box, one of which it cannot accept.
 */
describe("AdminWarming", () => {
  beforeEach(() => {
    listRegistriesMock.mockReset().mockResolvedValue({
      data: [
        { name: "npm", type: "npm" },
        { name: "jb", type: "jetbrains" },
      ],
    });
    routes({ warmed: 0, skipped: 0, errors: 0, failures: [] });
  });

  /**
   * A3: the count alone left an operator warming eleven registries with
   * "3 errors" and no way to learn which three without shell access to the
   * instance they are administering through this console.
   */
  it("names the packages a warm run failed on", async () => {
    routes({
      warmed: 1,
      skipped: 0,
      errors: 2,
      failures: [
        { package: "left-pad", version: "1.0.0", error: "404 from upstream" },
        { package: "lodash", version: null, error: "could not list versions" },
      ],
    });
    const wrapper = await mountPage();
    const row = warmRow(wrapper, "npm");
    await row.find("input").setValue("left-pad, lodash");
    await row.find("button").trigger("click");
    await flushPromises();

    expect(row.text()).toContain("left-pad");
    expect(row.text()).toContain("404 from upstream");
    // No version to name when the *listing* failed — naming one would be a guess.
    expect(row.text()).toContain("could not list versions");
  });

  /**
   * A count with nothing to name it is a panicked task, which the report counts
   * and cannot identify. Saying so beats a silent mismatch with the badge.
   */
  it("says so when an error has no package to name", async () => {
    routes({ warmed: 0, skipped: 0, errors: 1, failures: [] });
    const wrapper = await mountPage();
    const row = warmRow(wrapper, "npm");
    await row.find("input").setValue("left-pad");
    await row.find("button").trigger("click");
    await flushPromises();

    expect(row.text()).toMatch(/without reporting which package/i);
  });

  /**
   * §6.1 / PRODUCT principle 5: registry types are data. Eleven identical cards
   * put a JetBrains path placeholder on cargo and npm — a placeholder
   * suggesting an input the registry cannot accept.
   */
  it("offers a package field to a package registry and a path field to a path one", async () => {
    const wrapper = await mountPage();
    expect(warmRow(wrapper, "npm").find("input").attributes("placeholder")).toMatch(
      /@scope\/package/,
    );
    expect(warmRow(wrapper, "jb").find("input").attributes("placeholder")).toMatch(/idea/);
  });

  it("refuses an empty run rather than posting one", async () => {
    const wrapper = await mountPage();
    const row = warmRow(wrapper, "npm");
    await row.find("button").trigger("click");
    await flushPromises();

    expect(row.text()).toMatch(/at least one package or path/i);
    // Refused here, not by the server: nothing was posted to `/warm`.
    expect(authFetchMock.mock.calls.filter(([url]) => String(url).endsWith("/warm"))).toHaveLength(
      0,
    );
  });

  it("says nothing is configured rather than rendering an empty table", async () => {
    authFetchMock.mockResolvedValue({ ok: true, json: async () => ({ registries: [] }) });
    const wrapper = await mountPage();
    expect(wrapper.text()).toMatch(/warm_packages/);
  });
});
