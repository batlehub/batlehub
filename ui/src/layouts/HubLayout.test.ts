import { describe, it, expect, vi } from "vitest";
import { mount } from "@vue/test-utils";

const route = { path: "/me/profile" };
vi.mock("vue-router", () => ({
  useRoute: () => route,
  // `props: ["to"]` only, so everything else — `aria-current` included — falls
  // through to the anchor the way the real RouterLink renders it.
  RouterLink: { props: ["to"], template: "<a :href='to'><slot/></a>" },
  RouterView: { template: "<div data-testid='outlet' />" },
}));

import HubLayout from "./HubLayout.vue";

const TABS = [
  { to: "/me/profile", label: "account.profile" },
  { to: "/me/tokens", label: "account.tokens" },
];

const mountHub = (path = "/me/profile") => {
  route.path = path;
  return mount(HubLayout, { props: { title: "Account", description: "Your session", tabs: TABS } });
};

/**
 * The shell behind `/me` and `/tools`.
 *
 * Its tabs are routed rather than local state, so the assertion that matters is
 * that each one is a link with its own URL and that exactly one is marked
 * current — a tab strip that renders buttons is not bookmarkable, and one that
 * marks nothing current leaves a screen reader with no place in the page.
 */
describe("HubLayout", () => {
  it("renders the heading and description it is given", () => {
    const w = mountHub();
    expect(w.get("h1").text()).toBe("Account");
    expect(w.text()).toContain("Your session");
  });

  it("renders one link per tab, at its own URL", () => {
    const links = mountHub().findAll("nav a");
    expect(links.map((a) => a.attributes("href"))).toEqual(["/me/profile", "/me/tokens"]);
  });

  it("marks the tab matching the current route, and only that one", () => {
    const current = mountHub("/me/tokens")
      .findAll("nav a")
      .filter((a) => a.attributes("aria-current") === "page");
    expect(current).toHaveLength(1);
    expect(current[0].attributes("href")).toBe("/me/tokens");
  });

  it("treats a child route as its tab being current", () => {
    // `/me/tokens/new` is still the tokens tab; a strict equality test would
    // leave the strip with nothing marked once a tab grows a sub-route.
    const w = mountHub("/me/tokens/new");
    const current = w.findAll("nav a").filter((a) => a.attributes("aria-current") === "page");
    expect(current.map((a) => a.attributes("href"))).toEqual(["/me/tokens"]);
  });

  it("labels the tab strip and renders the routed tab below it", () => {
    const w = mountHub();
    expect(w.get("nav").attributes("aria-label")).toContain("Account");
    expect(w.find('[data-testid="outlet"]').exists()).toBe(true);
  });
});
