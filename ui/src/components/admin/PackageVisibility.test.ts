import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { getVisibilityMock, setVisibilityMock } = vi.hoisted(() => ({
  getVisibilityMock: vi.fn(),
  setVisibilityMock: vi.fn(),
}));

vi.mock("@/client/sdk.gen", () => ({
  getPackageVisibility: getVisibilityMock,
  setPackageVisibility: setVisibilityMock,
}));

import PackageVisibility from "./PackageVisibility.vue";

async function mountComp() {
  const wrapper = mount(PackageVisibility, { props: { registry: "npm", name: "lodash" } });
  await flushPromises();
  return wrapper;
}

describe("PackageVisibility", () => {
  beforeEach(() => {
    getVisibilityMock.mockReset().mockResolvedValue({ data: { visibility: "public" } });
    setVisibilityMock.mockReset().mockResolvedValue({ data: { visibility: "internal" } });
  });

  it("loads and displays the current visibility", async () => {
    const wrapper = await mountComp();
    expect(getVisibilityMock).toHaveBeenCalledWith({
      path: { registry: "npm", name: "lodash" },
    });
    expect(wrapper.text()).toContain("public");
  });

  it("disables Save until the selection changes", async () => {
    const wrapper = await mountComp();
    const save = wrapper.findAll("button").find((b) => b.text().includes("Save"))!;
    expect(save.attributes("disabled")).toBeDefined();
  });

  it("saves the new visibility and reloads", async () => {
    const wrapper = await mountComp();
    const vm = wrapper.vm as unknown as { selected: string; save: () => Promise<void> };
    vm.selected = "internal";
    await vm.save();
    await flushPromises();
    expect(setVisibilityMock).toHaveBeenCalledWith({
      path: { registry: "npm", name: "lodash" },
      body: { visibility: "internal" },
    });
    expect(getVisibilityMock).toHaveBeenCalledTimes(2);
  });

  it("says the load failed rather than showing the fallback visibility", async () => {
    getVisibilityMock.mockResolvedValueOnce({ error: { message: "no such package" } });
    const wrapper = await mountComp();
    expect(wrapper.get('[role="alert"]').text()).toContain("no such package");
    expect(wrapper.findAll("button").some((b) => b.text().includes("Save"))).toBe(false);
  });

  it("shows an error message when saving fails", async () => {
    setVisibilityMock.mockResolvedValueOnce({ error: { message: "nope" } });
    const wrapper = await mountComp();
    const vm = wrapper.vm as unknown as { selected: string; save: () => Promise<void> };
    vm.selected = "internal";
    await vm.save();
    await flushPromises();
    expect(wrapper.text()).toContain("nope");
  });
});
