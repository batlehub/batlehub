import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";
import { ref } from "vue";

const { listRegistriesMock } = vi.hoisted(() => ({ listRegistriesMock: vi.fn() }));
vi.mock("@/client/sdk.gen", () => ({ listRegistries: listRegistriesMock }));

const auth = {
  token: ref("tok-123"),
  identity: ref<{ user_id?: string } | null>({ user_id: "alice" }),
  isAuthenticated: ref(true),
  isAdmin: ref(false),
  expiresAt: ref(0),
};
vi.mock("@/composables/useAuth", () => ({ useAuth: () => auth }));

import SetupGuide from "./SetupGuide.vue";

type Registry = { name: string; type: string; mode?: string; public_url?: string | null };

const stubs = {
  CodeBlock: { name: "CodeBlock", props: ["code", "lang"], template: "<pre>{{ code }}</pre>" },
  RouterLink: { props: ["to"], template: "<a :href='to'><slot/></a>" },
};

async function mountPage(registries: Registry[]) {
  listRegistriesMock.mockResolvedValue({ data: registries });
  const w = mount(SetupGuide, { global: { stubs } });
  await flushPromises();
  return w;
}

const tabLabels = (w: Awaited<ReturnType<typeof mountPage>>) =>
  w.findAll('[role="tab"]').map((t) => t.text());

const snippets = (w: Awaited<ReturnType<typeof mountPage>>) =>
  w.findAllComponents({ name: "CodeBlock" }).map((c) => c.props("code") as string);

beforeEach(() => {
  listRegistriesMock.mockReset();
  auth.token.value = "tok-123";
  auth.identity.value = { user_id: "alice" };
  auth.isAuthenticated.value = true;
  auth.isAdmin.value = false;
  auth.expiresAt.value = 0;
});

/**
 * The page a new user is sent to: the snippet for their package manager,
 * already filled in with this server's URL and their own credentials.
 *
 * Everything on it is derived from the registry list, so the tests are about
 * that derivation — which tools are offered, which registry a snippet names,
 * and which host the credentials are filed under.
 */
describe("SetupGuide", () => {
  it("offers a tab only for registry types this server actually serves", async () => {
    const w = await mountPage([{ name: "py", type: "pypi" }]);
    const labels = tabLabels(w);
    expect(labels).toContain("PyPI");
    expect(labels).not.toContain("Cargo");
  });

  it("writes the server URL and the registry's own name into the snippet", async () => {
    const w = await mountPage([{ name: "py-mirror", type: "pypi" }]);
    const shown = snippets(w).join("\n");
    expect(shown).toContain("/proxy/py-mirror");
  });

  /**
   * A host-routed registry has its own hostname, and that is the URL a client
   * must use — the `/proxy/{name}` subpath is only the fallback for a server
   * without `[subdomain_routing]`.
   */
  it("prefers a host-routed registry's own URL over the proxy subpath", async () => {
    const w = await mountPage([
      { name: "py-mirror", type: "pypi", public_url: "https://pypi.hub.test" },
    ]);
    const shown = snippets(w).join("\n");
    expect(shown).toContain("https://pypi.hub.test");
    expect(shown).not.toContain("/proxy/py-mirror");
  });

  it("lets the user pick between two registries of the same type", async () => {
    const one = await mountPage([{ name: "py", type: "pypi" }]);
    expect(one.find("#setup-registry-pypi").exists()).toBe(false);

    const two = await mountPage([
      { name: "py", type: "pypi" },
      { name: "py-internal", type: "pypi" },
    ]);
    expect(two.find("#setup-registry-pypi").exists()).toBe(true);
  });

  // ── The .netrc tab ──────────────────────────────────────────────────────────

  it("files the credentials under the host the client talks to", async () => {
    const w = await mountPage([]);
    const stanzas = snippets(w).find((s) => s.startsWith("machine"))!;
    expect(stanzas).toContain("login alice");
    expect(stanzas).toContain("password tok-123");
  });

  it("adds a stanza per host-routed registry", async () => {
    // One stanza per distinct host: `.netrc` matches by hostname, so a snippet
    // naming only this origin leaves every host-routed registry unauthenticated.
    const w = await mountPage([
      { name: "py", type: "pypi", public_url: "https://pypi.hub.test" },
      { name: "npmjs", type: "npm", public_url: "https://npm.hub.test" },
    ]);
    // Radix switches on mousedown, not click: jsdom dispatches no pointer
    // sequence of its own, so the click alone leaves the first tab open.
    const tab = w.findAll('[role="tab"]').find((t) => t.text() === ".netrc")!;
    await tab.trigger("mousedown");
    await flushPromises();

    const stanzas = snippets(w).find((s) => s.startsWith("machine"))!;
    expect(stanzas).toContain("machine pypi.hub.test");
    expect(stanzas).toContain("machine npm.hub.test");
    expect(stanzas.match(/machine /g)).toHaveLength(3); // and this origin
  });

  it("hides the .netrc tab from a visitor who has no credentials", async () => {
    auth.isAuthenticated.value = false;
    const w = await mountPage([{ name: "py", type: "pypi" }]);
    expect(tabLabels(w)).not.toContain(".netrc");
  });

  it("points an OIDC session at a personal token instead of its session token", async () => {
    // A session token expires; a `.netrc` written with one stops working
    // silently the next morning.
    auth.expiresAt.value = Date.now() + 3600_000;
    const w = await mountPage([]);
    expect(w.find('a[href="/me/tokens"]').exists()).toBe(true);
  });

  // ── The empty and filtered states ───────────────────────────────────────────

  it("says there is nothing to connect to when a visitor can see no registry", async () => {
    auth.isAuthenticated.value = false;
    const w = await mountPage([]);
    expect(w.text()).toMatch(/nothing to connect to/i);
    expect(w.find('a[href="/admin/operations/config-reload"]').exists()).toBe(false);
  });

  it("offers an admin the config editor from that empty state", async () => {
    auth.isAuthenticated.value = false;
    auth.isAdmin.value = true;
    const w = await mountPage([]);
    expect(w.find('a[href="/admin/operations/config-reload"]').exists()).toBe(true);
  });

  it("narrows a long tool list by name, and says when nothing matches", async () => {
    const w = await mountPage(
      ["npm", "cargo", "pypi", "maven", "nuget", "rubygems", "composer", "conda"].map((type) => ({
        name: type,
        type,
      })),
    );
    // Past six tools the strip stops being a chooser, so a filter appears.
    const filter = w.get("#tool-filter");
    await filter.setValue("cargo");
    expect(tabLabels(w).filter((l) => l !== ".netrc")).toEqual(["Cargo"]);

    await filter.setValue("nothing-like-this");
    expect(tabLabels(w).filter((l) => l !== ".netrc")).toHaveLength(0);
    expect(w.text()).toContain("nothing-like-this");
  });

  it("shows a loading line before the registry list arrives", () => {
    listRegistriesMock.mockReturnValue(new Promise(() => {}));
    const w = mount(SetupGuide, { global: { stubs } });
    expect(w.text()).toMatch(/loading/i);
  });
});
