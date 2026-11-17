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
import { ADMIN_SECTIONS } from "@/config/adminSections";

function mountAdmin(path = "/admin/dashboard") {
  localStorage.clear();
  route.path = path;
  return mount(AdminLayout, { attachTo: document.body });
}

/** Mounted on a store the previous visit left behind. */
function remount(path: string) {
  route.path = path;
  return mount(AdminLayout, { attachTo: document.body });
}

function hrefsIn(w: ReturnType<typeof mountAdmin>, selector: string) {
  return w.findAll(`${selector} a`).map((a) => a.attributes("href"));
}

/**
 * The admin shell (RFC 0050 §5.3): `ConsoleLayout` fed by `ADMIN_SECTIONS`.
 * One <nav> serves both widths — the sidebar from `md` up, the phone menu
 * below — so there is no second list to drift from the first.
 */
describe("AdminLayout", () => {
  it("offers a delegate only the pages and sections of their verbs (RFC 0042)", () => {
    held.verbs = ["audit:read"];
    try {
      const w = mountAdmin("/admin/system/audit-log");
      expect(hrefsIn(w, "#admin-nav")).toEqual(["/admin/system/audit-log"]);
    } finally {
      held.verbs = null;
    }
  });

  /** RFC 0046 M1: every page one click away, grouped under its section. */
  it("lists every admin page, under its section (M1)", () => {
    const w = mountAdmin();
    expect(hrefsIn(w, "#admin-nav")).toEqual(
      ADMIN_SECTIONS.flatMap((section) => section.pages.map((page) => page.to)),
    );
    expect(w.findAll('#admin-nav [role="group"]')).toHaveLength(ADMIN_SECTIONS.length - 1);
    expect(w.get("#admin-nav").text()).toContain("Blocked accounts & addresses");
  });

  /** RFC 0050 §4.2: the current group is open, the others closed until opened. */
  it("opens the current group only, and its header cannot close it", () => {
    const w = mountAdmin("/admin/system/health");
    const buttons = w.findAll("#admin-nav button[aria-expanded]");
    // Every group but System is a toggle, closed.
    expect(buttons).toHaveLength(ADMIN_SECTIONS.length - 2);
    for (const b of buttons) expect(b.attributes("aria-expanded")).toBe("false");
    expect(w.find("#admin-group-admin-system-label").element.tagName).toBe("SPAN");
    expect(w.get("#admin-group-admin-system-pages").isVisible()).toBe(true);
    expect(w.get("#admin-group-admin-access-pages").isVisible()).toBe(false);
  });

  it("remembers an opened group across a reload, per browser", async () => {
    const w = mountAdmin("/admin/system/health");
    const access = w.get('button[aria-controls="admin-group-admin-access-pages"]');
    await access.trigger("click");
    expect(access.attributes("aria-expanded")).toBe("true");
    expect(JSON.parse(localStorage.getItem("batlehub.nav.admin") ?? "")).toEqual(["/admin/access"]);
    // The reload: the same store, a fresh shell.
    const again = remount("/admin/system/health");
    expect(again.get("#admin-group-admin-access-pages").isVisible()).toBe(true);
    await again.get('button[aria-controls="admin-group-admin-access-pages"]').trigger("click");
    expect(JSON.parse(localStorage.getItem("batlehub.nav.admin") ?? "")).toEqual([]);
  });

  /** T10: a crafted store naming a hidden group renders nothing of it. */
  it("never renders a group the viewer may not read, whatever the store says", () => {
    localStorage.setItem("batlehub.nav.admin", JSON.stringify(["/admin/access", 7]));
    held.verbs = ["audit:read"];
    try {
      const w = remount("/admin/system/audit-log");
      expect(w.find("#admin-group-admin-access-pages").exists()).toBe(false);
    } finally {
      held.verbs = null;
    }
  });

  it("falls back to the current group alone when the store cannot be read", () => {
    localStorage.setItem("batlehub.nav.admin", JSON.stringify(["/admin/access"]));
    vi.stubGlobal("localStorage", {
      getItem: () => {
        throw new Error("private window");
      },
    });
    try {
      const w = remount("/admin/packages/all");
      expect(w.get("#admin-group-admin-packages-pages").isVisible()).toBe(true);
      expect(w.get("#admin-group-admin-access-pages").isVisible()).toBe(false);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("stacks on mobile and only becomes a row from md up", () => {
    const root = mountAdmin().findAll("div")[0];
    expect(root.classes()).toContain("flex-col");
    expect(root.classes()).toContain("md:flex-row");
  });

  it("marks the page, sub-routes included", () => {
    const w = mountAdmin("/admin/registry/npm");
    const current = w
      .findAll("#admin-nav a")
      .filter((a) => a.attributes("aria-current") === "page")
      .map((a) => a.attributes("href"));
    // One registry's page belongs to the Registries list (M3, M4).
    expect(current).toEqual(["/admin/registries"]);
  });

  /** "All registries" is a prefix of its siblings: only the longest match is current. */
  it("marks only the page itself under a page that prefixes it", () => {
    const w = mountAdmin("/admin/registries/upstream");
    const current = w
      .findAll("#admin-nav a")
      .filter((a) => a.attributes("aria-current") === "page")
      .map((a) => a.attributes("href"));
    expect(current).toEqual(["/admin/registries/upstream"]);
  });

  /**
   * RFC 0050 §4.2: on a phone the trigger names the section and the page, and
   * opens the same tree — pages included, since the tab strips are gone.
   */
  it("names section and page on the phone trigger and opens the tree on demand", async () => {
    const w = mountAdmin("/admin/system/configuration");
    const trigger = w.get('button[aria-controls="admin-nav"]');
    expect(trigger.text()).toContain("System");
    expect(trigger.text()).toContain("Configuration");
    expect(trigger.attributes("aria-expanded")).toBe("false");
    expect(w.get("#admin-nav").classes()).toContain("hidden");

    await trigger.trigger("click");
    expect(trigger.attributes("aria-expanded")).toBe("true");
    expect(w.get("#admin-nav").classes()).not.toContain("hidden");
    expect(hrefsIn(w, "#admin-nav")).toContain("/admin/system/health");
  });

  it("renders the routed admin page", () => {
    expect(mountAdmin().find('[data-testid="outlet"]').exists()).toBe(true);
  });
});
