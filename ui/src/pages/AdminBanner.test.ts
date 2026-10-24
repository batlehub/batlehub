import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { mount, flushPromises, type VueWrapper } from "@vue/test-utils";

const sdk = vi.hoisted(() => ({
  setBannerMock: vi.fn(),
  clearBannerMock: vi.fn(),
}));
vi.mock("@/client/sdk.gen", () => ({
  setBanner: sdk.setBannerMock,
  clearBanner: sdk.clearBannerMock,
}));

import AdminBanner from "./AdminBanner.vue";

/** `useBanner` polls raw `fetch`; this is what it finds. */
function stubBanner(body: unknown) {
  vi.stubGlobal(
    "fetch",
    vi.fn(() => Promise.resolve(new Response(JSON.stringify(body), { status: 200 }))),
  );
}

/** Unmounted in teardown: `useBanner` installs a 30 s poll cleared on unmount. */
const mounted: VueWrapper[] = [];

async function mountPage() {
  const wrapper = mount(AdminBanner, {
    global: { stubs: { SectionTabs: true, RouterLink: true } },
  });
  mounted.push(wrapper);
  await flushPromises();
  await flushPromises();
  return wrapper;
}

const button = (w: VueWrapper, label: string) =>
  w.findAll("button").find((b) => b.text().trim() === label);

/** The global banner, moved here from config reload with its behaviour intact. */
describe("AdminBanner", () => {
  beforeEach(() => {
    stubBanner(null);
    sdk.setBannerMock.mockReset().mockResolvedValue({ data: {} });
    sdk.clearBannerMock.mockReset().mockResolvedValue({ data: {} });
  });

  afterEach(() => {
    for (const wrapper of mounted.splice(0)) wrapper.unmount();
    vi.unstubAllGlobals();
  });

  it("refuses to set an empty banner", async () => {
    const wrapper = await mountPage();

    expect(button(wrapper, "Set Banner")?.attributes("disabled")).toBeDefined();
    await wrapper.get("#banner-message").setValue("   ");
    expect(button(wrapper, "Set Banner")?.attributes("disabled")).toBeDefined();
  });

  it("sets the banner at the chosen level and clears the field", async () => {
    const wrapper = await mountPage();

    await wrapper.get("#banner-message").setValue("Upgrading at 22:00 UTC");
    await wrapper.get("#banner-level").setValue("warning");
    await button(wrapper, "Set Banner")!.trigger("click");
    await flushPromises();

    expect(sdk.setBannerMock).toHaveBeenCalledWith({
      body: { message: "Upgrading at 22:00 UTC", level: "warning" },
    });
    expect(wrapper.text()).toContain("Banner set");
    expect((wrapper.get("#banner-message").element as HTMLInputElement).value).toBe("");
  });

  it("reports a failure to set the banner", async () => {
    sdk.setBannerMock.mockResolvedValue({ error: { message: "not permitted" } });
    const wrapper = await mountPage();

    await wrapper.get("#banner-message").setValue("hello");
    await button(wrapper, "Set Banner")!.trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("not permitted");
  });

  it("only offers to clear a banner that exists, and shows who set it", async () => {
    const noBanner = await mountPage();
    expect(noBanner.text()).toContain("No banner currently set.");
    expect(button(noBanner, "Clear Banner")?.attributes("disabled")).toBeDefined();

    stubBanner({ message: "Read-only mode", level: "error", set_at: "now", set_by: "alice" });
    const wrapper = await mountPage();

    expect(wrapper.text()).toContain("Read-only mode");
    expect(wrapper.text()).toContain("alice");

    await button(wrapper, "Clear Banner")!.trigger("click");
    await flushPromises();

    expect(sdk.clearBannerMock).toHaveBeenCalled();
    expect(wrapper.text()).toContain("Banner cleared");
  });

  it("reports a failure to clear the banner", async () => {
    stubBanner({ message: "Read-only mode", level: "error", set_at: "now", set_by: "alice" });
    sdk.clearBannerMock.mockResolvedValue({ error: { message: "storage unreachable" } });
    const wrapper = await mountPage();

    await button(wrapper, "Clear Banner")!.trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("storage unreachable");
  });
});
