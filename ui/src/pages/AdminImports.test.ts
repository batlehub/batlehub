import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { authFetchMock } = vi.hoisted(() => ({ authFetchMock: vi.fn() }));
vi.mock("@/composables/useAuthFetch", () => ({
  useAuthFetch: () => ({ authFetch: authFetchMock }),
}));

import AdminImports from "./AdminImports.vue";

const run = (over: Record<string, unknown> = {}) => ({
  started_at: "2026-09-08T10:00:00Z",
  finished_at: "2026-09-08T10:00:05Z",
  imported: 2,
  skipped: 7,
  errors: 0,
  triggered_by: "admin",
  failures: null,
  ...over,
});

const configured = (repo: string, last_run: unknown = null) => ({
  repo,
  assets: ["*.vsix"],
  last_run,
});

/**
 * Route the mock by URL. `mockReset` rather than re-implement: the call log is
 * what "the right repo was posted" is asserted against, and it otherwise
 * carries over from the previous test.
 */
function routes(listing: unknown, importResult: Record<string, unknown> = {}) {
  authFetchMock.mockReset();
  authFetchMock.mockImplementation((url: string) =>
    Promise.resolve({
      ok: true,
      status: 200,
      json: async () => (url.endsWith("/admin/imports") ? listing : importResult),
    }),
  );
}

async function mountPage() {
  const wrapper = mount(AdminImports, { global: { stubs: { SectionTabs: true } } });
  await flushPromises();
  return wrapper;
}

const rowFor = (w: Awaited<ReturnType<typeof mountPage>>, repo: string) =>
  w.findAll("tbody tr").find((r) => r.text().includes(repo))!;

beforeEach(() => authFetchMock.mockReset());

describe("AdminImports", () => {
  /**
   * The distinction the page exists for. RFC 0021 §6.5 asks for "its last run",
   * and the scheduler's own comment says why: "the import ran and found nothing
   * new" and "the import has not run" are two states, and a page that showed
   * both as an empty cell would be worse than no page.
   */
  it("tells a run that found nothing apart from never having run", async () => {
    routes({
      registries: [
        {
          registry: "vsx",
          imports: [
            configured("acme/ext", run({ imported: 0, skipped: 0 })),
            configured("acme/other"),
          ],
        },
      ],
    });
    const w = await mountPage();

    // Ran, found nothing: the zero is shown, not hidden.
    expect(rowFor(w, "acme/ext").text()).toContain("0 imported");
    expect(rowFor(w, "acme/ext").text()).not.toContain("Has not run");
    // Never ran: said in words.
    expect(rowFor(w, "acme/other").text()).toContain("Has not run");
  });

  /** A scheduled run has no operator behind it, and must not name one. */
  it("names who asked, or says the schedule did", async () => {
    routes({
      registries: [
        {
          registry: "vsx",
          imports: [
            configured("acme/manual", run({ triggered_by: "alice" })),
            configured("acme/timer", run({ triggered_by: null })),
          ],
        },
      ],
    });
    const w = await mountPage();

    expect(rowFor(w, "acme/manual").text()).toContain("alice");
    expect(rowFor(w, "acme/timer").text()).toContain("Scheduled");
    expect(rowFor(w, "acme/timer").text()).not.toContain("Asked by");
  });

  /**
   * One registry can have several imports configured into it. Pressing one
   * button must run that one, or an operator retrying a failing repo silently
   * re-runs the healthy one too.
   */
  it("posts the repo whose button was pressed", async () => {
    routes(
      {
        registries: [
          { registry: "vsx", imports: [configured("acme/first"), configured("acme/second")] },
        ],
      },
      { imported: 1, skipped: 0, errors: 0, failures: [] },
    );
    const w = await mountPage();

    await rowFor(w, "acme/second").find("button").trigger("click");
    await flushPromises();

    const post = authFetchMock.mock.calls.find((c) => c[1]?.method === "POST");
    expect(post, "no import was posted").toBeTruthy();
    expect(post![0]).toContain("/registries/vsx/import");
    expect(JSON.parse(post![1].body)).toEqual({ repo: "acme/second" });
  });

  /** `--tag` is the only way to reach a pre-release; the field must travel. */
  it("sends the tag when one is typed, and omits it when not", async () => {
    routes(
      { registries: [{ registry: "vsx", imports: [configured("acme/ext")] }] },
      { imported: 1, skipped: 0, errors: 0, failures: [] },
    );
    const w = await mountPage();

    await rowFor(w, "acme/ext").find("input").setValue("v1.0.0-rc.1");
    await rowFor(w, "acme/ext").find("button").trigger("click");
    await flushPromises();

    const post = authFetchMock.mock.calls.find((c) => c[1]?.method === "POST");
    expect(JSON.parse(post![1].body)).toEqual({ repo: "acme/ext", tag: "v1.0.0-rc.1" });
  });

  /**
   * Failures are named, not counted: "3 errors" is not something an operator
   * can act on and a tag with a reason is. The same rule the warming report
   * next door follows.
   */
  it("names the assets that did not import", async () => {
    routes(
      { registries: [{ registry: "vsx", imports: [configured("acme/ext")] }] },
      {
        imported: 0,
        skipped: 0,
        errors: 1,
        failures: [
          { tag: "v2.1.0", asset: "ext-2.1.0.vsix", error: "manifest names no publisher" },
        ],
      },
    );
    const w = await mountPage();

    await rowFor(w, "acme/ext").find("button").trigger("click");
    await flushPromises();

    const text = rowFor(w, "acme/ext").text();
    expect(text).toContain("ext-2.1.0.vsix");
    expect(text).toContain("manifest names no publisher");
  });

  /** An error count with nothing to name says so, rather than showing nothing. */
  it("says when errors came back with no asset to name", async () => {
    routes(
      { registries: [{ registry: "vsx", imports: [configured("acme/ext")] }] },
      { imported: 0, skipped: 0, errors: 2, failures: [] },
    );
    const w = await mountPage();

    await rowFor(w, "acme/ext").find("button").trigger("click");
    await flushPromises();

    expect(rowFor(w, "acme/ext").text()).toContain("without naming an asset");
  });

  /** The empty state names the config block rather than rendering an empty table. */
  it("names the config block when nothing is configured", async () => {
    routes({ registries: [] });
    const w = await mountPage();

    expect(w.text()).toContain("[[release_imports]]");
    expect(w.findAll("tbody tr")).toHaveLength(0);
  });
});
