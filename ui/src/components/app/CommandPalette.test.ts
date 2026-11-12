import { afterEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { nextTick, ref } from "vue";
import { createMemoryHistory, createRouter } from "vue-router";

/**
 * `can(verb, registry?)` as `useAuth` answers it. The registry-only viewer of
 * RFC 0046 §7.1 T2 holds `system:read` on one registry and nothing at the
 * instance tier: the palette must call `can(verb)` the way the guard does, and
 * so offer them no admin page and no hub.
 */
const auth = {
  instance: [] as string[],
  perRegistry: {} as Record<string, string[]>,
  hasRegistryAccess: true,
};
vi.mock("@/composables/useAuth", () => ({
  useAuth: () => ({
    can: (verb: string, registry?: string) =>
      registry !== undefined
        ? (auth.perRegistry[registry] ?? auth.instance).includes(verb)
        : auth.instance.includes(verb),
    canAny: (verbs: readonly string[]) => verbs.some((v) => auth.instance.includes(v)),
    identity: ref({ has_registry_access: auth.hasRegistryAccess }),
    isAuthenticated: ref(true),
    isOidcSession: ref(false),
  }),
}));

const regs = {
  registries: ref([{ name: "npm", type: "npm", mode: "proxy" }]),
  loading: ref(false),
  error: ref<string | null>(null),
  reload: vi.fn(),
};
vi.mock("@/composables/useRegistryOptions", () => ({ useRegistryOptions: () => regs }));

import CommandPalette from "./CommandPalette.vue";
import { ADMIN_PAGE_VERBS } from "@/config/adminSections";
import { isPaletteShortcut } from "@/composables/usePalette";

const ALL = [...new Set(Object.values(ADMIN_PAGE_VERBS))];

function makeRouter() {
  return createRouter({
    history: createMemoryHistory(),
    routes: [{ path: "/:p(.*)*", component: { template: "<div />" } }],
  });
}

async function openPalette(query = "") {
  const router = makeRouter();
  const w = mount(CommandPalette, {
    props: { open: false, "onUpdate:open": (v: boolean) => w.setProps({ open: v }) },
    global: { plugins: [router] },
    attachTo: document.body,
  });
  await w.setProps({ open: true });
  await flushPromises();
  const input = document.querySelector<HTMLInputElement>('input[role="combobox"]');
  if (!input) throw new Error("palette input not rendered");
  input.value = query;
  input.dispatchEvent(new Event("input"));
  await nextTick();
  return { w, router, input };
}

const rows = () => [...document.querySelectorAll<HTMLElement>('[role="option"]')];
const hrefs = () => rows().map((r) => r.getAttribute("href"));

afterEach(() => {
  document.body.innerHTML = "";
  auth.instance = [];
  auth.perRegistry = {};
  regs.error.value = null;
  regs.loading.value = false;
  regs.reload.mockClear();
});

describe("CommandPalette", () => {
  it("offers a registry-only viewer no admin page and no hub (T2, T3)", async () => {
    auth.perRegistry = { npm: ["system:read", "config:read"] };
    await openPalette("npm");
    expect(hrefs().some((h) => h?.startsWith("/admin/"))).toBe(false);
    expect(hrefs()).toContain("/packages?registry=npm");
  });

  it("draws each row as a link to the resolved location", async () => {
    auth.instance = ALL;
    await openPalette("npm");
    expect(rows().every((r) => r.tagName === "A")).toBe(true);
    expect(hrefs()).toContain("/admin/registry/npm");
    expect(hrefs().at(-1)).toBe("/packages?q=npm");
  });

  it("opens the active row on Enter, and in a new tab on Ctrl+Enter", async () => {
    auth.instance = ALL;
    const open = vi.spyOn(window, "open").mockReturnValue(null);
    const { router, input } = await openPalette("rescan");
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true }));
    expect(open).toHaveBeenCalledWith("/admin/security/scans?tab=failures", "_blank", "noopener");
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter" }));
    await flushPromises();
    expect(router.currentRoute.value.fullPath).toBe("/admin/security/scans?tab=failures");
    open.mockRestore();
  });

  it("states a failed registry list and refetches it on the next opening", async () => {
    auth.instance = ALL;
    regs.error.value = "boom";
    const { w } = await openPalette("npm");
    expect(document.body.textContent).toContain("Registries could not be loaded.");
    expect(regs.reload).toHaveBeenCalledTimes(1);
    await w.setProps({ open: false });
    await w.setProps({ open: true });
    expect(regs.reload).toHaveBeenCalledTimes(2);
  });

  it("follows the composable's list: a new viewer sees only their registries", async () => {
    auth.instance = ALL;
    await openPalette("npm");
    expect(hrefs()).toContain("/admin/registry/npm");
    regs.registries.value = [{ name: "npm-other", type: "npm", mode: "local" }];
    await nextTick();
    expect(hrefs()).not.toContain("/admin/registry/npm");
    expect(hrefs()).toContain("/admin/registry/npm-other");
    regs.registries.value = [{ name: "npm", type: "npm", mode: "proxy" }];
  });

  it("turns a pasted token into no URL (§7.3)", async () => {
    auth.instance = ALL;
    await openPalette("bh_" + "x".repeat(40));
    expect(hrefs().some((h) => h?.includes("bh_"))).toBe(false);
    expect(document.body.textContent).toContain("Nothing matches.");
  });

  it("draws no heading for a viewer with no admin verb, and says what to type", async () => {
    await openPalette("");
    expect(document.querySelectorAll('[role="group"]')).toHaveLength(0);
    expect(document.body.textContent).toContain("Type a package or registry name.");
    expect(hrefs().some((h) => h?.startsWith("/admin/"))).toBe(false);
  });

  it("shows an admin the common tasks on an empty query", async () => {
    auth.instance = ALL;
    await openPalette("");
    expect(document.body.textContent).toContain("Common tasks");
    expect(rows()).toHaveLength(12);
  });
});

/** §4.2 and §7.1 T6: never in a textarea, always from a single-line input. */
describe("isPaletteShortcut", () => {
  const press = (target: HTMLElement, init: KeyboardEventInit = { key: "k", ctrlKey: true }) => {
    const event = new KeyboardEvent("keydown", init);
    Object.defineProperty(event, "target", { value: target });
    return isPaletteShortcut(event);
  };

  it("works from a single-line input and the page, with Ctrl or ⌘", () => {
    expect(press(document.createElement("input"))).toBe(true);
    expect(press(document.body, { key: "k", metaKey: true })).toBe(true);
  });

  it("does nothing in a textarea or contenteditable", () => {
    expect(press(document.createElement("textarea"))).toBe(false);
    const editable = document.createElement("div");
    editable.setAttribute("contenteditable", "true");
    expect(press(editable)).toBe(false);
  });

  it("needs the modifier, and only that one", () => {
    expect(press(document.body, { key: "k" })).toBe(false);
    expect(press(document.body, { key: "k", ctrlKey: true, shiftKey: true })).toBe(false);
  });
});
