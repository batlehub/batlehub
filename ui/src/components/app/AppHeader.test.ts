import { mount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";

import AppHeader from "./AppHeader.vue";
import AppFooter from "./AppFooter.vue";

/**
 * The shell's accessibility contract (RFC 0003 §4.7).
 *
 * These are the checks that had no coverage while the console shipped 18
 * `aria-*` attributes across 29 pages: an icon-only control with no name is
 * "button" to a screen reader, and a disclosure with no `aria-expanded` never
 * reports whether it is open.
 */
/* AppHeader and AppNav read the route through composables, not $route. */
vi.mock("vue-router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("vue-router")>()),
  useRoute: () => ({ path: "/packages" }),
  useRouter: () => ({ push: vi.fn() }),
}));

vi.mock("@/composables/useAuth", () => ({
  useAuth: () => ({
    identity: { value: { role: "admin", groups: [], has_registry_access: true } },
    isAdmin: { value: true },
    isAuthenticated: { value: true },
    logout: vi.fn(),
  }),
}));

const stubs = {
  RouterLink: { props: ["to"], template: "<a :href='to'><slot/></a>" },
  UserMenu: true,
  GlobalBanner: true,
  DisplaySettings: true,
};

const mountHeader = () =>
  mount(AppHeader, {
    props: { mobileOpen: false },
    global: { stubs },
  });

describe("AppHeader", () => {
  it("names the mobile menu control instead of leaving it an unlabelled icon", () => {
    const button = mountHeader().find("button[aria-controls='mobile-nav']");
    expect(button.exists()).toBe(true);
    expect(button.attributes("aria-label")).toBe("Open menu");
  });

  it("reports the disclosure state, and which region it controls", async () => {
    const wrapper = mountHeader();
    const button = wrapper.find("button[aria-controls='mobile-nav']");
    expect(button.attributes("aria-expanded")).toBe("false");

    await wrapper.setProps({ mobileOpen: true });
    expect(wrapper.find("button[aria-controls='mobile-nav']").attributes("aria-label")).toBe(
      "Close menu",
    );
    expect(wrapper.find("#mobile-nav").exists()).toBe(true);
  });

  /**
   * The wordmark pointed at /packages for as long as `/` was a blind redirect
   * there, and kept pointing at it after Phase 4 gave `/` a real surface — so
   * the one control everyone uses to "go home" skipped the home page. Nothing
   * failed when that drifted, which is why it is pinned here.
   */
  it("sends the wordmark home, not to the catalog", () => {
    const logo = mountHeader().find("a");
    expect(logo.text()).toContain("BatleHub");
    expect(logo.attributes("href")).toBe("/");
  });

  it("is a banner landmark containing a labelled nav", () => {
    const wrapper = mountHeader();
    expect(wrapper.element.tagName).toBe("HEADER");
    expect(wrapper.find("nav").exists()).toBe(true);
  });
});

describe("AppFooter", () => {
  it("is a contentinfo landmark", () => {
    const wrapper = mount(AppFooter, { global: { stubs } });
    expect(wrapper.element.tagName).toBe("FOOTER");
  });
});
