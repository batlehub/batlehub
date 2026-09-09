import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { mount, flushPromises, type VueWrapper } from "@vue/test-utils";

const {
  explorePackagesMock,
  exploreUpstreamSearchMock,
  listRegistriesMock,
  statsMock,
  exploreFetchVersionMock,
} = vi.hoisted(() => ({
  explorePackagesMock: vi.fn(),
  exploreUpstreamSearchMock: vi.fn(),
  listRegistriesMock: vi.fn(),
  statsMock: vi.fn(),
  exploreFetchVersionMock: vi.fn(),
}));
vi.mock("@/client/sdk.gen", () => ({
  explorePackages: explorePackagesMock,
  exploreUpstreamSearch: exploreUpstreamSearchMock,
  listRegistries: listRegistriesMock,
  exploreRegistryStats: statsMock,
  exploreFetchVersion: exploreFetchVersionMock,
}));

/**
 * The session the fetch button asks about (RFC 0007-bis §11 q3).
 *
 * Real refs, for the reason `PackageDetailPage.test.ts` records at length: a
 * plain `{ value }` object is truthy in a template however its `.value` reads,
 * so every `v-if` on an auth flag would be true and the flag untestable.
 */
const { authState } = vi.hoisted(() => ({
  authState: { token: "t", isAdmin: false, isAuthenticated: true },
}));
vi.mock("@/composables/useAuth", async () => {
  const { ref } = await import("vue");
  return {
    useAuth: () => ({
      token: ref(authState.token),
      isAdmin: ref(authState.isAdmin),
      isAuthenticated: ref(authState.isAuthenticated),
    }),
  };
});

/**
 * `routeState` stands in for the address bar: the page reads its whole starting
 * state out of `route.query` and writes every change back with `replace`, so a
 * test that cannot see both halves cannot tell whether a search survives a
 * return to the list.
 */
const { pushMock, replaceMock, routeState } = vi.hoisted(() => ({
  pushMock: vi.fn(),
  replaceMock: vi.fn(),
  routeState: { path: "/packages", query: {} as Record<string, string> },
}));
vi.mock("vue-router", () => ({
  useRouter: () => ({ push: pushMock, replace: replaceMock }),
  useRoute: () => routeState,
  RouterLink: { template: "<a :href='String(to)'><slot/></a>", props: ["to"] },
}));

import PackageCatalog from "./PackageCatalog.vue";
import { scopeExploreCacheTo, useExploreCache } from "@/composables/useExploreCache";

const entry = (name: string, over: Record<string, unknown> = {}) => ({
  registry: "npm",
  name,
  latest_version: "1.0.0",
  versions: 1,
  downloads: 10,
  last_accessed: null,
  source: "proxied",
  state: "cached",
  cached_versions: 1,
  cached_bytes: 4096,
  last_fetched_at: null,
  newest_version: "1.0.0",
  has_blocked: false,
  has_yanked: false,
  ...over,
});

const listing = (names: string[], total = names.length) => ({
  data: { items: names.map((n) => entry(n)), total, page: 0, per_page: 20 },
});

/** A listing of one row whose state the server graded. */
const graded = (state: string, over: Record<string, unknown> = {}) => ({
  data: {
    items: [entry("graded-pkg", { state, ...over })],
    total: 1,
    page: 0,
    per_page: 20,
  },
});

/** The href behind a row's package name — the row's one destination. */
function nameLink(wrapper: VueWrapper, name: string): string | undefined {
  const row = wrapper.findAll("tbody tr").find((r) => r.text().includes(name));
  return row
    ?.findAll("a")
    .find((a) => a.text().trim() === name)
    ?.attributes("href");
}

async function mountPage() {
  const wrapper = mount(PackageCatalog, {
    global: {
      // The `href` is rendered, not dropped: the destination of a row is now
      // carried by a link rather than by a `router.push` in a click handler,
      // so a stub that swallows `to` would make every navigation assertion
      // below unfalsifiable.
      stubs: { RouterLink: { template: "<a :href='String(to)'><slot /></a>", props: ["to"] } },
    },
  });
  await flushPromises();
  return wrapper;
}

/**
 * The page `/packages` never had a component test.
 *
 * `useExploreCache.test.ts` covered the composable in isolation — TTL, key
 * independence, `invalidate` — and never exercised it against the page, which
 * is why a store that survived logout passed a suite specifically written for
 * it (RFC 0004-bis §2.9).
 */
describe("PackageCatalog", () => {
  beforeEach(() => {
    vi.useRealTimers();
    explorePackagesMock.mockReset().mockResolvedValue(listing(["lodash"]));
    exploreUpstreamSearchMock.mockReset().mockResolvedValue({ data: { items: [] } });
    listRegistriesMock.mockReset().mockResolvedValue({ data: [{ name: "npm", type: "npm" }] });
    statsMock.mockReset().mockResolvedValue({ data: { registries: [] } });
    // A fresh viewer per test, which is also how the store is emptied between
    // them — the module-level map outlives a `mount`.
    scopeExploreCacheTo(`test-${Math.random()}`);
  });

  it("renders the rows the listing returns", async () => {
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("lodash");
  });

  it("issues no second request for a repeated search", async () => {
    const wrapper = await mountPage();
    expect(explorePackagesMock).toHaveBeenCalledTimes(1);

    // Sorting re-queries under a new key…
    await wrapper.find("select").setValue("name");
    await flushPromises();
    expect(explorePackagesMock).toHaveBeenCalledTimes(2);

    // …and going back to the default order reads the entry the first call
    // filled. `fetched`, not `downloads`: the catalog is ordered by last fetch,
    // which is what its caption claims and what the proof settles on.
    await wrapper.find("select").setValue("fetched");
    await flushPromises();
    expect(explorePackagesMock).toHaveBeenCalledTimes(2);
    expect(wrapper.text()).toContain("lodash");
  });

  /**
   * `packages.value = body.items` was unconditional — no sequence token, no
   * `AbortController` — and `onSortChange` is undebounced. So a slow first
   * request overwrote the table after a fast second had already filled it,
   * leaving the rows and the controls describing different queries.
   */
  it("a slow first response does not overwrite a fast second", async () => {
    let releaseSlow!: (v: unknown) => void;
    const slow = new Promise((resolve) => {
      releaseSlow = resolve;
    });

    explorePackagesMock
      .mockReturnValueOnce(slow) // initial mount: hangs
      .mockResolvedValue(listing(["fast-result"]));

    const wrapper = await mountPage();
    await wrapper.find("select").setValue("name");
    await flushPromises();
    expect(wrapper.text()).toContain("fast-result");

    // The superseded response lands last, and must not be displayed.
    releaseSlow(listing(["slow-result"]));
    await flushPromises();
    expect(wrapper.text()).toContain("fast-result");
    expect(wrapper.text()).not.toContain("slow-result");
  });

  /**
   * The late response is still *cached*: it is correct for its own key, only
   * stale for the screen. Discarding it would mean re-fetching the moment the
   * operator went back.
   */
  it("a superseded response is still written to the cache", async () => {
    let releaseSlow!: (v: unknown) => void;
    const slow = new Promise((resolve) => {
      releaseSlow = resolve;
    });
    explorePackagesMock.mockReturnValueOnce(slow).mockResolvedValue(listing(["fast-result"]));

    const wrapper = await mountPage();
    await wrapper.find("select").setValue("name");
    await flushPromises();
    releaseSlow(listing(["slow-result"]));
    await flushPromises();

    const cache = useExploreCache<{ items: { name: string }[] }>();
    // The initial mount's coordinates: default registry, page 0, "fetched".
    expect(cache.get("", 0, "fetched", "")?.items[0].name).toBe("slow-result");
  });

  it("surfaces a listing error rather than an empty table", async () => {
    explorePackagesMock.mockResolvedValue({ error: { message: "upstream exploded" } });
    const wrapper = await mountPage();
    expect(wrapper.text()).toMatch(/failed to load/i);
  });
});

/**
 * Resolution as state — DESIGN.md's organising idea, and the one this page is
 * the proving surface for. The grading is the server's; what is checked here is
 * that the page *renders what it was told* rather than re-deciding it, which is
 * how the old table ended up showing two states for a world that has six.
 */
describe("PackageCatalog resolution column", () => {
  beforeEach(() => {
    vi.useRealTimers();
    explorePackagesMock.mockReset().mockResolvedValue(listing(["lodash"]));
    exploreUpstreamSearchMock.mockReset().mockResolvedValue({ data: { items: [] } });
    listRegistriesMock.mockReset().mockResolvedValue({ data: [{ name: "npm", type: "npm" }] });
    statsMock.mockReset().mockResolvedValue({ data: { registries: [] } });
    scopeExploreCacheTo(`test-${Math.random()}`);
  });

  /**
   * The fine 3×3 is "held and verified"; the coarse 2×2 is everything else.
   * Asserted on the cell count rather than on a class, because the count *is*
   * the distinction — a 2×2 rendered with the fine class would still read as
   * coarse to anyone looking at it.
   */
  it("draws a fine matrix for what is held and a coarse one for what is not", async () => {
    explorePackagesMock.mockResolvedValue(graded("cached"));
    let matrix = (await mountPage()).find("[data-testid='resolution-matrix']");
    expect(matrix.findAll("i")).toHaveLength(9);

    scopeExploreCacheTo(`test-${Math.random()}`);
    explorePackagesMock.mockResolvedValue(graded("pending"));
    matrix = (await mountPage()).find("[data-testid='resolution-matrix']");
    expect(matrix.findAll("i")).toHaveLength(4);
  });

  it("names each of the six states the server can return", async () => {
    const expected: Record<string, RegExp> = {
      cached: /cached/i,
      stale: /stale/i,
      held: /held/i,
      pending: /pending/i,
      yanked: /yanked/i,
      blocked: /blocked/i,
    };
    for (const [state, pattern] of Object.entries(expected)) {
      scopeExploreCacheTo(`test-${state}-${Math.random()}`);
      explorePackagesMock.mockResolvedValue(graded(state));
      const wrapper = await mountPage();
      expect(wrapper.find("[data-state]").attributes("data-state")).toBe(state);
      expect(wrapper.text()).toMatch(pattern);
    }
  });

  /**
   * An unknown state must not render as an unlabelled mark or crash the row. A
   * server that grows a seventh state should degrade to "we do not hold this",
   * which is the safe reading.
   */
  it("falls back to pending for a state it does not recognise", async () => {
    explorePackagesMock.mockResolvedValue(graded("quantum"));
    const wrapper = await mountPage();
    expect(wrapper.find("[data-state]").attributes("data-state")).toBe("pending");
  });

  /**
   * The proof states a denial's rule in its own row, tied to the package by
   * `aria-describedby`. Without the tie it is a paragraph a screen reader meets
   * after the row and has to relate by proximity.
   */
  it("gives a refused package a note, tied to the package cell", async () => {
    explorePackagesMock.mockResolvedValue(graded("blocked"));
    const wrapper = await mountPage();

    const described = wrapper.find("[aria-describedby]");
    expect(described.exists()).toBe(true);
    const noteId = described.attributes("aria-describedby")!;
    const note = wrapper.find(`#${CSS.escape(noteId)}`);
    expect(note.exists()).toBe(true);
    expect(note.text()).toMatch(/administrator/i);
  });

  it("leaves a package that is not refused without a note", async () => {
    explorePackagesMock.mockResolvedValue(graded("cached"));
    const wrapper = await mountPage();
    expect(wrapper.find("[aria-describedby]").exists()).toBe(false);
  });

  /**
   * `cached_bytes: null` is "we never recorded a size", which is a different
   * fact from zero — rendering it as `0 B` would claim we hold an empty file.
   */
  it("renders an unknown size as a dash rather than as zero bytes", async () => {
    explorePackagesMock.mockResolvedValue(graded("cached", { cached_bytes: null }));
    const wrapper = await mountPage();
    expect(wrapper.text()).not.toMatch(/0 B/);
  });

  /** The caption is what tells two identically-shaped tables apart. */
  it("states the row count and the ordering in the caption", async () => {
    explorePackagesMock.mockResolvedValue(listing(["a", "b"], 2));
    const wrapper = await mountPage();
    expect(wrapper.find("caption").text()).toMatch(/2 packages/i);
    expect(wrapper.find("caption").text()).toMatch(/last fetch/i);
  });

  it("renames the ordering in the caption when the sort changes", async () => {
    const wrapper = await mountPage();
    await wrapper.find("select").setValue("downloads");
    await flushPromises();
    expect(wrapper.find("caption").text()).toMatch(/most downloaded/i);
  });
});

const upstreamHit = (name: string, over: Record<string, unknown> = {}) => ({
  registry: "npm",
  name,
  description: null,
  latest_version: "9.9.9",
  already_cached: false,
  // The default is *not offered*: most of this file's assertions are about
  // rows and links, and a button drawn under every upstream hit would put its
  // label into `row.text()` for every one of them. The fetch tests below opt
  // in, which is also the shape a registry with `console_fetch = false` sends.
  fetch: { offered: false, reason: null },
  ...over,
});

/**
 * The specimen (RFC 0004-bis §14.9): the page announces its subject, and the
 * caption states only facts the API actually returned. A missing fact is
 * dropped, never printed as a zero — "we never recorded any sizes" and "0 B
 * held" are different claims about the instance.
 */
describe("PackageCatalog specimen", () => {
  beforeEach(() => {
    vi.useRealTimers();
    explorePackagesMock.mockReset().mockResolvedValue(listing(["lodash"]));
    exploreUpstreamSearchMock.mockReset().mockResolvedValue({ data: { items: [] } });
    listRegistriesMock.mockReset().mockResolvedValue({ data: [{ name: "npm", type: "npm" }] });
    statsMock.mockReset().mockResolvedValue({ data: { registries: [] } });
    scopeExploreCacheTo(`test-${Math.random()}`);
  });

  const specimen = (w: { find: (s: string) => { text: () => string } }) =>
    w.find("[data-testid='specimen-name']").text();

  it("names the instance, and counts what it holds across every registry", async () => {
    listRegistriesMock.mockResolvedValue({
      data: [
        { name: "pypi", type: "pypi", mode: "proxy" },
        { name: "npm", type: "npm", mode: "hybrid" },
      ],
    });
    statsMock.mockResolvedValue({
      data: {
        registries: [
          { registry: "npm", package_count: 1200, cached_bytes: 2048 },
          { registry: "pypi", package_count: 84, cached_bytes: 1024 },
        ],
      },
    });
    const wrapper = await mountPage();

    expect(specimen(wrapper)).toBe("All registries");
    const facts = wrapper.find("section p").text();
    expect(facts).toContain("2 registries");
    expect(facts).toContain("1,284 packages");
    expect(facts).toMatch(/3(\.0)? KiB cached/i);
  });

  it("sorts the registries it lists, whatever order the API returned them in", async () => {
    listRegistriesMock.mockResolvedValue({
      data: [
        { name: "pypi", type: "pypi", mode: "proxy" },
        { name: "cargo", type: "cargo", mode: "proxy" },
        { name: "npm", type: "npm", mode: "proxy" },
      ],
    });
    const wrapper = await mountPage();
    const options = wrapper.findAll("aside button").map((b) => b.text().replace(/\d+$/, ""));
    expect(options.slice(1)).toEqual(["cargo", "npm", "pypi"]);
  });

  /**
   * The server answers `{ registries: [], upstream_unavailable: true }` when the
   * stats query fails. Discarding that flag rendered a failed query as every
   * registry showing 0 — indistinguishable from an instance that holds nothing.
   */
  it("says nothing rather than zero when the counts could not be read", async () => {
    statsMock.mockResolvedValue({ data: { registries: [], upstream_unavailable: true } });
    const wrapper = await mountPage();

    expect(wrapper.find("aside").text()).toContain("All registries");
    // The count renders as a bare number, never parenthesised, so the original
    // `not.toContain("(0)")` could not fail — it missed exactly the regression
    // its own comment names. Assert no count is rendered at all.
    expect(wrapper.find("aside").text()).not.toMatch(/\b0\b/);
    expect(wrapper.findAll("aside .tabular-nums")).toHaveLength(0);
    // No count claimed anywhere in the caption either.
    expect(wrapper.find("section p").exists()).toBe(false);
  });

  it("treats a failed stats call the same as unavailable counts", async () => {
    statsMock.mockResolvedValue({ error: { message: "stats query timed out" } });
    const wrapper = await mountPage();
    expect(wrapper.find("section p").exists()).toBe(false);
  });

  it("survives the registry list being unreachable", async () => {
    listRegistriesMock.mockRejectedValue(new Error("network down"));
    const wrapper = await mountPage();
    expect(specimen(wrapper)).toBe("All registries");
  });

  it("gives the display step to the chosen registry, with how it runs", async () => {
    listRegistriesMock.mockResolvedValue({
      data: [{ name: "npm", type: "npm", mode: "hybrid", upstream: "registry.npmjs.org" }],
    });
    statsMock.mockResolvedValue({
      data: { registries: [{ registry: "npm", package_count: 1284, cached_bytes: 4096 }] },
    });
    const wrapper = await mountPage();

    await wrapper
      .findAll("aside button")
      .find((b) => b.text().startsWith("npm"))!
      .trigger("click");
    await flushPromises();

    expect(specimen(wrapper)).toBe("npm");
    const facts = wrapper.find("section p").text();
    expect(facts).toContain("npm");
    expect(facts).toContain("hybrid");
    expect(facts).toContain("registry.npmjs.org");
    expect(facts).toContain("1,284 packages");
  });

  it("drops the upstream fact for a registry that has none", async () => {
    listRegistriesMock.mockResolvedValue({
      data: [{ name: "internal", type: "npm", mode: "local" }],
    });
    const wrapper = await mountPage();

    await wrapper
      .findAll("aside button")
      .find((b) => b.text().startsWith("internal"))!
      .trigger("click");
    await flushPromises();

    expect(specimen(wrapper)).toBe("internal");
    const facts = wrapper.find("section p").text();
    expect(facts).toContain("local");
    // The behaviour the test is named for. Asserting only that an *unrelated*
    // fact is present left `facts.push(reg.upstream ?? "…")` passing, so a
    // registry with no upstream could render an empty fact between the dots.
    expect(facts).not.toMatch(/undefined|null/);
    expect(facts.split("·").map((f) => f.trim())).not.toContain("");
  });

  /** `cached_bytes: null` is "we never recorded sizes", not "0 B". */
  it("omits the cached size when the registry never reported one", async () => {
    listRegistriesMock.mockResolvedValue({ data: [{ name: "npm", type: "npm", mode: "proxy" }] });
    statsMock.mockResolvedValue({
      data: { registries: [{ registry: "npm", package_count: 3, cached_bytes: null }] },
    });
    const wrapper = await mountPage();

    await wrapper
      .findAll("aside button")
      .find((b) => b.text().startsWith("npm"))!
      .trigger("click");
    await flushPromises();

    expect(wrapper.find("section p").text()).not.toMatch(/cached/);
  });
});

/**
 * The catalog as a working surface: filtering by registry, searching (which also
 * asks the upstreams), paging, and getting to a package's own page.
 */
describe("PackageCatalog browsing", () => {
  beforeEach(() => {
    vi.useRealTimers();
    pushMock.mockReset();
    explorePackagesMock.mockReset().mockResolvedValue(listing(["lodash"]));
    exploreUpstreamSearchMock.mockReset().mockResolvedValue({ data: { items: [] } });
    exploreFetchVersionMock.mockReset().mockResolvedValue({ data: { fetched: true } });
    authState.isAuthenticated = true;
    listRegistriesMock.mockReset().mockResolvedValue({
      data: [
        { name: "npm", type: "npm", mode: "proxy" },
        { name: "pypi", type: "pypi", mode: "proxy" },
      ],
    });
    statsMock.mockReset().mockResolvedValue({ data: { registries: [] } });
    scopeExploreCacheTo(`test-${Math.random()}`);
  });

  /** Type into the search box and let the 300 ms debounce elapse. */
  async function typeSearch(wrapper: Awaited<ReturnType<typeof mountPage>>, value: string) {
    await wrapper.find("input").setValue(value);
    await new Promise((resolve) => setTimeout(resolve, 350));
    await flushPromises();
  }

  /**
   * The debounce timer is cleared on unmount.
   *
   * `typeSearch` always waits 350 ms — past the 300 ms deadline — so no test
   * ever left a timer in flight, and routing away within the window ran
   * `fetchPackages` against a destroyed component.
   */
  it("does not fetch after the page is left mid-debounce", async () => {
    const wrapper = await mountPage();
    const before = explorePackagesMock.mock.calls.length;

    await wrapper.find("input").setValue("left");
    wrapper.unmount();
    await new Promise((resolve) => setTimeout(resolve, 350));
    await flushPromises();

    expect(explorePackagesMock).toHaveBeenCalledTimes(before);
  });

  it("scopes the query to the registry the facet selected, and back again", async () => {
    const wrapper = await mountPage();

    await wrapper
      .findAll("aside button")
      .find((b) => b.text().startsWith("pypi"))!
      .trigger("click");
    await flushPromises();
    expect(explorePackagesMock).toHaveBeenLastCalledWith({
      query: {
        page: 0,
        sort: "fetched",
        registry: "pypi",
        q: undefined,
        in: "name",
      },
    });

    // Going back to "all" reads the entry the initial load filled — the facet is
    // a cache key, so returning to a view already seen costs no request.
    const calls = explorePackagesMock.mock.calls.length;
    await wrapper.findAll("aside button")[0].trigger("click"); // "All registries"
    await flushPromises();
    expect(explorePackagesMock).toHaveBeenCalledTimes(calls);
    // Positive evidence that the facet actually reset. Asserting only that
    // nothing happened is equally true when the click does nothing: deleting
    // the all-option's `@click` left this green, because both fixtures return
    // `lodash` and the table text is identical either way.
    expect(wrapper.find("[data-testid='specimen-name']").text()).toBe("All registries");
  });

  /**
   * The registry a row belongs to is only worth a column when more than one
   * registry is in view; inside a selected registry it is noise on every row.
   */
  it("names each row's registry only while looking at all of them", async () => {
    const wrapper = await mountPage();
    expect(wrapper.find("tbody tr").text()).toContain("npm");

    await wrapper
      .findAll("aside button")
      .find((b) => b.text().startsWith("npm"))!
      .trigger("click");
    await flushPromises();
    expect(wrapper.find("tbody tr").text()).not.toContain("npm");
  });

  it("searches this instance and the upstreams, and marks where a row came from", async () => {
    exploreUpstreamSearchMock.mockResolvedValue({
      data: { items: [upstreamHit("left-pad"), upstreamHit("lodash", { already_cached: true })] },
    });
    const wrapper = await mountPage();

    await typeSearch(wrapper, "lo");

    expect(explorePackagesMock).toHaveBeenLastCalledWith({
      query: { page: 0, sort: "fetched", registry: undefined, q: "lo", in: "name" },
    });
    expect(exploreUpstreamSearchMock).toHaveBeenCalledWith({
      query: { name: "lo", limit: 10, registry: undefined },
    });

    // The upstream-only hit is listed and labelled; the one we already hold is
    // not repeated as an upstream row.
    const rows = wrapper.findAll("tbody tr");
    const upstreamRow = rows.find((r) => r.text().includes("left-pad"))!;
    expect(upstreamRow.text()).toContain("upstream");
    expect(rows.filter((r) => r.text().includes("lodash"))).toHaveLength(1);
  });

  it("does not call the upstreams for a single character", async () => {
    const wrapper = await mountPage();
    await typeSearch(wrapper, "l");
    expect(exploreUpstreamSearchMock).not.toHaveBeenCalled();
  });

  it("reuses the upstream answer instead of asking third parties twice", async () => {
    exploreUpstreamSearchMock.mockResolvedValue({ data: { items: [upstreamHit("left-pad")] } });
    const wrapper = await mountPage();

    await typeSearch(wrapper, "left");
    await typeSearch(wrapper, "");
    await typeSearch(wrapper, "left");

    expect(exploreUpstreamSearchMock).toHaveBeenCalledTimes(1);
  });

  it("survives an upstream search that fails", async () => {
    exploreUpstreamSearchMock.mockRejectedValue(new Error("upstream refused"));
    const wrapper = await mountPage();

    await typeSearch(wrapper, "left");

    expect(wrapper.text()).toContain("lodash");
  });

  it("re-reads the registry rather than the cache when refreshed", async () => {
    const wrapper = await mountPage();
    expect(explorePackagesMock).toHaveBeenCalledTimes(1);

    await wrapper
      .findAll("button")
      .find((b) => /refresh/i.test(b.text()))!
      .trigger("click");
    await flushPromises();

    expect(explorePackagesMock).toHaveBeenCalledTimes(2);
  });

  it("pages through a listing longer than one page", async () => {
    explorePackagesMock.mockResolvedValue(listing(["lodash"], 45));
    const wrapper = await mountPage();

    expect(wrapper.text()).toContain("45 cached packages total");

    await wrapper
      .findAll("nav button")
      .find((b) => /next/i.test(b.text()))!
      .trigger("click");
    await flushPromises();

    expect(explorePackagesMock).toHaveBeenLastCalledWith({
      query: {
        page: 1,
        sort: "fetched",
        registry: undefined,
        q: undefined,
        in: "name",
      },
    });
  });

  it("hides the pager when everything fits on one page", async () => {
    const wrapper = await mountPage();
    expect(wrapper.find("nav").exists()).toBe(false);
  });

  /**
   * Empty is two states, and telling them apart is the point: a user shown "no
   * packages" while a filter is applied concludes the registry is broken.
   */
  it("distinguishes an empty instance from a search that matched nothing", async () => {
    explorePackagesMock.mockResolvedValue(listing([]));
    const wrapper = await mountPage();

    expect(wrapper.text()).toContain("No packages cached yet");

    await typeSearch(wrapper, "nothing-matches");
    expect(wrapper.text()).toContain("Nothing matches that search");

    await wrapper
      .findAll("button")
      .find((b) => /clear search/i.test(b.text()))!
      .trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("No packages cached yet");
  });

  /**
   * A package this instance has not pulled opens too, and used to be the one
   * row in the catalog that led nowhere.
   *
   * This test asserted the opposite — that an upstream row calls no navigation —
   * and that assertion was the bug held in place: `explore/detail.rs` resolves
   * `upstream_detail` for anything not held here, and `PackageDetailPage`
   * renders those versions with the per-version *Fetch this version* button
   * (RFC 0007-bis §6.4). The page existed, the search surfaced the package, and
   * the click between them was refused, so the only way in was to type the URL.
   *
   * Both kinds are asserted in one test on purpose: the point is that they now
   * resolve to the *same* address from the same two fields.
   *
   * Asserted on the **link**, not on a click. The row used to carry the
   * destination in a `@click` handler with no `tabindex`, no `role` and no key
   * handler, so the assertion this replaces could pass over a target no
   * keyboard could reach — see the WCAG 2.1.1 case below.
   */
  it("links a package's page whether or not this instance holds it", async () => {
    exploreUpstreamSearchMock.mockResolvedValue({ data: { items: [upstreamHit("left-pad")] } });
    const wrapper = await mountPage();
    await typeSearch(wrapper, "l@test");

    expect(nameLink(wrapper, "left-pad")).toBe("/packages/npm/left-pad");
    expect(nameLink(wrapper, "lodash")).toBe("/packages/npm/lodash");
  });

  /**
   * WCAG 2.1.1 Keyboard (A), on the product's primary navigation path.
   *
   * `<TableRow @click>` gave a mouse the whole row and a keyboard nothing —
   * no `tabindex`, no `role`, no key handler — and the package name was plain
   * text. The one keyboard route to a detail page was the **details** link
   * inside a refusal note, which renders only when the row *has* a note. So a
   * normally cached package could not be opened from the keyboard at all.
   *
   * The row must also stay un-clickable as a whole: a second navigation on the
   * row would double every activation and re-introduce a mouse-only target.
   */
  it("puts the destination on the name, not on the row", async () => {
    const wrapper = await mountPage();

    const row = wrapper.findAll("tbody tr").find((r) => r.text().includes("lodash"))!;
    const link = row.findAll("a").find((a) => a.text().trim() === "lodash");
    expect(link, "the package name renders as a link").toBeDefined();
    expect(link!.attributes("href")).toBe("/packages/npm/lodash");

    pushMock.mockClear();
    await row.trigger("click");
    expect(pushMock, "the row itself navigates nowhere").not.toHaveBeenCalled();
    expect(row.classes()).not.toContain("cursor-pointer");
  });

  /**
   * A name with slashes in it — Go and Composer coordinates — survives the trip.
   *
   * The path is built with `encodeURIComponent`, so `github.com/ttacon/chalk`
   * arrives as one route param rather than three path segments. An upstream
   * search against a `go` registry returns exactly this shape, so it is the
   * first thing a click on those results would have hit.
   */
  it("encodes a slashed package name into a single route param", async () => {
    exploreUpstreamSearchMock.mockResolvedValue({
      data: { items: [upstreamHit("github.com/ttacon/chalk", { registry: "go" })] },
    });
    const wrapper = await mountPage();
    await typeSearch(wrapper, "chalk");

    expect(nameLink(wrapper, "github.com/ttacon/chalk")).toBe(
      "/packages/go/github.com%2Fttacon%2Fchalk",
    );
  });

  // ── Fetching from the listing (RFC 0007-bis §11 q3) ────────────────────────

  /** Search, with one upstream-only hit the server says is fetchable. */
  async function searchWithFetchableHit(over: Record<string, unknown> = {}) {
    exploreUpstreamSearchMock.mockResolvedValue({
      data: {
        items: [upstreamHit("left-pad", { fetch: { offered: true, reason: null }, ...over })],
      },
    });
    const wrapper = await mountPage();
    await typeSearch(wrapper, "left");
    return wrapper;
  }

  const fetchButton = (wrapper: VueWrapper) =>
    wrapper.findAll("tbody button").find((b) => b.text().includes("9.9.9"));

  /**
   * The button names the version, and asks for that one.
   *
   * The version is the upstream search's own answer and is already in the row's
   * version column — so no choice is being made here, which is the objection
   * §11 q3 was recommended *no* over.
   */
  it("offers the version an upstream row names, and fetches that version", async () => {
    const wrapper = await searchWithFetchableHit();

    const button = fetchButton(wrapper)!;
    expect(button.text()).toContain("9.9.9");

    await button.trigger("click");
    await flushPromises();

    expect(exploreFetchVersionMock).toHaveBeenCalledWith({
      path: { registry: "npm", name: "left-pad", version: "9.9.9" },
    });
  });

  /**
   * A successful fetch refreshes both halves of the page.
   *
   * Without it the ten-minute upstream cache serves the row straight back as a
   * discovery and the button looks as though it did nothing.
   */
  it("refreshes the listing and the upstream results after a fetch", async () => {
    const wrapper = await searchWithFetchableHit();
    const listings = explorePackagesMock.mock.calls.length;
    const upstreams = exploreUpstreamSearchMock.mock.calls.length;

    await fetchButton(wrapper)!.trigger("click");
    await flushPromises();

    expect(explorePackagesMock.mock.calls.length).toBeGreaterThan(listings);
    expect(exploreUpstreamSearchMock.mock.calls.length).toBeGreaterThan(upstreams);
  });

  /** A refusal is the rule's own words, on the row that was refused. */
  it("shows the rule's reason when the fetch is refused", async () => {
    exploreFetchVersionMock.mockResolvedValue({
      error: { code: "fetch.denied", message: "release-age gate: 3 days to go" },
    });
    const wrapper = await searchWithFetchableHit();

    await fetchButton(wrapper)!.trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("release-age gate: 3 days to go");
    // The listing was not refetched: nothing changed, and a reload would have
    // dropped the message that explains why.
    expect(fetchButton(wrapper)).toBeDefined();
  });

  /**
   * Not offered means no button and no sentence.
   *
   * The reason for a registry — the operator's switch, or a kind with no single
   * artifact per version — belongs on the package page, which states it once.
   * Repeating it under every row of a page of hits would drown the rows.
   */
  it("draws nothing when the server does not offer the fetch", async () => {
    exploreUpstreamSearchMock.mockResolvedValue({
      data: {
        items: [
          upstreamHit("left-pad", {
            fetch: { offered: false, reason: "maven has no single artifact per version" },
          }),
        ],
      },
    });
    const wrapper = await mountPage();
    await typeSearch(wrapper, "left");

    expect(fetchButton(wrapper)).toBeUndefined();
    expect(wrapper.text()).not.toContain("no single artifact per version");
  });

  /** A reader with no session is not offered a button the endpoint would `401`. */
  it("draws no button for a reader with no session", async () => {
    authState.isAuthenticated = false;
    const wrapper = await searchWithFetchableHit();
    expect(fetchButton(wrapper)).toBeUndefined();
  });

  /**
   * A row from a server that predates the field draws nothing rather than
   * throwing inside the render and taking the whole listing with it — including
   * its cached rows, which had nothing to do with the upstream half.
   */
  it("survives an upstream row with no offer at all", async () => {
    exploreUpstreamSearchMock.mockResolvedValue({
      data: { items: [{ registry: "npm", name: "left-pad", latest_version: "9.9.9" }] },
    });
    const wrapper = await mountPage();
    await typeSearch(wrapper, "left");

    expect(wrapper.text()).toContain("left-pad");
    expect(wrapper.text()).toContain("lodash");
    expect(fetchButton(wrapper)).toBeUndefined();
  });
});

/**
 * Searching what a package *says* (RFC 0007-bis §4.3).
 *
 * The ranking is Postgres's and is tested where it lives. What the page owes the
 * reader is narrower and entirely its own: a control that only appears when the
 * instance can honour it, a label on the row that needs explaining and not on
 * the ones that do not, a snippet rendered as **text**, and an empty state that
 * says what was actually searched.
 */
/**
 * The catalog's page size is the operator's, not this component's.
 *
 * It was a hard-coded 20 in both places — the request and the pager arithmetic
 * — so `[limits].packages_per_page` would have been a setting the one screen it
 * exists for ignored. The console asks for no size here (unlike the version
 * table on a package page, which asks for the rows it draws) and sizes its
 * pager from what came back.
 */
describe("PackageCatalog page size", () => {
  beforeEach(() => {
    vi.useRealTimers();
    pushMock.mockReset();
    explorePackagesMock.mockReset();
    exploreUpstreamSearchMock.mockReset().mockResolvedValue({ data: { items: [] } });
    listRegistriesMock.mockReset().mockResolvedValue({ data: [] });
    statsMock.mockReset().mockResolvedValue({ data: { registries: [] } });
    scopeExploreCacheTo(`test-${Math.random()}`);
  });

  it("asks for no page size at all", async () => {
    explorePackagesMock.mockResolvedValue(listing(["lodash"]));

    await mountPage();

    const query = explorePackagesMock.mock.calls.at(-1)?.[0]?.query;
    expect(query).toBeDefined();
    expect(query).not.toHaveProperty("per_page");
  });

  /** 60 rows at 50 a page is two pages, not three — the pager has to do its
      arithmetic with the number the server applied. */
  it("sizes the pager from the answer rather than from a constant", async () => {
    explorePackagesMock.mockResolvedValue({
      data: {
        items: Array.from({ length: 50 }, (_, i) => entry(`pkg-${i}`)),
        total: 60,
        page: 0,
        per_page: 50,
      },
    });

    const wrapper = await mountPage();

    expect(wrapper.text()).toContain("1 of 2");
  });

  /** An older server, or one whose answer omits it: the pager still works off
      the number the console started with rather than dividing by undefined. */
  it("falls back to twenty when the answer does not say", async () => {
    explorePackagesMock.mockResolvedValue({
      data: { items: [entry("lodash")], total: 60, page: 0 },
    });

    const wrapper = await mountPage();

    expect(wrapper.text()).toContain("1 of 3");
  });
});

describe("PackageCatalog prose search", () => {
  const proseListing = (over: Record<string, unknown> = {}) => ({
    data: {
      items: [
        entry("retry", { matched_in: "name", snippet: null }),
        entry("resilience-toolkit", {
          matched_in: "readme",
          snippet: "…exponential backoff for flaky upstreams…",
        }),
      ],
      total: 2,
      page: 0,
      per_page: 20,
      readme_search_enabled: true,
      searched_in: "both",
      truncated: false,
      ...over,
    },
  });

  beforeEach(() => {
    vi.useRealTimers();
    exploreUpstreamSearchMock.mockReset().mockResolvedValue({ data: { items: [] } });
    listRegistriesMock.mockReset().mockResolvedValue({ data: [{ name: "npm", type: "npm" }] });
    statsMock.mockReset().mockResolvedValue({ data: { registries: [] } });
    scopeExploreCacheTo(`test-${Math.random()}`);
  });

  const scopeSelect = (w: ReturnType<typeof mount>) =>
    w.findAll("select").find((sel) => sel.find('option[value="readme"]').exists());

  /**
   * A control whose other options do nothing is worse than no control. The
   * server says whether prose search is on; the page does not guess.
   */
  it("offers the scope control only when the instance searches prose", async () => {
    explorePackagesMock.mockReset().mockResolvedValue({
      data: { ...listing(["lodash"]).data, readme_search_enabled: false },
    });
    let wrapper = await mountPage();
    expect(scopeSelect(wrapper)).toBeUndefined();

    explorePackagesMock.mockReset().mockResolvedValue(proseListing());
    scopeExploreCacheTo(`test-${Math.random()}`);
    wrapper = await mountPage();
    expect(scopeSelect(wrapper)).toBeDefined();
  });

  it("sends the chosen scope, and re-runs at once rather than on a debounce", async () => {
    explorePackagesMock.mockReset().mockResolvedValue(proseListing());
    const wrapper = await mountPage();

    await scopeSelect(wrapper)!.setValue("readme");
    await flushPromises();

    expect(explorePackagesMock).toHaveBeenLastCalledWith({
      query: {
        page: 0,
        sort: "fetched",
        registry: undefined,
        q: undefined,
        in: "readme",
      },
    });
  });

  /**
   * Only the row that needs explaining is labelled. A row that matched on its
   * name is self-explanatory, and labelling every row would make the one that
   * matters invisible.
   */
  it("labels a prose match and leaves a name match unlabelled", async () => {
    explorePackagesMock.mockReset().mockResolvedValue(proseListing());
    const wrapper = await mountPage();

    const rows = wrapper.findAll("tbody tr");
    const named = rows.find((r) => r.text().includes("retry"))!;
    const prose = rows.find((r) => r.text().includes("resilience-toolkit"))!;

    expect(prose.text().toLowerCase()).toContain("readme");
    expect(named.text().toLowerCase()).not.toContain("readme");
  });

  /**
   * The snippet is package-authored content on a second surface. It is
   * interpolated, never `v-html` (RFC 0007-bis §7.4) — so markup in it appears
   * as characters, which is what this asserts.
   */
  it("renders a snippet as text and never as markup", async () => {
    explorePackagesMock.mockReset().mockResolvedValue(
      proseListing({
        items: [
          entry("hostile", {
            matched_in: "readme",
            snippet: "<img src=x onerror=alert(1)> and <b>bold</b>",
          }),
        ],
        total: 1,
      }),
    );
    const wrapper = await mountPage();

    expect(wrapper.text()).toContain("<img src=x onerror=alert(1)>");
    expect(wrapper.find("tbody img").exists()).toBe(false);
    expect(wrapper.find("tbody b").exists()).toBe(false);
  });

  /**
   * "Nothing matches that search" would imply the query was checked against
   * every package here. It was checked against the READMEs of the versions this
   * instance holds, which is a narrower claim and the honest one.
   */
  it("says what was searched when a prose search finds nothing", async () => {
    explorePackagesMock
      .mockReset()
      .mockResolvedValue(proseListing({ items: [], total: 0, searched_in: "readme" }));
    const wrapper = await mountPage();

    await scopeSelect(wrapper)!.setValue("readme");
    // Typed and debounced locally: `typeSearch` belongs to the browsing suite.
    await wrapper.find("input").setValue("backoff");
    await new Promise((resolve) => setTimeout(resolve, 350));
    await flushPromises();

    expect(wrapper.text()).toContain("READMEs of versions held on this instance");
  });

  /** A cap that applied, said out loud rather than read as "that is all". */
  it("says when the prose search was truncated", async () => {
    explorePackagesMock
      .mockReset()
      .mockResolvedValue(proseListing({ truncated: true, total: 200 }));
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("Narrow the query");
  });
});

/**
 * The search lives in the URL (so a return to the list is a return to the
 * search).
 *
 * Every control on this page wrote to component state and nowhere else, so the
 * whole arrangement — registry, query, scope, sort, page — existed only as long
 * as the component did. Open a package from the fifth page of a search, come
 * back, and the box was empty. The detail page's own back button pushed
 * `/packages?registry=…`, which read like a fix and was not: nothing here ever
 * looked at `route.query`.
 */
describe("PackageCatalog url state", () => {
  beforeEach(() => {
    vi.useRealTimers();
    routeState.query = {};
    replaceMock.mockReset().mockImplementation((loc: { query?: Record<string, string> }) => {
      // Stand in for the address bar actually changing, which is what the page's
      // own "has anything changed?" check reads back.
      routeState.query = loc.query ?? {};
    });
    explorePackagesMock.mockReset().mockResolvedValue(listing(["lodash"]));
    exploreUpstreamSearchMock.mockReset().mockResolvedValue({ data: { items: [] } });
    listRegistriesMock.mockReset().mockResolvedValue({ data: [{ name: "npm", type: "npm" }] });
    statsMock.mockReset().mockResolvedValue({ data: { registries: [] } });
    scopeExploreCacheTo(`test-${Math.random()}`);
  });

  afterEach(() => {
    routeState.query = {};
  });

  it("opens on the search its URL describes", async () => {
    routeState.query = { registry: "npm", q: "lodash", in: "both", sort: "name", page: "3" };

    const wrapper = await mountPage();

    // The first request is the reader's search, not the defaults — fetching
    // those first would show a list nobody asked for and then replace it.
    expect(explorePackagesMock).toHaveBeenCalledTimes(1);
    expect(explorePackagesMock).toHaveBeenLastCalledWith({
      query: { page: 2, sort: "name", registry: "npm", q: "lodash", in: "both" },
    });
    // And the controls agree with the list, rather than showing an empty box
    // above rows that were filtered.
    expect(wrapper.find("input").element.value).toBe("lodash");
  });

  it("writes a search back so returning to the list returns to it", async () => {
    const wrapper = await mountPage();

    await wrapper.find("input").setValue("left-pad");
    await new Promise((resolve) => setTimeout(resolve, 350));
    await flushPromises();

    expect(replaceMock).toHaveBeenLastCalledWith({
      path: "/packages",
      query: { q: "left-pad" },
    });
  });

  it("keeps a default off the URL", async () => {
    const wrapper = await mountPage();

    // `fetched` is the default sort, so selecting it says nothing.
    await wrapper.find("select").setValue("name");
    await flushPromises();
    expect(replaceMock).toHaveBeenLastCalledWith({ path: "/packages", query: { sort: "name" } });

    await wrapper.find("select").setValue("fetched");
    await flushPromises();
    expect(replaceMock).toHaveBeenLastCalledWith({ path: "/packages", query: {} });
  });

  /** A hand-edited URL cannot put the page somewhere its own controls could not. */
  it("falls back to the defaults for values it does not recognise", async () => {
    routeState.query = { sort: "whatever", in: "sideways", page: "-4" };

    await mountPage();

    expect(explorePackagesMock).toHaveBeenLastCalledWith({
      query: {
        page: 0,
        sort: "fetched",
        registry: undefined,
        q: undefined,
        in: "name",
      },
    });
  });

  /**
   * The page must not write to a route it no longer owns: a fetch settling after
   * the reader has opened a package would otherwise rewrite that page's address.
   */
  it("does not touch the URL once the reader has left the catalog", async () => {
    const wrapper = await mountPage();
    routeState.path = "/packages/npm/lodash";

    await wrapper.find("input").setValue("left-pad");
    await new Promise((resolve) => setTimeout(resolve, 350));
    await flushPromises();

    expect(replaceMock).not.toHaveBeenCalled();
    routeState.path = "/packages";
  });
});
