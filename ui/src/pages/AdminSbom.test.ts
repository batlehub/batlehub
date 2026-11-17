import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { listRegistriesMock, authFetchMock } = vi.hoisted(() => ({
  listRegistriesMock: vi.fn(),
  authFetchMock: vi.fn(),
}));
vi.mock("@/client/sdk.gen", () => ({ listRegistries: listRegistriesMock }));
vi.mock("@/composables/useAuthFetch", () => ({
  useAuthFetch: () => ({ authFetch: authFetchMock }),
}));

import AdminSbom from "./AdminSbom.vue";
import { useAuth } from "@/composables/useAuth";

async function mountPage() {
  const wrapper = mount(AdminSbom, { global: { stubs: {} } });
  await flushPromises();
  return wrapper;
}

const vm = (w: Awaited<ReturnType<typeof mountPage>>) =>
  w.vm as unknown as { fromDate: string; toDate: string; errorMsg: string | null };

/**
 * The page's question: "give me a bill of materials for this window".
 *
 * §4.3's assertion for it is that `from > to` is refused at the edge — the
 * endpoint answers a backwards range with a perfectly valid *empty* SBOM, and
 * an empty answer that reads as a fact is this RFC's recurring defect. On a
 * file an operator may hand to an auditor it is the worst version of it.
 */
describe("AdminSbom", () => {
  beforeEach(() => {
    listRegistriesMock.mockReset().mockResolvedValue({ data: [{ name: "npm", type: "npm" }] });
    authFetchMock.mockReset().mockResolvedValue({
      ok: true,
      blob: async () => new Blob(["{}"]),
      headers: new Headers(),
    });
  });

  it("refuses a backwards range instead of exporting an empty file", async () => {
    const wrapper = await mountPage();
    vm(wrapper).fromDate = "2026-08-12";
    vm(wrapper).toDate = "2026-08-01";
    await flushPromises();

    expect(wrapper.text()).toMatch(/after the end date/i);
    const button = wrapper.findAll("button").find((b) => /download sbom/i.test(b.text()))!;
    expect(button.attributes("disabled")).toBeDefined();

    // And it does not reach the network even if pressed.
    await button.trigger("click");
    await flushPromises();
    expect(authFetchMock).not.toHaveBeenCalled();
  });

  it("exports a well-ordered range", async () => {
    const wrapper = await mountPage();
    vm(wrapper).fromDate = "2026-08-01";
    vm(wrapper).toDate = "2026-08-12";
    await flushPromises();

    await wrapper
      .findAll("button")
      .find((b) => /download sbom/i.test(b.text()))!
      .trigger("click");
    await flushPromises();
    expect(authFetchMock).toHaveBeenCalledTimes(1);
  });

  /** A single date is not a range and must not be refused as one. */
  it("allows an open-ended window", async () => {
    const wrapper = await mountPage();
    vm(wrapper).fromDate = "2026-08-01";
    await flushPromises();
    expect(wrapper.text()).not.toMatch(/after the end date/i);
  });

  /**
   * §6.2: the registry filter is a `Select` over the registries that exist,
   * not a box whose placeholder guessed a naming convention.
   */
  it("offers the registries rather than asking for one to be typed", async () => {
    const wrapper = await mountPage();
    expect(wrapper.find("input#sbom-registry").exists()).toBe(false);
    expect(wrapper.find("#sbom-registry").exists()).toBe(true);
  });

  it("links to the flags only for a viewer who may read them", async () => {
    useAuth().identity.value = { verbs: ["system:read"], registries: {} } as never;
    expect((await mountPage()).find('[data-testid="sbom-flags-link"]').exists()).toBe(false);
    useAuth().identity.value = { verbs: ["system:read", "flags:read"], registries: {} } as never;
    expect((await mountPage()).find('[data-testid="sbom-flags-link"]').exists()).toBe(true);
    useAuth().identity.value = null;
  });
});
