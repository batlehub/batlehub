import { describe, it, expect, vi } from "vitest";
import { mount } from "@vue/test-utils";

const auth = { isOidcSession: { value: false } };
vi.mock("@/composables/useAuth", () => ({ useAuth: () => auth }));

import AccountLayout from "./AccountLayout.vue";

const tabsOf = (w: ReturnType<typeof mount>) =>
  (w.findComponent({ name: "HubLayout" }).props("tabs") as Array<{ to: string }>).map((t) => t.to);

function mountAccount(oidcSession: boolean) {
  auth.isOidcSession.value = oidcSession;
  return mount(AccountLayout, {
    global: {
      stubs: {
        HubLayout: {
          name: "HubLayout",
          props: ["title", "tabs", "description"],
          template: "<div/>",
        },
      },
    },
  });
}

/**
 * `/me`'s tab strip, whose only decision is whether the tokens tab is there.
 *
 * API tokens are minted against an OIDC session; a static-token user who
 * followed the tab would land on a page its guard bounces them off. Hiding it
 * is the difference between a surface that is not for you and a surface that
 * looks like it is and then refuses.
 */
describe("AccountLayout", () => {
  it("offers the tokens tab to a signed-in OIDC user", () => {
    expect(tabsOf(mountAccount(true))).toContain("/me/tokens");
  });

  it("hides the tokens tab from a static-token user", () => {
    const tabs = tabsOf(mountAccount(false));
    expect(tabs).not.toContain("/me/tokens");
    // The rest of the hub is still there: this hides one tab, not the page.
    expect(tabs).toEqual(["/me/profile", "/me/namespace", "/me/cli"]);
  });
});
