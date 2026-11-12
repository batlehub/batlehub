import { describe, it, expect, vi } from "vitest";
import { mount } from "@vue/test-utils";

const route = { path: "/admin/dashboard" };
vi.mock("vue-router", () => ({
  useRoute: () => route,
  RouterLink: { props: ["to"], template: "<a :href='to'><slot/></a>" },
  RouterView: { template: "<div data-testid='outlet' />" },
}));

const held = { verbs: null as string[] | null };
vi.mock("@/composables/useAuth", () => ({
  useAuth: () => ({ can: (verb: string) => held.verbs === null || held.verbs.includes(verb) }),
}));

import AdminLayout from "./AdminLayout.vue";
import { ADMIN_SECTIONS, ADMIN_SIDEBAR } from "@/config/adminSections";

function mountAdmin(path = "/admin/dashboard") {
  route.path = path;
  return mount(AdminLayout);
}

function hrefsIn(w: ReturnType<typeof mountAdmin>, selector: string) {
  return w.findAll(`${selector} a`).map((a) => a.attributes("href"));
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
  it("offers a delegate only the pages and sections of their verbs (RFC 0042)", () => {
    held.verbs = ["audit:read"];
    try {
      const w = mountAdmin("/admin/system/audit-log");
      expect(hrefsIn(w, "aside")).toEqual(["/admin/system/audit-log"]);
      expect(hrefsIn(w, "#admin-sections-mobile")).toEqual(["/admin/system"]);
    } finally {
      held.verbs = null;
    }
  });

  /** RFC 0046 M1: every page one click away, grouped under its section. */
  it("lists every admin page in the desktop sidebar, under its section (M1)", () => {
    const w = mountAdmin();
    expect(hrefsIn(w, "aside")).toEqual(
      ADMIN_SECTIONS.flatMap((section) => section.pages.map((page) => page.to)),
    );
    // Headings are text: not links, not focusable, so Tab moves page to page.
    const headings = w.findAll('aside [role="group"] > span');
    expect(headings).toHaveLength(ADMIN_SECTIONS.length - 1);
    for (const heading of headings) expect(heading.attributes("tabindex")).toBeUndefined();
    expect(w.get("aside").text()).toContain("Blocked accounts & addresses");
  });

  /** RFC 0046 M1′: a section shows only the pages held, never a greyed-out row. */
  it("shows a security group its two pages and nothing else (M1′)", () => {
    held.verbs = ["quarantine:read", "findings:read"];
    try {
      const w = mountAdmin("/admin/security/scans");
      expect(hrefsIn(w, "aside")).toEqual(["/admin/security/scans", "/admin/security/findings"]);
      expect(w.findAll('aside [role="group"]')).toHaveLength(1);
    } finally {
      held.verbs = null;
    }
  });

  it("stacks on mobile and only becomes a row from md up", () => {
    // The template opens with a comment, so the component's root is a fragment
    // and `wrapper.element` is that comment rather than the container.
    const root = mountAdmin().findAll("div")[0];
    expect(root.classes()).toContain("flex-col");
    expect(root.classes()).toContain("md:flex-row");
  });

  /** RFC 0046 M1″: the phone menu stays a list of sections, not ~23 rows. */
  it("keeps the mobile menu a list of sections (M1″)", () => {
    expect(hrefsIn(mountAdmin(), "#admin-sections-mobile")).toEqual(
      ADMIN_SIDEBAR.map((link) => link.to),
    );
  });

  it("marks the page on desktop and its section on mobile, sub-routes included", () => {
    const w = mountAdmin("/admin/registry/npm");
    // The position is announced, not only painted.
    const current = (selector: string) =>
      w
        .findAll(`${selector} a`)
        .filter((a) => a.attributes("aria-current") === "page")
        .map((a) => a.attributes("href"));
    expect(current("aside")).toEqual(["/admin/registries"]);
    // One registry's page belongs to the Registries section (M3, M4).
    expect(current("#admin-sections-mobile")).toEqual(["/admin/registries"]);
  });

  /** "All registries" is a prefix of its siblings: only the longest match is current. */
  it("marks only the page itself under a page that prefixes it", () => {
    const w = mountAdmin("/admin/registries/upstream");
    const current = w
      .findAll("aside a")
      .filter((a) => a.attributes("aria-current") === "page")
      .map((a) => a.attributes("href"));
    expect(current).toEqual(["/admin/registries/upstream"]);
  });

  /**
   * On a phone the sections sit behind one trigger that names where you are,
   * instead of four wrapped rows of links above the page.
   */
  it("names the current section on the mobile trigger and opens the list on demand", async () => {
    const w = mountAdmin("/admin/system/configuration");
    const trigger = w.get('button[aria-controls="admin-sections-mobile"]');
    expect(trigger.text()).toContain("System");
    expect(trigger.attributes("aria-expanded")).toBe("false");
    expect((w.get("#admin-sections-mobile").element as HTMLElement).style.display).toBe("none");

    await trigger.trigger("click");
    expect(trigger.attributes("aria-expanded")).toBe("true");
    expect((w.get("#admin-sections-mobile").element as HTMLElement).style.display).toBe("");
  });

  it("renders the routed admin page", () => {
    expect(mountAdmin().find('[data-testid="outlet"]').exists()).toBe(true);
  });
});
