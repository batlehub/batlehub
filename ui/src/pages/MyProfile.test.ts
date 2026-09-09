import { describe, it, expect, vi } from "vitest";
import { mount } from "@vue/test-utils";
import { ref } from "vue";

type Identity = { user_id?: string; role?: string; auth_provider?: string; groups?: string[] };
// Real refs: the template reads `identity?.user_id` directly, and Vue only
// unwraps something `isRef` recognises — a `{ value }` literal renders blank.
const auth = {
  identity: ref<Identity | null>(null),
  oidcProvider: ref(""),
};
vi.mock("@/composables/useAuth", () => ({ useAuth: () => auth }));

import MyProfile from "./MyProfile.vue";

function mountProfile(identity: Identity | null, oidcProvider = "") {
  auth.identity.value = identity;
  auth.oidcProvider.value = oidcProvider;
  return mount(MyProfile);
}

/**
 * `/me/profile`: who the server thinks you are, and which groups it resolved
 * for you. Everything on it comes from the session rather than a request, so
 * the tests are about what it does with an identity that is partly missing —
 * which is the normal case for a static-token user.
 */
describe("MyProfile", () => {
  it("states the user id, the role and the provider", () => {
    const w = mountProfile({
      user_id: "alice",
      role: "admin",
      auth_provider: "oidc",
      groups: [],
    });
    expect(w.text()).toContain("alice");
    expect(w.text()).toContain("admin");
    expect(w.text()).toContain("oidc");
  });

  /**
   * Two providers can share an issuer URL, in which case the validator that
   * accepted the token is not the button the user clicked. The stored provider
   * is the one that answers "how did I get in".
   */
  it("prefers the provider stored at login over the one that validated the token", () => {
    const w = mountProfile({ user_id: "alice", role: "user", auth_provider: "oidc2" }, "authentik");
    expect(w.text()).toContain("authentik");
    expect(w.text()).not.toContain("oidc2");
  });

  it("falls back to anonymous when there is no identity at all", () => {
    const w = mountProfile(null);
    expect(w.text()).toContain("anonymous");
    expect(w.text()).toContain("—");
  });

  it("splits a provider-prefixed group from a plain one", () => {
    const w = mountProfile({
      user_id: "alice",
      role: "user",
      groups: ["oidc:team-a", "maintainers"],
    });
    const items = w.findAll("li").map((li) => li.text());
    expect(items).toHaveLength(2);
    // The prefix is shown as its own badge, so `team-a` reads as the group name.
    expect(items[0]).toContain("oidc");
    expect(items[0]).toContain("team-a");
    expect(items[1]).toBe("maintainers");
    // And the count next to the heading agrees with the list.
    expect(w.text()).toContain("(2)");
  });

  it("says no groups were assigned rather than showing an empty list", () => {
    const w = mountProfile({ user_id: "alice", role: "user", groups: [] });
    expect(w.findAll("li")).toHaveLength(0);
    expect(w.text()).toMatch(/no groups/i);
  });
});
