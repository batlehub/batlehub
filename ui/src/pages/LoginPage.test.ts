import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises, type VueWrapper } from "@vue/test-utils";
import { createRouter, createMemoryHistory, type Router } from "vue-router";

// `useAuth` (imported transitively) calls the SDK's `me()` at module load, and
// the page itself calls `me`/`listOidcProviders`. Mock the SDK + client so no
// real `fetch` runs and we control the responses the page renders from.
const { meMock, listOidcProvidersMock, oidcRefreshMock, setConfigMock } = vi.hoisted(() => ({
  meMock: vi.fn(),
  listOidcProvidersMock: vi.fn(),
  oidcRefreshMock: vi.fn(),
  setConfigMock: vi.fn(),
}));

vi.mock("@/client/sdk.gen", () => ({
  me: meMock,
  listOidcProviders: listOidcProvidersMock,
  oidcRefresh: oidcRefreshMock,
}));
vi.mock("@/client/client.gen", () => ({ client: { setConfig: setConfigMock } }));

import LoginPage from "./LoginPage.vue";
import { useAuth } from "@/composables/useAuth";

const ANON = { role: "anonymous" as const, groups: [], has_registry_access: true, verbs: [], registries: {} };
const USER = { role: "user" as const, groups: [], has_registry_access: true, verbs: [], registries: {} };

function makeRouter(): Router {
  const stub = { template: "<div />" };
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: "/login", component: stub },
      { path: "/packages", component: stub },
    ],
  });
}

async function mountLogin(
  initial: string | { path: string; query?: Record<string, string> } = "/login",
): Promise<{ wrapper: VueWrapper; router: Router; push: ReturnType<typeof vi.spyOn> }> {
  const router = makeRouter();
  await router.push(initial);
  await router.isReady();
  const push = vi.spyOn(router, "push");
  const wrapper = mount(LoginPage, { global: { plugins: [router] } });
  await flushPromises();
  return { wrapper, router, push };
}

function buttonByText(wrapper: VueWrapper, text: string) {
  return wrapper.findAll("button").find((b) => b.text().includes(text));
}

describe("LoginPage (integration)", () => {
  beforeEach(() => {
    meMock.mockReset().mockResolvedValue({ data: ANON });
    listOidcProvidersMock.mockReset().mockResolvedValue({ data: [] });
    oidcRefreshMock.mockReset();
    setConfigMock.mockClear();
    useAuth().logout();
    localStorage.clear();
    sessionStorage.clear();
  });

  it("renders one OIDC button per configured provider, labelled by name", async () => {
    listOidcProvidersMock.mockResolvedValue({
      data: [{ name: "keycloak" }, { name: "github-sso" }],
    });
    const { wrapper } = await mountLogin();
    const labels = wrapper.findAll("button").map((b) => b.text());
    expect(labels).toContain("Sign in with Keycloak");
    expect(labels).toContain("Sign in with Github Sso");
  });

  it("uses a single generic OIDC label when exactly one provider is configured", async () => {
    listOidcProvidersMock.mockResolvedValue({ data: [{ name: "keycloak" }] });
    const { wrapper } = await mountLogin();
    expect(wrapper.text()).toContain("Sign in with OIDC");
    expect(wrapper.text()).not.toContain("Sign in with Keycloak");
  });

  it("renders no OIDC buttons (nor the divider) when no providers are configured", async () => {
    const { wrapper } = await mountLogin();
    expect(wrapper.text()).not.toContain("Sign in with OIDC");
    expect(wrapper.text()).not.toContain("or use a token");
    // The static-token form button remains.
    expect(wrapper.text()).toContain("Sign in with token");
  });

  it("surfaces an error forwarded from the OIDC callback via ?error", async () => {
    const { wrapper } = await mountLogin({
      path: "/login",
      query: { error: "State mismatch — possible CSRF attack." },
    });
    expect(wrapper.text()).toContain("State mismatch — possible CSRF attack.");
  });

  it("rejects an empty token submit without hitting the API", async () => {
    const { wrapper } = await mountLogin();
    await wrapper.find("form").trigger("submit.prevent");
    await flushPromises();
    expect(wrapper.text()).toContain("Please enter a token.");
    expect(meMock).not.toHaveBeenCalled();
  });

  it("signs in with a valid non-anonymous token, persists it, and navigates", async () => {
    meMock.mockResolvedValue({ data: USER });
    const { wrapper, push } = await mountLogin();
    await wrapper.find("#token").setValue("good-token");
    await wrapper.find("form").trigger("submit.prevent");
    await flushPromises();
    expect(setConfigMock).toHaveBeenCalledWith({ auth: "good-token" });
    expect(localStorage.getItem("batlehub_access_token")).toBe("good-token");
    expect(push).toHaveBeenCalledWith("/packages");
  });

  it("honours the ?redirect destination after a successful sign-in", async () => {
    meMock.mockResolvedValue({ data: USER });
    const { wrapper, push } = await mountLogin({
      path: "/login",
      query: { redirect: "/admin/health" },
    });
    await wrapper.find("#token").setValue("good-token");
    await wrapper.find("form").trigger("submit.prevent");
    await flushPromises();
    expect(push).toHaveBeenCalledWith("/admin/health");
  });

  it("rejects a token that only grants anonymous access", async () => {
    meMock.mockResolvedValue({ data: ANON });
    const { wrapper } = await mountLogin();
    await wrapper.find("#token").setValue("anon-token");
    await wrapper.find("form").trigger("submit.prevent");
    await flushPromises();
    expect(wrapper.text()).toContain("anonymous access");
    expect(localStorage.getItem("batlehub_access_token")).toBeNull();
  });

  it("rejects an invalid token when the probe call fails", async () => {
    meMock.mockRejectedValue(new Error("401 Unauthorized"));
    const { wrapper } = await mountLogin();
    await wrapper.find("#token").setValue("bad-token");
    await wrapper.find("form").trigger("submit.prevent");
    await flushPromises();
    expect(wrapper.text()).toContain("Invalid token");
    expect(localStorage.getItem("batlehub_access_token")).toBeNull();
  });

  it("lets the user continue anonymously to /packages", async () => {
    const { wrapper, push } = await mountLogin();
    await buttonByText(wrapper, "Continue without signing in")!.trigger("click");
    expect(push).toHaveBeenCalledWith("/packages");
  });

  it("redirects an already-authenticated visitor away from the login page", async () => {
    // Seed an authenticated session before mounting.
    meMock.mockResolvedValue({ data: USER });
    const auth = useAuth();
    auth.token.value = "existing";
    auth.identity.value = USER;
    await flushPromises();
    expect(auth.isAuthenticated.value).toBe(true);

    const router = makeRouter();
    await router.push("/login");
    await router.isReady();
    const replace = vi.spyOn(router, "replace");
    mount(LoginPage, { global: { plugins: [router] } });
    await flushPromises();

    // onMounted short-circuits straight to the default redirect.
    expect(replace).toHaveBeenCalledWith("/packages");
  });
});
