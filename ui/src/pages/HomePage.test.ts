import { mount, flushPromises } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";

/**
 * The home route greets its viewer, and that greeting is now the page's one
 * Display element — the step DESIGN.md allows once per view. Two things about
 * it are correctness rather than taste:
 *
 *   - the wordmark is *not* also here. It used to be, a few centimetres under
 *     the identical wordmark in the bar, and removing it left the greeting as
 *     the only h1 the rendered design gate can find.
 *   - `user_id` is the provider's `user_id_claim`, which defaults to `sub` —
 *     a UUID on Keycloak. At 104px that is four lines of poster, so a long
 *     greeting steps down to Pixel Medium instead.
 */

const identity = { value: { user_id: "alice", role: "admin" } as Record<string, unknown> };

vi.mock("@/composables/useAuth", () => ({
  useAuth: () => ({
    identity,
    isAdmin: { value: false },
    can: () => false,
    isAuthenticated: { value: true },
    token: { value: "t" },
  }),
}));

const listRegistries = vi.fn(() =>
  Promise.resolve({ data: [{ name: "npm", mode: "proxy" }] } as unknown),
);
vi.mock("@/client/sdk.gen", () => ({ listRegistries: () => listRegistries() }));

import HomePage from "./HomePage.vue";

const stubs = {
  RouterLink: { template: "<a><slot /></a>" },
  AdvisoriesWidget: { template: "<section data-testid='widget' />" },
  QuotaWidget: { template: "<section data-testid='widget' />" },
  RecentPullsWidget: { template: "<section data-testid='widget' />" },
};

async function mountHome(userId: string) {
  identity.value = { user_id: userId, role: "admin" };
  const wrapper = mount(HomePage, { global: { stubs } });
  await flushPromises();
  return wrapper;
}

describe("HomePage", () => {
  it("greets the viewer by name, and does not repeat the wordmark", async () => {
    const h1 = (await mountHome("alice")).find("h1");
    expect(h1.text()).toBe("Hello alice.");
    expect(h1.classes()).toContain("text-display");
  });

  it("steps the greeting off the Display size when the name is a UUID", async () => {
    const h1 = (await mountHome("6f1d8c3e-2b7a-4f90-9d51-0a7c2e5b8f43")).find("h1");
    expect(h1.text()).toContain("6f1d8c3e-2b7a-4f90-9d51-0a7c2e5b8f43");
    expect(h1.classes()).not.toContain("text-display");
    expect(h1.classes()).toContain("text-2xl");
  });

  it("says once that the API is not answering, not once per section", async () => {
    listRegistries.mockRejectedValueOnce(new TypeError("Failed to fetch"));
    const w = await mountHome("alice");
    expect(w.findAll('[role="alert"]')).toHaveLength(1);
    expect(w.text()).toContain("The API is not answering");
    expect(w.text()).not.toContain("Failed to fetch");
    expect(w.findAll('[data-testid="widget"]')).toHaveLength(0);
  });

  it("shows the personal sections once the registry list loads", async () => {
    const w = await mountHome("alice");
    expect(w.findAll('[data-testid="widget"]')).toHaveLength(3);
  });
});
