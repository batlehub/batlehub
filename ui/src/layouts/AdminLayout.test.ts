import { describe, it, expect, vi } from "vitest";
import { mount } from "@vue/test-utils";

const route = { path: "/admin/dashboard" };
vi.mock("vue-router", () => ({
  useRoute: () => route,
  RouterLink: { props: ["to"], template: "<a :href='to'><slot/></a>" },
  RouterView: { template: "<div data-testid='outlet' />" },
}));

import AdminLayout from "./AdminLayout.vue";
import { ADMIN_SIDEBAR } from "@/config/adminSections";

function mountAdmin(path = "/admin/dashboard") {
  route.path = path;
  return mount(AdminLayout);
}

/**
 * The admin shell: a sidebar from `md` up, a horizontal strip below it.
 *
 * The layout assertion is not decoration. The container was a flex *row* at
 * every width, so under `md` the full-width mobile strip sat beside the content
 * and every admin page scrolled sideways on a phone — for as long as it did, no
 * design gate was measuring that width.
 */
describe("AdminLayout", () => {
  it("stacks on mobile and only becomes a row from md up", () => {
    // The template opens with a comment, so the component's root is a fragment
    // and `wrapper.element` is that comment rather than the container.
    const root = mountAdmin().findAll("div")[0];
    expect(root.classes()).toContain("flex-col");
    expect(root.classes()).toContain("md:flex-row");
  });

  it("offers every admin section in both the sidebar and the mobile strip", () => {
    const hrefs = mountAdmin()
      .findAll("a")
      .map((a) => a.attributes("href"));
    for (const link of ADMIN_SIDEBAR) {
      // Once in the sidebar, once in the strip: one is hidden at any width.
      expect(hrefs.filter((h) => h === link.to)).toHaveLength(2);
    }
  });

  it("marks the section the route is in, including its sub-routes", () => {
    const active = mountAdmin("/admin/packages/bulk")
      .findAll("a")
      .filter((a) => a.classes().includes("font-semibold"))
      .map((a) => a.attributes("href"));
    expect(new Set(active)).toEqual(new Set(["/admin/packages"]));
  });

  it("renders the routed admin page", () => {
    expect(mountAdmin().find('[data-testid="outlet"]').exists()).toBe(true);
  });
});
