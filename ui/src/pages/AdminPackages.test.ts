import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const mocks = vi.hoisted(() => ({
  listPackages: vi.fn(),
  listRegistries: vi.fn(),
  blockPackage: vi.fn(),
  unblockPackage: vi.fn(),
  bulkBlockPackages: vi.fn(),
  bulkUnblockPackages: vi.fn(),
}));
vi.mock("@/client/sdk.gen", () => mocks);

const { authFetchMock } = vi.hoisted(() => ({ authFetchMock: vi.fn() }));
vi.mock("@/composables/useAuthFetch", () => ({
  useAuthFetch: () => ({ authFetch: authFetchMock }),
}));

vi.mock("vue-router", () => ({
  useRouter: () => ({ push: vi.fn() }),
  /*
   * `String(to)` renders every object `to` as "[object Object]", so any
   * assertion about a link carrying a query silently checked nothing. Resolve
   * the object form into a real URL instead.
   */
  RouterLink: {
    template: "<a :href='href'><slot/></a>",
    props: ["to"],
    computed: {
      href(this: { to: string | { path: string; query?: Record<string, unknown> } }) {
        if (typeof this.to === "string") return this.to;
        const entries = Object.entries(this.to.query ?? {})
          .filter(([, v]) => v !== undefined && v !== null)
          .map(([k, v]) => [k, String(v)] as [string, string]);
        const query = new URLSearchParams(entries).toString();
        return query ? `${this.to.path}?${query}` : this.to.path;
      },
    },
  },
}));

import AdminPackages from "./AdminPackages.vue";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";

const pkg = (over: Record<string, unknown> = {}) => ({
  id: "1",
  package_id: { registry: "npm", name: "left-pad", version: "1.0.0", artifact: null },
  status: { status: "available" },
  last_accessed: "2026-08-12T10:00:00Z",
  last_accessed_by: "oidc:alice",
  access_count: 7,
  ...over,
});

const listing = (items: unknown[]) => ({
  data: { items, total: items.length, page: 0, per_page: 1000 },
});

async function mountPage() {
  const wrapper = mount(AdminPackages, {
    attachTo: document.body,
    global: { stubs: { SectionTabs: true } },
  });
  await flushPromises();
  return wrapper;
}

type Page = Awaited<ReturnType<typeof mountPage>>;

const blocked = (over: Record<string, unknown> = {}) =>
  pkg({
    status: {
      status: "blocked",
      reason: "CVE-2026-0001",
      blocked_by: "oidc:alice",
      blocked_at: "2026-08-12T10:00:00Z",
    },
    ...over,
  });

const rowButton = (w: Page, label: RegExp, row = 0) => {
  const rows = w.findAll("tbody tr");
  return rows[row].findAll("button").find((b) => label.test(b.text()))!;
};

/*
 * Matched against the *trimmed whole* label, not a substring: `/^Block selected$/i`
 * also tests true against "Unblock selected", and resolved to the right button
 * only because the bulk bar happens to render Block before Unblock. Reordering
 * the bar would have silently pointed four block tests at the unblock dialog —
 * including the one asserting a bulk block is refused without a reason.
 */
const barButton = (w: Page, label: RegExp) =>
  w.findAll("button").find((b) => label.test(b.text().trim()))!;

/** The dialog is teleported to `document.body`, so it is reached as a component. */
const dialog = (w: Page) => w.findComponent(DestructiveConfirm);

/**
 * Click the dialog's real confirm button.
 *
 * Emitting `confirm` on the component calls the parent handler directly, so
 * `canConfirm` — which gates `:disabled` on this very button — never runs.
 * Every type-to-confirm assertion here then checked that a *prop was passed*,
 * not that it gated anything: deleting `nameMatches` from `canConfirm`, which
 * removes the typed-name requirement for every irreversible action in the app,
 * left them all green.
 */
function confirmButton(action: string): HTMLButtonElement {
  const buttons = [...document.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')];
  // By label, not by position: the dialog renders Cancel, the action, then a
  // trailing "Close", so `.at(-1)` silently clicks a button that does nothing.
  const button = buttons.find((b) => b.textContent?.trim().replace(/…$/, "") === action);
  expect(button, `no "${action}" button in the open dialog`).toBeTruthy();
  return button!;
}

/** Satisfy the type-to-confirm gate the way an operator does. */
async function typeConfirmName(name: string) {
  const input = document.querySelector<HTMLInputElement>("#destructive-confirm-name")!;
  input.value = name;
  input.dispatchEvent(new Event("input", { bubbles: true }));
  await flushPromises();
}

async function confirmDialog(w: Page) {
  const d = dialog(w);
  expect(d.props("open")).toBe(true);
  const action = d.props("action") as string;
  let button = confirmButton(action);

  const confirmName = d.props("confirmName") as string;
  if (!d.props("reversible") && confirmName) {
    // The gate itself, asserted rather than assumed: an irreversible action has
    // to refuse until the operator has typed the object's name.
    expect(button.disabled, "an irreversible action was confirmable without typing its name").toBe(
      true,
    );
    await typeConfirmName(confirmName);
    button = confirmButton(action);
  }

  expect(button.disabled, "the dialog's own gate refused the confirmation").toBe(false);
  button.click();
  await flushPromises();
}

/** The reason the dialog collects lives in its slot, inside the teleport. */
async function typeReason(reason: string) {
  const input = document.querySelector<HTMLInputElement>("#block-reason")!;
  input.value = reason;
  input.dispatchEvent(new Event("input"));
  await flushPromises();
}

/** Tick every row's checkbox through the header control. */
async function selectAll(w: Page) {
  await w.find('thead input[type="checkbox"]').setValue(true);
  await flushPromises();
}

/** The page's question: "what is cached here, and what is blocked". */
describe("AdminPackages", () => {
  beforeEach(() => {
    mocks.listPackages.mockReset().mockResolvedValue(listing([pkg()]));
    mocks.listRegistries.mockReset().mockResolvedValue({ data: [{ name: "npm", type: "npm" }] });
    mocks.blockPackage.mockReset().mockResolvedValue({ data: {} });
    mocks.unblockPackage.mockReset().mockResolvedValue({ data: {} });
    mocks.bulkBlockPackages
      .mockReset()
      .mockResolvedValue({ data: { succeeded_count: 1, failed_count: 0 } });
    mocks.bulkUnblockPackages
      .mockReset()
      .mockResolvedValue({ data: { succeeded_count: 1, failed_count: 0 } });
    authFetchMock.mockReset();
  });

  afterEach(() => {
    document.body.innerHTML = "";
  });

  /**
   * §6.1: six columns, not ten.
   *
   * Measured at 1440 the table wanted ~1650px intrinsic in a 1134px container,
   * so the row verbs sat off-screen at the console's own standard width.
   */
  it("fits its columns, with the links on the cells they are about", async () => {
    const wrapper = await mountPage();

    const headers = wrapper.findAll("thead th").map((h) => h.text());
    // The checkbox, six named columns, and Actions.
    expect(headers).toHaveLength(8);
    // Exactly one blank head — the checkbox. The unlabelled *nav* column that
    // held two link buttons is gone; its links moved onto the name and version
    // cells, where a reader is already pointing.
    expect(headers.filter((h) => h === "")).toHaveLength(1);

    const row = wrapper.find("tbody tr");
    const links = row.findAll("a").map((a) => a.attributes("href"));
    expect(links.some((h) => h?.includes("/packages/npm/left-pad"))).toBe(true);
  });

  /**
   * §4.3: select-all → bulk block states its count before acting.
   *
   * A confirmation that does not say how many things it is about is a
   * confirmation nobody can actually give.
   */
  it("states the count before a bulk block runs", async () => {
    mocks.listPackages.mockResolvedValue(
      listing([
        pkg({ id: "1" }),
        pkg({
          id: "2",
          package_id: { registry: "npm", name: "lodash", version: "2.0.0", artifact: null },
        }),
      ]),
    );
    const wrapper = await mountPage();

    await wrapper.find('thead input[type="checkbox"]').setValue(true);
    await flushPromises();

    const blockBtn = wrapper.findAll("button").find((b) => /^Block selected$/i.test(b.text()))!;
    await blockBtn.trigger("click");
    await flushPromises();

    // The count is in the dialog, and the request has not been sent yet.
    expect(document.body.textContent).toContain("2");
    expect(mocks.bulkBlockPackages).not.toHaveBeenCalled();
  });

  it("surfaces a load error rather than an empty catalogue", async () => {
    mocks.listPackages.mockResolvedValue({ error: { message: "db unreachable" } });
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("db unreachable");
  });

  it("says the catalogue is empty rather than looking broken", async () => {
    mocks.listPackages.mockResolvedValue(listing([]));
    const wrapper = await mountPage();
    expect(wrapper.text()).toMatch(/no packages/i);
  });

  // ── Filtering ───────────────────────────────────────────────────────────────

  /**
   * `GET /api/v1/admin/packages` declares `registry` and `blocked_only`; the
   * page used to send only `per_page: 1000`, so "what is blocked right now" —
   * half of this page's question — was answerable by the API and unreachable
   * here, and the header count silently capped at a thousand.
   */
  it("asks the server for the filtered set instead of capping a page", async () => {
    const wrapper = await mountPage();

    await wrapper.find("select").setValue("npm");
    await flushPromises();
    await wrapper.find('input[type="checkbox"][class*="h-3.5"]').setValue(true);
    await flushPromises();

    expect(mocks.listPackages).toHaveBeenLastCalledWith({
      query: { per_page: 1000, registry: "npm", blocked_only: true },
    });
  });

  it("offers the configured registries as the filter's options", async () => {
    mocks.listRegistries.mockResolvedValue({
      data: [
        { name: "npm", type: "npm" },
        { name: "pypi", type: "pypi" },
      ],
    });
    const wrapper = await mountPage();
    expect(wrapper.findAll("select option").map((o) => o.text())).toEqual([
      "All registries",
      "npm",
      "pypi",
    ]);
  });

  it("narrows the loaded rows by name, registry or version", async () => {
    mocks.listPackages.mockResolvedValue(
      listing([
        pkg({ id: "1" }),
        pkg({
          id: "2",
          package_id: { registry: "pypi", name: "requests", version: "2.31.0", artifact: null },
        }),
      ]),
    );
    const wrapper = await mountPage();
    const search = wrapper.find('input[type="text"], input:not([type])');

    await search.setValue("requests");
    expect(wrapper.findAll("tbody tr")).toHaveLength(1);

    await search.setValue("2.31");
    expect(wrapper.findAll("tbody tr")).toHaveLength(1);

    await search.setValue("PYPI");
    expect(wrapper.findAll("tbody tr")).toHaveLength(1);
  });

  it("tells a filter that matched nothing apart from an empty catalogue", async () => {
    const wrapper = await mountPage();
    await wrapper.find('input[type="text"], input:not([type])').setValue("nothing-matches-this");

    expect(wrapper.text()).toContain("No packages match your filter.");
    expect(wrapper.text()).toContain("Try clearing the filter.");
  });

  // ── One row at a time ───────────────────────────────────────────────────────

  it("shows why a package is blocked, and offers the opposite verb", async () => {
    mocks.listPackages.mockResolvedValue(listing([blocked()]));
    const wrapper = await mountPage();
    const row = wrapper.find("tbody tr");

    expect(row.text()).toContain("Blocked");
    expect(row.text()).toContain("CVE-2026-0001");
    /* Not a row tint. `bg-destructive/5` measured 1.03:1 in dark and 1.09:1 in
       light — DESIGN.md's Undependable Fill Rule in its plainest form, a fill
       standing in for a state channel while being invisible. The word and the
       badge above are the channels; `system-rules.test.ts` keeps the tint from
       coming back. */
    expect(row.attributes("class") ?? "").not.toContain("bg-destructive/");
    expect(row.findAll("button").map((b) => b.text())).toEqual(["Unblock", "Delete"]);
  });

  it("unblocks a row and reloads the listing", async () => {
    mocks.listPackages.mockResolvedValue(listing([blocked()]));
    mocks.unblockPackage.mockResolvedValue({ data: {} });
    const wrapper = await mountPage();

    await rowButton(wrapper, /unblock/i).trigger("click");
    await flushPromises();

    expect(mocks.unblockPackage).toHaveBeenCalledWith({
      body: { registry: "npm", name: "left-pad", version: "1.0.0", artifact: null },
    });
    expect(mocks.listPackages).toHaveBeenCalledTimes(2);
  });

  it("reports a failed unblock rather than silently leaving the row blocked", async () => {
    mocks.listPackages.mockResolvedValue(listing([blocked()]));
    mocks.unblockPackage.mockRejectedValue(new Error("upstream refused"));
    const wrapper = await mountPage();

    await rowButton(wrapper, /unblock/i).trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("upstream refused");
  });

  /**
   * A block a consumer will read back on a 403 needs a reason, so the dialog
   * collects one — and refuses to act without it.
   */
  it("blocks one package with the reason typed into the dialog", async () => {
    mocks.blockPackage.mockResolvedValue({ data: {} });
    const wrapper = await mountPage();

    await rowButton(wrapper, /^Block$/).trigger("click");
    await flushPromises();

    expect(dialog(wrapper).props("scope")).toContain("left-pad@1.0.0 in npm");
    expect(dialog(wrapper).props("reversible")).toBe(true);

    await typeReason("CVE-2026-0001");
    await confirmDialog(wrapper);

    expect(mocks.blockPackage).toHaveBeenCalledWith({
      body: {
        registry: "npm",
        name: "left-pad",
        version: "1.0.0",
        artifact: null,
        reason: "CVE-2026-0001",
      },
    });
  });

  it("does not block without a reason", async () => {
    const wrapper = await mountPage();

    await rowButton(wrapper, /^Block$/).trigger("click");
    await flushPromises();
    await confirmDialog(wrapper);

    expect(mocks.blockPackage).not.toHaveBeenCalled();
    // Not calling the API is only half of it. `runPending` clears `pending` —
    // closing the dialog — before dispatching, so a bare `return` left the
    // operator looking at a dismissed dialog and an unblocked package with
    // nothing said. Asserting only the absent call enshrined that silence.
    expect(wrapper.text()).toContain("A reason is required to block");
  });

  /**
   * Deleting a package record purges its cached artifact and cannot be undone,
   * so the friction is proportional: the operator types the package's name.
   */
  it("makes a single delete type-to-confirm, then deletes it", async () => {
    authFetchMock.mockResolvedValue({ ok: true, status: 200, json: async () => ({}) });
    const wrapper = await mountPage();

    await rowButton(wrapper, /^Delete$/).trigger("click");
    await flushPromises();

    expect(dialog(wrapper).props("reversible")).toBe(false);
    expect(dialog(wrapper).props("confirmName")).toBe("left-pad");

    await confirmDialog(wrapper);

    const [url, init] = authFetchMock.mock.calls[0];
    expect(url).toContain("/api/v1/admin/packages/delete");
    expect(JSON.parse(init.body)).toEqual({
      registry: "npm",
      name: "left-pad",
      version: "1.0.0",
      artifact: null,
    });
  });

  it("surfaces the server's reason for refusing a delete", async () => {
    authFetchMock.mockResolvedValue({
      ok: false,
      status: 409,
      json: async () => ({ error: "package is referenced by a lockfile" }),
    });
    const wrapper = await mountPage();

    await rowButton(wrapper, /^Delete$/).trigger("click");
    await confirmDialog(wrapper);

    expect(wrapper.text()).toContain("package is referenced by a lockfile");
  });

  it("falls back to the status code when a failed delete says nothing", async () => {
    authFetchMock.mockResolvedValue({ ok: false, status: 500, json: async () => ({}) });
    const wrapper = await mountPage();

    await rowButton(wrapper, /^Delete$/).trigger("click");
    await confirmDialog(wrapper);

    expect(wrapper.text()).toContain("HTTP 500");
  });

  // ── Selection and bulk verbs ────────────────────────────────────────────────

  it("selects and deselects every filtered row from the header checkbox", async () => {
    mocks.listPackages.mockResolvedValue(
      listing([
        pkg({ id: "1" }),
        pkg({
          id: "2",
          package_id: { registry: "npm", name: "lodash", version: "2.0.0", artifact: null },
        }),
      ]),
    );
    const wrapper = await mountPage();

    await selectAll(wrapper);
    expect(wrapper.text()).toContain("2 selected");

    await wrapper.find('thead input[type="checkbox"]').setValue(false);
    await flushPromises();
    expect(wrapper.text()).not.toContain("selected");
  });

  it("selects one row at a time, and clears the selection on demand", async () => {
    const wrapper = await mountPage();

    await wrapper.find('tbody input[type="checkbox"]').setValue(true);
    await flushPromises();
    expect(wrapper.text()).toContain("1 selected");

    // Ticking the same box again takes it back out of the selection.
    await wrapper.find('tbody input[type="checkbox"]').setValue(false);
    await flushPromises();
    expect(wrapper.text()).not.toContain("1 selected");

    await wrapper.find('tbody input[type="checkbox"]').setValue(true);
    await flushPromises();
    await barButton(wrapper, /^Clear$/).trigger("click");
    expect(wrapper.text()).not.toContain("1 selected");
  });

  it("blocks the selection with one reason, and reports the outcome", async () => {
    mocks.bulkBlockPackages.mockResolvedValue({ data: { succeeded_count: 1, failed_count: 0 } });
    const wrapper = await mountPage();

    await selectAll(wrapper);
    await barButton(wrapper, /^Block selected$/i).trigger("click");
    await flushPromises();
    await typeReason("policy violation");
    await confirmDialog(wrapper);

    expect(mocks.bulkBlockPackages).toHaveBeenCalledWith({
      body: {
        items: [
          {
            registry: "npm",
            name: "left-pad",
            version: "1.0.0",
            artifact: null,
            reason: "policy violation",
          },
        ],
      },
    });
    expect(wrapper.text()).toContain("Blocked 1 package.");
  });

  /**
   * A partial failure is the case the hand-built `, N failed` suffix existed
   * for: the operator must be told the run was not clean.
   */
  it("says how many of the selection failed", async () => {
    mocks.bulkBlockPackages.mockResolvedValue({ data: { succeeded_count: 3, failed_count: 2 } });
    const wrapper = await mountPage();

    await selectAll(wrapper);
    await barButton(wrapper, /^Block selected$/i).trigger("click");
    await flushPromises();
    await typeReason("policy violation");
    await confirmDialog(wrapper);

    expect(wrapper.text()).toContain("Blocked 3 packages.");
    expect(wrapper.text()).toContain("2 failed.");
  });

  /**
   * Without this the request failed, the selection cleared, the table reloaded,
   * and the page said nothing at all — a failed bulk action looked exactly like
   * a successful one.
   */
  it("reports a bulk block that the server rejected outright", async () => {
    mocks.bulkBlockPackages.mockResolvedValue({ error: { message: "read-only mode" } });
    const wrapper = await mountPage();

    await selectAll(wrapper);
    await barButton(wrapper, /^Block selected$/i).trigger("click");
    await flushPromises();
    await typeReason("policy violation");
    await confirmDialog(wrapper);

    expect(wrapper.text()).toContain("read-only mode");
    expect(wrapper.text()).not.toContain("Blocked 1 package");
  });

  it("does not send a bulk block without a reason", async () => {
    const wrapper = await mountPage();

    await selectAll(wrapper);
    await barButton(wrapper, /^Block selected$/i).trigger("click");
    await flushPromises();
    await confirmDialog(wrapper);

    expect(mocks.bulkBlockPackages).not.toHaveBeenCalled();
    expect(wrapper.text()).toContain("A reason is required to block");
  });

  it("unblocks the selection without asking for a reason", async () => {
    mocks.listPackages.mockResolvedValue(listing([blocked()]));
    const wrapper = await mountPage();

    await selectAll(wrapper);
    await barButton(wrapper, /^Unblock selected$/i).trigger("click");
    await flushPromises();

    expect(document.querySelector("#block-reason")).toBeNull();
    await confirmDialog(wrapper);

    expect(mocks.bulkUnblockPackages).toHaveBeenCalledWith({
      body: { items: [{ registry: "npm", name: "left-pad", version: "1.0.0", artifact: null }] },
    });
    expect(wrapper.text()).toContain("Unblocked 1 package.");
  });

  it("reports a bulk unblock the server rejected", async () => {
    mocks.bulkUnblockPackages.mockResolvedValue({ error: { message: "not permitted" } });
    const wrapper = await mountPage();

    await selectAll(wrapper);
    await barButton(wrapper, /^Unblock selected$/i).trigger("click");
    await confirmDialog(wrapper);

    expect(wrapper.text()).toContain("not permitted");
  });

  it("purges the selection's records and cached artifacts", async () => {
    authFetchMock.mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ succeeded_count: 1, failed_count: 0 }),
    });
    const wrapper = await mountPage();

    await selectAll(wrapper);
    await barButton(wrapper, /^Delete selected$/i).trigger("click");
    await flushPromises();

    expect(dialog(wrapper).props("confirmName")).toBe("delete");
    await confirmDialog(wrapper);

    const [url, init] = authFetchMock.mock.calls[0];
    expect(url).toContain("/api/v1/admin/packages/bulk-delete");
    expect(JSON.parse(init.body).items).toHaveLength(1);
    expect(wrapper.text()).toContain("Deleted 1 package.");
  });

  /**
   * The bulk gate is a keyword, so the case an operator's keyboard produces is
   * not a reason to refuse. The single delete above stays case-sensitive: its
   * gate is the package's own name.
   */
  it("accepts the bulk delete keyword typed in upper case", async () => {
    authFetchMock.mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ succeeded_count: 1, failed_count: 0 }),
    });
    const wrapper = await mountPage();

    await selectAll(wrapper);
    await barButton(wrapper, /^Delete selected$/i).trigger("click");
    await flushPromises();

    await typeConfirmName("DELETE");
    confirmButton("Delete").click();
    await flushPromises();

    expect(authFetchMock.mock.calls[0][0]).toContain("/api/v1/admin/packages/bulk-delete");
  });

  it("reports a bulk delete that never reached the server", async () => {
    authFetchMock.mockRejectedValue(new Error("network unreachable"));
    const wrapper = await mountPage();

    await selectAll(wrapper);
    await barButton(wrapper, /^Delete selected$/i).trigger("click");
    await confirmDialog(wrapper);

    expect(wrapper.text()).toContain("network unreachable");
    // Shown as the failure it is, not in the result line beside "Deleted N".
    expect(wrapper.find("p.text-destructive").text()).toContain("network unreachable");
    expect(wrapper.find("output").exists()).toBe(false);
  });

  it("carries the artifact through the row's links and its identity", async () => {
    mocks.listPackages.mockResolvedValue(
      listing([
        pkg({
          package_id: {
            registry: "maven",
            name: "com.example:lib",
            version: "1.0.0",
            artifact: "lib-1.0.0.jar",
          },
        }),
      ]),
    );
    authFetchMock.mockResolvedValue({ ok: true, status: 200, json: async () => ({}) });
    const wrapper = await mountPage();

    const row = wrapper.find("tbody tr");
    expect(row.text()).toContain("lib-1.0.0.jar");

    // The link half. `toContain` on the row text is satisfied by the plain
    // <span> in the name cell, so dropping `artifact` from the version link's
    // query left this test green.
    const versionHref = row.findAll("a").at(-1)!.attributes("href");
    expect(versionHref).toContain("version=1.0.0");
    expect(versionHref).toContain("artifact=lib-1.0.0.jar");
  });

  /**
   * A delete payload asserted against a *real* artifact coordinate.
   *
   * Every payload assertion in this file pins `artifact: null`, so nothing held
   * the field for the one registry where it is load-bearing: for Maven it is
   * the only thing separating `lib-1.0.0.jar` from `lib-1.0.0-sources.jar`, and
   * dropping or mistyping it purges the wrong file irreversibly.
   */
  it("sends the artifact coordinate when deleting a Maven package", async () => {
    mocks.listPackages.mockResolvedValue(
      listing([
        pkg({
          package_id: {
            registry: "maven",
            name: "com.example:lib",
            version: "1.0.0",
            artifact: "lib-1.0.0.jar",
          },
        }),
      ]),
    );
    authFetchMock.mockResolvedValue({ ok: true, status: 200, json: async () => ({}) });
    const wrapper = await mountPage();

    await rowButton(wrapper, /^Delete$/).trigger("click");
    await flushPromises();
    await confirmDialog(wrapper);

    const [url, init] = authFetchMock.mock.calls[0];
    expect(url).toContain("/api/v1/admin/packages/delete");
    expect(JSON.parse(init.body)).toMatchObject({
      registry: "maven",
      name: "com.example:lib",
      version: "1.0.0",
      artifact: "lib-1.0.0.jar",
    });
  });
});

/**
 * Rows are keyed by the package, not by their position in the filtered list.
 *
 * The reported symptom — checked boxes migrating to the wrong rows — does not
 * happen, and it is worth writing down why, because the reasoning is what says
 * these tests are the right ones: `:checked` is a bound value and Vue's
 * `patchDOMProp` writes it on every patch, while `@change` re-syncs `selected`
 * before the next render. Nothing *bound* can drift.
 *
 * Keyboard focus is not a bound value. It lives on a DOM node, and with an
 * index key the node stays put while its contents are re-labelled — so the
 * focus ring ends up on a different package than the one the reader put it on,
 * with no visible change to say so. On a page whose row actions are block,
 * unblock and delete.
 */
describe("AdminPackages row identity", () => {
  const named = (name: string) =>
    pkg({ id: name, package_id: { registry: "npm", name, version: "1.0.0", artifact: null } });

  /** Which package's row holds the focused element. */
  const focusedRow = () =>
    document.activeElement?.closest("tr")?.textContent?.match(/zulu|beta|gamma/)?.[0];

  async function withRows() {
    mocks.listPackages.mockResolvedValue(listing([named("zulu"), named("beta"), named("gamma")]));
    const wrapper = mount(AdminPackages, {
      attachTo: document.body,
      global: { stubs: { SectionTabs: true } },
    });
    await flushPromises();
    return wrapper;
  }

  /** The filter that drops the first row, shifting the other two up. */
  const dropFirst = (wrapper: Awaited<ReturnType<typeof withRows>>) =>
    wrapper.find('input[type="text"], input:not([type])').setValue("a");

  it("keeps keyboard focus on the package it was put on", async () => {
    const wrapper = await withRows();
    const boxes = () => wrapper.findAll('tbody input[type="checkbox"]');

    (boxes()[1].element as HTMLInputElement).focus();
    expect(focusedRow()).toBe("beta");

    await dropFirst(wrapper);
    await flushPromises();
    expect(focusedRow(), "focus followed the row element, not the package").toBe("beta");
    wrapper.unmount();
  });

  /**
   * The counter-assertion, so the test above cannot pass by the list simply
   * not having changed.
   */
  it("really does drop a row when filtered", async () => {
    const wrapper = await withRows();
    expect(wrapper.findAll("tbody tr")).toHaveLength(3);
    await dropFirst(wrapper);
    await flushPromises();
    expect(wrapper.findAll("tbody tr")).toHaveLength(2);
    expect(wrapper.text()).not.toContain("zulu");
    wrapper.unmount();
  });

  it("carries a selection through a filter change", async () => {
    // True before the fix as well — recorded because it is what the report
    // claimed was broken, and a reader who finds this file should be able to
    // see that it was checked rather than assumed.
    const wrapper = await withRows();
    const boxes = () => wrapper.findAll('tbody input[type="checkbox"]');
    await boxes()[1].setValue(true);
    await flushPromises();

    await dropFirst(wrapper);
    await flushPromises();

    const rows = wrapper.findAll("tbody tr").map((r) => r.text().match(/zulu|beta|gamma/)?.[0]);
    const checked = boxes().map((b) => (b.element as HTMLInputElement).checked);
    expect(rows).toEqual(["beta", "gamma"]);
    expect(checked).toEqual([true, false]);
    wrapper.unmount();
  });
});
