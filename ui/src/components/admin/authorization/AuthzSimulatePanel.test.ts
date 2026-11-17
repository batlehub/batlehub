import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const mocks = vi.hoisted(() => ({
  adminAccessCheck: vi.fn(),
  listRegistries: vi.fn(),
  explorePackages: vi.fn(),
  explorePackageDetail: vi.fn(),
  listSubjects: vi.fn(),
}));
vi.mock("@/client/sdk.gen", () => mocks);

let mockRoute: { query: Record<string, unknown> } = { query: {} };
vi.mock("vue-router", () => ({
  useRoute: () => mockRoute,
  RouterLink: { template: "<a><slot/></a>" },
}));

const fetchSpy = vi.fn();
globalThis.fetch = fetchSpy as unknown as typeof fetch;

import AuthzSimulatePanel from "./AuthzSimulatePanel.vue";

const answer = (over: Record<string, unknown> = {}) => ({
  data: {
    decision: "allow",
    reason: null,
    rule_matched: null,
    blocked_by: null,
    covers: { rules: true, account_blocks: false, ip_blocks: false },
    ...over,
  },
});

async function mountPage() {
  const wrapper = mount(AuthzSimulatePanel);
  await flushPromises();
  return wrapper;
}

const vm = (w: Awaited<ReturnType<typeof mountPage>>) =>
  w.vm as unknown as { registry: string; packageName: string; version: string; userId: string };

/* Read off the component, not the page text: the client-IP field's own help
   sentence also says "not checked", and an assertion that cannot tell the two
   apart is not measuring the thing it names. */
const uncovered = (w: Awaited<ReturnType<typeof mountPage>>) =>
  (w.vm as unknown as { uncovered: string[] }).uncovered;

/**
 * The page's question: "would this identity be allowed?"
 *
 * §4.3 asks for two assertions: the query prefills, and the SDK is called
 * rather than `window.fetch`.
 */
describe("AuthzSimulatePanel (RFC 0050 §17.8)", () => {
  beforeEach(() => {
    mockRoute = { query: {} };
    fetchSpy.mockReset();
    mocks.adminAccessCheck.mockReset().mockResolvedValue(answer());
    mocks.listRegistries.mockReset().mockResolvedValue({ data: [{ name: "npm", type: "npm" }] });
    mocks.explorePackages.mockReset().mockResolvedValue({ data: { items: [], total: 0 } });
    mocks.explorePackageDetail.mockReset().mockResolvedValue({ data: { versions: [] } });
    mocks.listSubjects.mockReset().mockResolvedValue({ data: { items: [], truncated: false } });
  });

  /**
   * "Nobody opens an access checker for fun; they open it because something was
   * refused." An operator arriving from a denial in the audit log should not
   * retype every coordinate they just read.
   */
  it("prefills the coordinate from the query it was linked with", async () => {
    mockRoute = {
      query: { registry: "npm", name: "left-pad", version: "1.3.0", user_id: "oidc:alice" },
    };
    const wrapper = await mountPage();

    expect(vm(wrapper).registry).toBe("npm");
    expect(vm(wrapper).packageName).toBe("left-pad");
    expect(vm(wrapper).version).toBe("1.3.0");
    expect(vm(wrapper).userId).toBe("oidc:alice");
  });

  it("prefills the client address too, which Explain has no field for", async () => {
    mockRoute = { query: { client_ip: "10.0.0.7", role: "user" } };
    const wrapper = await mountPage();
    const state = wrapper.vm as unknown as { clientIp: string; role: string };
    expect(state.clientIp).toBe("10.0.0.7");
    expect(state.role).toBe("user");
  });

  /**
   * There is no `/api` proxy in front of the SPA, so a bare relative
   * `fetch("/api/v1/...")` POSTs to the Vite origin on both dev servers and on
   * every deployment where the API is not same-origin.
   */
  it("goes through the generated client, not window.fetch", async () => {
    const wrapper = await mountPage();
    await wrapper.find("form").trigger("submit");
    await flushPromises();

    expect(mocks.adminAccessCheck).toHaveBeenCalledTimes(1);
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  /**
   * A1: the simulator used to evaluate registry rules and nothing else, so an
   * admin who blocked `alice` on the next tab was told **allow**.
   */
  it("names the layer that denied, so the operator knows where to lift it", async () => {
    mocks.adminAccessCheck.mockResolvedValue(
      answer({
        decision: "deny",
        reason: "account 'alice' is blocked",
        blocked_by: "account",
        covers: { rules: true, account_blocks: true, ip_blocks: false },
      }),
    );
    const wrapper = await mountPage();
    await wrapper.find("form").trigger("submit");
    await flushPromises();

    expect(wrapper.text()).toMatch(/on the block list/i);
  });

  /**
   * B4: a simulation with no client address must not answer as though it had
   * one. An `allow` over two unconsulted layers reads identically to an `allow`
   * over three consulted ones.
   */
  it("states which layers the answer did not account for", async () => {
    const wrapper = await mountPage();
    await wrapper.find("form").trigger("submit");
    await flushPromises();

    // Named individually, not just "incomplete": the operator has to know
    // *which* field would close the gap.
    expect(uncovered(wrapper)).toEqual(["account blocks", "IP blocks"]);
  });

  it("claims no gap when every layer was consulted", async () => {
    mocks.adminAccessCheck.mockResolvedValue(
      answer({ covers: { rules: true, account_blocks: true, ip_blocks: true } }),
    );
    const wrapper = await mountPage();
    await wrapper.find("form").trigger("submit");
    await flushPromises();

    expect(uncovered(wrapper)).toEqual([]);
  });
});
