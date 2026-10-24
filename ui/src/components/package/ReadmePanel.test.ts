import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { explorePackageReadmeMock, highlightIntoMock } = vi.hoisted(() => ({
  explorePackageReadmeMock: vi.fn(),
  highlightIntoMock: vi.fn(),
}));

vi.mock("@/client/sdk.gen", () => ({
  explorePackageReadme: explorePackageReadmeMock,
}));

vi.mock("@/composables/useShiki", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/composables/useShiki")>()),
  highlightInto: highlightIntoMock,
}));

import ReadmePanel from "./ReadmePanel.vue";

function ok(overrides: Record<string, unknown> = {}) {
  return {
    data: {
      registry: "npm1",
      name: "express",
      version: "4.18.2",
      requested_version: "4.18.2",
      is_fallback: false,
      format: "markdown",
      source: "upstream-metadata",
      package_level: false,
      stored: true,
      freshness: null,
      truncated: false,
      rendered_html: "<h1>express</h1><p>Fast, unopinionated.</p>",
      source_text: null,
      extracted_at: "2024-01-01T00:00:00Z",
      ...overrides,
    },
  };
}

async function mountPanel(version: string | null = "4.18.2") {
  const wrapper = mount(ReadmePanel, {
    props: { registry: "npm1", name: "express", version },
  });
  await flushPromises();
  return wrapper;
}

describe("ReadmePanel", () => {
  beforeEach(() => {
    explorePackageReadmeMock.mockReset().mockResolvedValue(ok());
    highlightIntoMock.mockReset().mockResolvedValue(true);
  });

  /**
   * Every fenced block with a language is painted — in parallel, each into its
   * own element — and the ones with nothing to paint, or already painted, are
   * left alone.
   */
  it("highlights each fenced block with a language, and only those", async () => {
    explorePackageReadmeMock.mockResolvedValue(
      ok({
        rendered_html:
          '<pre><code class="language-js">a()</code></pre>' +
          '<pre><code class="language-sh">ls</code></pre>' +
          "<pre><code>no language</code></pre>" +
          '<pre><code class="language-js">   </code></pre>' +
          '<pre><code class="language-js" data-highlighted="true">done()</code></pre>',
      }),
    );
    const wrapper = await mountPanel();
    await flushPromises();

    expect(highlightIntoMock).toHaveBeenCalledTimes(2);
    expect(highlightIntoMock.mock.calls.map((c) => c[2])).toEqual(["js", "sh"]);
    const painted = wrapper.findAll('code[data-highlighted="true"]');
    expect(painted).toHaveLength(3);
  });

  it("renders the HTML the server sanitised", async () => {
    const wrapper = await mountPanel();
    expect(wrapper.find(".readme-body h1").text()).toBe("express");
    expect(wrapper.text()).toContain("Fast, unopinionated.");
  });

  /**
   * The panel follows the page's selected version. A panel that kept showing
   * 1.x's prose while the reader was looking at 2.x is the failure the whole
   * per-version key exists to prevent.
   */
  it("refetches when the selected version changes", async () => {
    const wrapper = await mountPanel("4.18.2");
    expect(explorePackageReadmeMock).toHaveBeenCalledTimes(1);

    explorePackageReadmeMock.mockResolvedValue(
      ok({
        version: "5.0.0",
        requested_version: "5.0.0",
        rendered_html: "<h1>express 5</h1>",
      }),
    );
    await wrapper.setProps({ version: "5.0.0" });
    await flushPromises();

    expect(explorePackageReadmeMock).toHaveBeenCalledTimes(2);
    expect(wrapper.find(".readme-body h1").text()).toBe("express 5");
  });

  /**
   * Prose that belongs to different code is the one thing this panel must never
   * present silently, so the label names both versions in words.
   */
  it("labels a fallback with the version it came from", async () => {
    explorePackageReadmeMock.mockResolvedValue(
      ok({
        version: "1.4.2",
        requested_version: "2.0.0-rc1",
        is_fallback: true,
      }),
    );
    const wrapper = await mountPanel("2.0.0-rc1");
    expect(wrapper.text()).toContain("1.4.2");
    expect(wrapper.text()).toContain("2.0.0-rc1");
  });

  it("says so when the text is the package's rather than this version's", async () => {
    explorePackageReadmeMock.mockResolvedValue(ok({ package_level: true }));
    const wrapper = await mountPanel();
    expect(wrapper.text().toLowerCase()).toContain("package");
  });

  it("says so when the stored source was truncated", async () => {
    explorePackageReadmeMock.mockResolvedValue(ok({ truncated: true }));
    const wrapper = await mountPanel();
    expect(wrapper.text().toLowerCase()).toContain("beginning");
  });

  /**
   * A derived answer is read from the cached upstream document rather than from
   * a durable record, and the reader is told — it is as current as the metadata
   * cache and no more.
   */
  it("marks a derived README as not held here", async () => {
    explorePackageReadmeMock.mockResolvedValue(ok({ stored: false, freshness: "cached" }));
    const wrapper = await mountPanel();
    expect(wrapper.text().toLowerCase()).toContain("not stored");
  });

  /**
   * The two absences read differently: one is a statement about the ecosystem,
   * the other a limit that resolves itself on the first download.
   */
  it("renders each absence as its own statement rather than as an error", async () => {
    explorePackageReadmeMock.mockResolvedValue({
      error: {
        code: "readme.unsupported-type",
        message: "maven packages carry no README",
      },
    });
    let wrapper = await mountPanel();
    expect(wrapper.text().toLowerCase()).toContain("no readme");

    explorePackageReadmeMock.mockResolvedValue({
      error: { code: "readme.none-stored", message: "no README stored" },
    });
    wrapper = await mountPanel();
    expect(wrapper.text().toLowerCase()).toContain("no readme stored");

    explorePackageReadmeMock.mockResolvedValue({
      error: { code: "readme.blocked", message: "blocked: known-malicious" },
    });
    wrapper = await mountPanel();
    expect(wrapper.text().toLowerCase()).toContain("blocked");
  });

  /**
   * With no version selected, the panel asks for the newest that has one — and
   * it asks for `format=both`, without which `source_text` comes back `null` and
   * the *View source* control has nothing to switch to.
   */
  it("asks for no particular version when none is selected, and for both formats", async () => {
    await mountPanel(null);
    expect(explorePackageReadmeMock).toHaveBeenCalledWith(
      expect.objectContaining({ query: { format: "both" } }),
    );
  });
});

/**
 * The security boundary, asserted as a repository-wide fact rather than left to
 * review (RFC 0007 §6.5).
 *
 * `v-html` renders unescaped markup. For a README that is *correct* — the HTML
 * is produced and sanitised server-side, by an allow-list with a fuzz target
 * over it — and it is correct precisely because it happens in one place that
 * says so. A third component growing a `v-html` is how that stops being true,
 * and it would not show up in any other test.
 */
describe("the v-html boundary", () => {
  /**
   * `CodeBlock.vue` is the other one, and it is not a hole: its HTML is
   * Shiki's rendering of *this repository's own* snippet strings, not anything
   * a package author can write.
   */
  const ALLOWED = ["ReadmePanel.vue", "CodeBlock.vue"];

  it("is only crossed by the components that document why", () => {
    /* `import.meta.glob` rather than `node:fs`, for the reason
       `locales/catalogues.test.ts` gives: this file type-checks under the app's
       tsconfig, which has no node types, and Vite resolves the pattern at
       transform time so the walk cannot drift from what the bundle contains.
       Walking with `readdirSync` left this test out of `vue-tsc` entirely. */
    const modules = import.meta.glob("../../**/*.vue", {
      query: "?raw",
      import: "default",
      eager: true,
    }) as Record<string, string>;

    const offenders = Object.entries(modules)
      .filter(([, source]) => /\sv-html\s*=/.test(source))
      .map(([path]) => path.split("/").pop()!)
      .filter((base) => !ALLOWED.includes(base));

    expect(offenders).toEqual([]);
  });
});
