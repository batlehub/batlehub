import { describe, it, expect, vi } from "vitest";
import { mount } from "@vue/test-utils";

const route = { path: "/security" };
vi.mock("vue-router", () => ({
  useRoute: () => route,
  RouterLink: { props: ["to"], template: "<a :href='to'><slot/></a>" },
  RouterView: { template: "<div data-testid='outlet' />" },
}));
const held = { verbs: [] as string[] };
vi.mock("@/composables/useAuth", () => ({
  useAuth: () => ({ can: (verb: string) => held.verbs.includes(verb) }),
}));

import SecurityLayout from "./SecurityLayout.vue";

function mountAt(path: string, verbs: string[]) {
  localStorage.clear();
  route.path = path;
  held.verbs = verbs;
  return mount(SecurityLayout, { attachTo: document.body });
}
const hrefs = (w: ReturnType<typeof mountAt>) =>
  w.findAll("#security-nav a").map((a) => a.attributes("href"));
const current = (w: ReturnType<typeof mountAt>) =>
  w
    .findAll("#security-nav a")
    .filter((a) => a.attributes("aria-current") === "page")
    .map((a) => a.attributes("href"));

/** RFC 0050 §5.3: the security space is the admin's shell fed another table. */
describe("SecurityLayout", () => {
  it("shows a flags-only viewer the overview and Flags, nothing else (§4.2)", () => {
    const w = mountAt("/security/flags", ["flags:read"]);
    expect(hrefs(w)).toEqual(["/security", "/security/flags"]);
  });

  it("marks the overview only on the overview, though it prefixes every page", () => {
    const all = ["quarantine:read", "findings:read", "flags:read", "audit:read"];
    expect(current(mountAt("/security", all))).toEqual(["/security"]);
    expect(current(mountAt("/security/holds", all))).toEqual(["/security/holds"]);
  });

  it("opens the group of the current page, one registry's coverage included", () => {
    const w = mountAt("/security/coverage/npm", ["quarantine:read"]);
    expect(current(w)).toEqual(["/security/coverage"]);
    expect(w.get("#security-group-security-follow-up-pages").isVisible()).toBe(true);
    expect(w.get("#security-group-security-triage-pages").isVisible()).toBe(false);
  });

  it("keeps its open groups apart from the admin's", async () => {
    const w = mountAt("/security", ["quarantine:read"]);
    await w.get('button[aria-controls="security-group-security-triage-pages"]').trigger("click");
    expect(localStorage.getItem("batlehub.nav.security")).toBe('["/security/triage"]');
    expect(localStorage.getItem("batlehub.nav.admin")).toBeNull();
  });
});
