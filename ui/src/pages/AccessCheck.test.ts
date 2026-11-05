import { mount, flushPromises } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";

const { checkAccessMock } = vi.hoisted(() => ({ checkAccessMock: vi.fn() }));
vi.mock("@/client/sdk.gen", () => ({
  checkAccess: checkAccessMock,
  listRegistries: vi.fn().mockResolvedValue({ data: [] }),
  explorePackages: vi.fn().mockResolvedValue({ data: { items: [] } }),
  listPackageVersions: vi.fn().mockResolvedValue({ data: { items: [] } }),
}));

import AccessCheck from "./AccessCheck.vue";
import { Badge } from "@/components/ui/badge";
import { Resolution } from "@/components/ui/resolution";

/**
 * The denial → diagnostics path (RFC 0003 §4.4).
 *
 * Nobody opens an access checker for fun; they open it because something was
 * refused. Arriving with the coordinate already filled in is the difference
 * between a tool that gets used and one that gets closed — retyping what you
 * just read on the previous screen is exactly the friction that makes a
 * diagnostic sit unused while people guess instead.
 */
const mountWith = (query: Record<string, string>) =>
  mount(AccessCheck, {
    global: {
      mocks: { $route: { query } },
      stubs: { RouterLink: true },
    },
  });

vi.mock("vue-router", () => ({
  useRoute: () => mockRoute,
  RouterLink: { template: "<a><slot/></a>" },
}));

let mockRoute: { query: Record<string, unknown> } = { query: {} };

/**
 * The registry field is a `Select` since RFC 0004-bis §6.2 — the set is closed,
 * small and already fetched, and four pages were each guessing a different
 * naming convention in a placeholder. Its value lives in the component's state
 * rather than in an `<input>`, so the prefill is asserted there; the two
 * free-text coordinates are still read off the DOM.
 */
const registryValue = (wrapper: ReturnType<typeof mountWith>) =>
  (wrapper.vm as unknown as { registry: string }).registry;

describe("AccessCheck prefill", () => {
  it("fills the coordinate from the query it was linked with", () => {
    mockRoute = { query: { registry: "npm1", name: "left-pad", version: "1.3.0" } };
    const wrapper = mountWith({});

    expect(registryValue(wrapper)).toBe("npm1");
    expect((wrapper.find("#name").element as HTMLInputElement).value).toBe("left-pad");
    expect((wrapper.find("#version").element as HTMLInputElement).value).toBe("1.3.0");
  });

  /** Opened directly from the nav, it still offers a usable starting point. */
  it("falls back to its defaults when opened without a query", () => {
    mockRoute = { query: {} };
    const wrapper = mountWith({});

    expect(registryValue(wrapper)).toBe("github");
    expect((wrapper.find("#name").element as HTMLInputElement).value).toBe("");
  });

  /** A repeated query param arrives as an array; it must not render as one. */
  it("ignores a non-string query value rather than rendering it", () => {
    mockRoute = { query: { registry: ["npm1", "npm2"] } };
    const wrapper = mountWith({});

    expect(registryValue(wrapper)).toBe("github");
  });
});

/**
 * The answer, in three channels.
 *
 * `--destructive` resolves to `--accent` (`assets/index.css`), so
 * `variant="default"` and `variant="destructive"` paint the same crimson. This
 * page's only job is to answer "was I allowed?", and it rendered both answers
 * identically — one channel, hue, saying nothing.
 *
 * DESIGN.md: "Never colour alone — pattern, word and hue all carry it."
 */
describe("AccessCheck answer", () => {
  type Headline = "pullable" | "waiting" | "refused";
  const answerWith = async (
    headline: Headline,
    causes: Array<Record<string, unknown>> = [],
    until: string | null = null,
  ) => {
    mockRoute = { query: {} };
    checkAccessMock.mockResolvedValue({
      data: {
        package: { registry: "npm1", name: "left-pad", version: "1.4.0", artifact: null },
        can_access: headline === "pullable",
        reason: null,
        proxy_url: null,
        headline,
        until,
        causes,
        resolved_version: null,
        versions: [],
      },
    });
    const wrapper = mountWith({});
    // Not `find("button")`: the registry Select and both Comboboxes render
    // triggers of their own, and the first one in the DOM is a radix element.
    const submit = wrapper.findAll("button").find((b) => /check access/i.test(b.text()))!;
    await submit.trigger("click");
    await flushPromises();
    return wrapper;
  };

  it("does not paint a pullable answer in the refusal hue", async () => {
    const wrapper = await answerWith("pullable");
    expect(wrapper.findComponent(Badge).props("variant")).toBe("known");
  });

  it("paints a refusal in the refusal hue, and a wait in the held one", async () => {
    expect((await answerWith("refused")).findComponent(Badge).props("variant")).toBe("destructive");
    expect((await answerWith("waiting")).findComponent(Badge).props("variant")).toBe("copper");
  });

  /**
   * The channel that survives a monochrome display and a colour-blind reader.
   * `Resolution`'s matrix is `aria-hidden` and it requires its own `label`, so
   * this costs nothing in the accessible name — the word is still the word.
   */
  it("carries the answer in the pattern as well as the hue", async () => {
    const allowed = await answerWith("pullable");
    expect(allowed.findComponent(Resolution).props("state")).toBe("cached");
    expect(allowed.find("[data-testid=resolution-matrix]").attributes("aria-hidden")).toBe("true");
    expect((await answerWith("waiting")).findComponent(Resolution).props("state")).toBe("held");
    expect((await answerWith("refused")).findComponent(Resolution).props("state")).toBe("blocked");
  });

  it("still says the answer in words", async () => {
    expect((await answerWith("pullable")).text()).toContain("Pullable");
    expect((await answerWith("refused")).text()).toContain("Not pullable");
  });

  /** RFC 0040 §4.8: every cause, in the server's order, each with its word. */
  it("lists every cause in order with its state word, code and next step", async () => {
    const wrapper = await answerWith(
      "waiting",
      [
        { kind: "access", state: "pass", detail: "you may read npm1", reason_code: null },
        {
          kind: "verdict",
          state: "wait",
          detail: "quarantined — MIN_AGE_NOT_MET",
          reason_code: "MIN_AGE_NOT_MET",
          next: "`batlehub-cli wait` returns when it is served",
        },
      ],
      "2026-11-04T14:00:00Z",
    );
    const items = wrapper.findAll("ol li").map((li) => li.text());
    expect(items).toHaveLength(2);
    expect(items[0]).toContain("pass");
    expect(items[0]).toContain("access");
    expect(items[1]).toContain("wait");
    expect(items[1]).toContain("MIN_AGE_NOT_MET");
    expect(wrapper.text()).toContain("pullable from");
    expect(wrapper.text()).toContain("returns when it is served");
  });

  /** T1: an invisible package and an unknown one are one sentence. */
  it("says a 404 as not found, whatever the body", async () => {
    mockRoute = { query: {} };
    checkAccessMock.mockResolvedValue({
      data: undefined,
      error: { error: "not found", message: "package not found" },
      response: { status: 404 },
    });
    const wrapper = mountWith({});
    const submit = wrapper.findAll("button").find((b) => /check access/i.test(b.text()))!;
    await submit.trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("No such package, or not one you can see.");
  });
});
