import { mount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";

import UserMenu from "./UserMenu.vue";

/**
 * The menu is the console's only home for surfaces demoted out of the primary
 * bar, which makes "is it actually listed here" a correctness question rather
 * than a cosmetic one.
 *
 * RFC 0003 removed `/tools` from the primary nav on the stated grounds that
 * diagnostics stay "reachable from the user menu" (`config/navigation.ts`), and
 * `config/navigation.test.ts` pins the removal for all three viewers. The other
 * half was never written: no entry existed here, so the two tool routes were
 * reachable only by typing a URL or from `PackageDetailPage`'s denial link. The
 * removal had a gate; the promise that justified it had none — which is the
 * shape RFC 0004-bis §2 is about.
 */

vi.mock("vue-router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("vue-router")>()),
  useRouter: () => ({ push: vi.fn() }),
}));

const logout = vi.fn();

vi.mock("@/composables/useAuth", () => ({
  useAuth: () => ({
    identity: { value: { user_id: "alice", role: "admin", auth_provider: "oidc" } },
    isAdmin: { value: true },
    isAuthenticated: { value: true },
    isOidcSession: { value: true },
    logout,
  }),
}));

/**
 * Radix renders the content only while open, and jsdom has no pointer to open
 * it with. Stubbing the primitives as pass-through wrappers is what makes the
 * *contents* assertable; the primitives' own behaviour is Radix's to test.
 */
const stubs = {
  RouterLink: { props: ["to"], template: "<a :href='to'><slot/></a>" },
  DropdownMenuRoot: { template: "<div><slot/></div>" },
  DropdownMenuTrigger: { template: "<button><slot/></button>" },
  DropdownMenuContent: { template: "<div><slot/></div>" },
  DropdownMenuItem: { template: "<div><slot/></div>" },
  DropdownMenuSeparator: { template: "<hr/>" },
  DropdownMenuLabel: { template: "<div><slot/></div>" },
};

const hrefs = () =>
  mount(UserMenu, { global: { stubs } })
    .findAll("a")
    .map((a) => a.attributes("href"));

describe("UserMenu", () => {
  it("lists the diagnostics hub, the promise that demoting it out of the nav made", () => {
    expect(hrefs()).toContain("/tools/access-check");
  });

  /**
   * `/tools` is a `SECTION_INDEXES` key, not a route — linking it would put a
   * redirect in a menu, which is the rule `AppHeader` states for the bar and
   * has no reason to stop applying one level in.
   */
  it("points at the first tab rather than the section index", () => {
    expect(hrefs()).not.toContain("/tools");
  });

  it("names the hub what the hub calls itself", () => {
    const wrapper = mount(UserMenu, { global: { stubs } });
    const link = wrapper.findAll("a").find((a) => a.attributes("href") === "/tools/access-check");
    expect(link?.text()).toBe("Tools");
  });

  it("still lists every account surface", () => {
    expect(hrefs()).toEqual(
      expect.arrayContaining(["/me/profile", "/me/tokens", "/me/namespace", "/me/cli"]),
    );
  });
});
