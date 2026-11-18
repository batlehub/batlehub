import { describe, it, expect, vi } from "vitest";
import { mount } from "@vue/test-utils";
import { createRouter, createMemoryHistory, type Router } from "vue-router";
import SectionTabs from "./SectionTabs.vue";

const TABS = [
  { to: "/admin/security/users", label: "Users" },
  { to: "/admin/security/ip-blocks", label: "IP Blocks" },
];

function makeRouter(initialPath: string): Router {
  const stub = { template: "<div />" };
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: "/admin/security/users", component: stub },
      { path: "/admin/security/ip-blocks", component: stub },
    ],
  });
  router.push(initialPath);
  return router;
}

describe("SectionTabs", () => {
  it("renders one option per tab and no desktop link row", async () => {
    const router = makeRouter("/admin/security/users");
    await router.isReady();
    const wrapper = mount(SectionTabs, {
      props: { tabs: TABS },
      global: { plugins: [router] },
    });
    const options = wrapper.findAll("option");
    expect(options.map((o) => o.text())).toEqual(["Users", "IP Blocks"]);
    // The sidebar lists these pages from `md` up; a second row would repeat it.
    expect(wrapper.findAll("a")).toHaveLength(0);
  });

  /** On a phone the tabs are one native selector naming the current one. */
  it("shows the current tab in the mobile selector", async () => {
    const router = makeRouter("/admin/security/ip-blocks");
    await router.isReady();
    const wrapper = mount(SectionTabs, {
      props: { tabs: TABS },
      global: { plugins: [router] },
    });
    expect((wrapper.get("select").element as HTMLSelectElement).value).toBe(
      "/admin/security/ip-blocks",
    );
  });

  it("navigates when another tab is picked in the mobile selector", async () => {
    const router = makeRouter("/admin/security/users");
    await router.isReady();
    const push = vi.spyOn(router, "push");
    const wrapper = mount(SectionTabs, {
      props: { tabs: TABS },
      global: { plugins: [router] },
    });
    await wrapper.get("select").setValue("/admin/security/ip-blocks");
    expect(push).toHaveBeenCalledWith("/admin/security/ip-blocks");
  });

  /** WCAG 3.2.2: an arrow key on the closed select must not load a page. */
  it("waits for Enter when the tab is changed from the keyboard", async () => {
    const router = makeRouter("/admin/security/users");
    await router.isReady();
    const push = vi.spyOn(router, "push");
    const wrapper = mount(SectionTabs, {
      props: { tabs: TABS },
      global: { plugins: [router] },
    });
    const select = wrapper.get("select");
    await select.trigger("keydown", { key: "ArrowDown" });
    await select.setValue("/admin/security/ip-blocks");
    expect(push).not.toHaveBeenCalled();
    await select.trigger("keydown", { key: "Enter" });
    expect(push).toHaveBeenCalledWith("/admin/security/ip-blocks");
  });
});
