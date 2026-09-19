import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { mount, flushPromises, type VueWrapper } from "@vue/test-utils";

const { authFetchMock } = vi.hoisted(() => ({ authFetchMock: vi.fn() }));
vi.mock("@/composables/useAuthFetch", () => ({
  useAuthFetch: () => ({ authFetch: authFetchMock }),
}));

const sdk = vi.hoisted(() => ({
  applyPendingReloadMock: vi.fn(),
  discardPendingReloadMock: vi.fn(),
  reloadConfigMock: vi.fn(),
  listConfigChangesMock: vi.fn(),
  setBannerMock: vi.fn(),
  clearBannerMock: vi.fn(),
}));
vi.mock("@/client/sdk.gen", () => ({
  applyPendingReload: sdk.applyPendingReloadMock,
  discardPendingReload: sdk.discardPendingReloadMock,
  reloadConfig: sdk.reloadConfigMock,
  listConfigChanges: sdk.listConfigChangesMock,
  setBanner: sdk.setBannerMock,
  clearBanner: sdk.clearBannerMock,
}));

import AdminConfigReload from "./AdminConfigReload.vue";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";

const soon = () => new Date(Date.now() + 5 * 60_000).toISOString();
const past = () => new Date(Date.now() - 60_000).toISOString();

const pending = (over: Record<string, unknown> = {}) => ({
  id: "11111111-1111-1111-1111-111111111111",
  created_at: new Date().toISOString(),
  expires_at: soon(),
  source: "file_watcher",
  diff: {
    added_registries: [],
    removed_registries: [],
    changed_registries: [],
    access_config_changed: false,
    limits_changed: false,
  },
  warnings: [],
  ...over,
});

const warning = (over: Record<string, unknown> = {}) => ({
  code: "registry_no_upstreams",
  message: "Registry 'legacy' has no upstreams configured.",
  path: "registries.legacy.upstreams",
  ...over,
});

/** One `authFetch` reply: what the endpoint answered, and how. */
type Reply = { status?: number; json?: unknown };

/**
 * Route every endpoint this page calls. `pendingBody` is the one most tests are
 * about; `over` replaces any other endpoint's reply by URL fragment, so a test
 * that cares about the editor does not have to restate the rest of the page.
 */
function routes(pendingBody: unknown | null, over: Record<string, Reply> = {}) {
  const replies: Record<string, Reply> = {
    "/config/pending":
      pendingBody === null ? { status: 404, json: null } : { status: 200, json: pendingBody },
    "/config/warnings": { json: { warnings: [] } },
    "/config/content": { json: { content: "[server]\nport = 8080\n", is_readonly: false } },
    "/config/validate": { json: { warnings: [] } },
    "/config/from-content": { json: { warnings: [], pending_created: true } },
    ...over,
  };

  authFetchMock.mockImplementation((url: string) => {
    const key = Object.keys(replies).find((k) => url.includes(k));
    const reply = key ? replies[key] : { json: {} };
    const status = reply.status ?? 200;
    return Promise.resolve({
      ok: status < 400,
      status,
      json: async () => reply.json,
    });
  });
}

/**
 * Every wrapper this file mounts, so teardown can unmount them.
 *
 * The page installs a 5 s `setInterval(fetchPending)` and `useBanner` a 30 s
 * one, both cleared only in `onUnmounted`. Mounting ~30 times without
 * unmounting left ~60 live intervals firing into *later* tests' mocks, which
 * makes `authFetchMock`'s call count depend on how long the suite ran rather
 * than on what the test did.
 */
const mounted: VueWrapper[] = [];

async function mountPage() {
  const wrapper = mount(AdminConfigReload, {
    global: { stubs: { SectionTabs: true, RouterLink: true } },
  });
  mounted.push(wrapper);
  await flushPromises();
  await flushPromises();
  return wrapper;
}

type Page = VueWrapper;

const button = (w: Page, label: string | RegExp) =>
  w
    .findAll("button")
    .find((b) => (typeof label === "string" ? b.text().trim() === label : label.test(b.text())));

const applyButton = (w: Page) => button(w, "Apply");

/** The dialogs live behind a teleport, so they are reached as components. */
const dialogFor = (w: Page, action: string) =>
  w.findAllComponents(DestructiveConfirm).find((d) => d.props("action") === action)!;

/**
 * Click the dialog's real confirm button rather than emitting `confirm` on the
 * component: emitting calls the parent handler directly, so `canConfirm` — the
 * thing that gates `:disabled` on this very button — never runs, and every
 * assertion about the gate checks only that a prop was passed.
 *
 * By label, not by position: the dialog renders Cancel, the action, then a
 * trailing "Close", so `.at(-1)` silently clicks a button that does nothing.
 */
function confirmButton(action: string): HTMLButtonElement {
  const buttons = [...document.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')];
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

async function confirmDialog(w: Page, action: string) {
  const dialog = dialogFor(w, action);
  expect(dialog.props("open")).toBe(true);
  let button = confirmButton(action);

  const confirmName = dialog.props("confirmName") as string;
  if (!dialog.props("reversible") && confirmName) {
    // The gate itself, asserted rather than assumed.
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

/** `useBanner` polls raw `fetch`; this is what it finds. */
function stubBanner(body: unknown) {
  vi.stubGlobal(
    "fetch",
    vi.fn(() => Promise.resolve(new Response(JSON.stringify(body), { status: 200 }))),
  );
}

/**
 * The page's question: "what is about to change, and do I accept it".
 *
 * §4.3's two assertions: a diff carrying only `access_config_changed` still
 * renders a decision surface, and an expired pending disables Apply.
 */
describe("AdminConfigReload", () => {
  beforeEach(() => {
    authFetchMock.mockReset();
    stubBanner(null);
    sdk.applyPendingReloadMock
      .mockReset()
      .mockResolvedValue({ data: { diff: { added_registries: ["npm"], removed_registries: [] } } });
    sdk.discardPendingReloadMock.mockReset().mockResolvedValue({ data: {} });
    sdk.reloadConfigMock
      .mockReset()
      .mockResolvedValue({ data: { diff: { added_registries: [], removed_registries: ["old"] } } });
    sdk.listConfigChangesMock.mockReset().mockResolvedValue({ data: { items: [] } });
    sdk.setBannerMock.mockReset().mockResolvedValue({ data: {} });
    sdk.clearBannerMock.mockReset().mockResolvedValue({ data: {} });
  });

  // Unmount *before* clearing the body: a teleported dialog whose nodes have
  // already been removed makes Vue walk a detached fragment and throw.
  afterEach(() => {
    for (const wrapper of mounted.splice(0)) wrapper.unmount();
    document.body.innerHTML = "";
    vi.unstubAllGlobals();
  });

  /**
   * A change to RBAC alone used to render *nothing at all* — an empty badge row
   * that invites a blind apply, on the operator's decision surface.
   */
  it("renders a decision surface for an access-config-only change", async () => {
    routes(pending({ diff: { ...pending().diff, access_config_changed: true } }));
    const wrapper = await mountPage();

    expect(wrapper.text()).toMatch(/access control changed/i);
    expect(applyButton(wrapper)).toBeDefined();
  });

  it("disables Apply once the pending reload has expired", async () => {
    routes(pending({ expires_at: past() }));
    const wrapper = await mountPage();

    expect(applyButton(wrapper)?.attributes("disabled")).toBeDefined();
  });

  it("leaves Apply available while the pending reload is live", async () => {
    routes(pending());
    const wrapper = await mountPage();

    // `?.` makes a missing button satisfy `toBeUndefined()`, so the button has
    // to be shown to exist before its enabled-ness means anything.
    const apply = applyButton(wrapper);
    expect(apply, "Apply is not rendered at all").toBeDefined();
    expect(apply!.attributes("disabled")).toBeUndefined();
  });

  /**
   * A4: the snapshot carries the *candidate's* warnings.
   *
   * `PendingReload` has held them since it was written and the snapshot dropped
   * them, so the one surface that exists to review a change before applying it
   * could not show what the change would warn about. The page worked around it
   * by remembering the `validate` call — which is lost on any page reload, and
   * never existed for a file-watcher reload nobody staged by hand.
   */
  it("shows what the candidate config would warn about", async () => {
    routes(pending({ warnings: [warning()] }));
    const wrapper = await mountPage();

    expect(wrapper.text()).toContain("registries.legacy.upstreams");
    expect(wrapper.text()).toContain("no upstreams configured");
  });

  it("says there is nothing staged rather than rendering an empty decision", async () => {
    routes(null);
    const wrapper = await mountPage();
    expect(wrapper.text()).toMatch(/no pending reload/i);
    expect(applyButton(wrapper)).toBeUndefined();
  });

  it("names every part of the diff it is asking the operator to accept", async () => {
    routes(
      pending({
        diff: {
          added_registries: ["npm"],
          removed_registries: ["legacy"],
          changed_registries: [{ name: "pypi", fields: ["upstreams", "mode"] }],
          access_config_changed: true,
          limits_changed: true,
        },
      }),
    );
    const wrapper = await mountPage();
    const text = wrapper.text();

    expect(text).toContain("+npm");
    expect(text).toContain("-legacy");
    // A changed registry used to render as a bare `~pypi`, saying nothing about what changed.
    expect(text).toContain("~pypi (upstreams, mode)");
    expect(text).toMatch(/access control changed/i);
    expect(text).toMatch(/limits changed/i);
  });

  // ── Applying, discarding, forcing ───────────────────────────────────────────

  /**
   * PRODUCT.md principle 2: applying a staged config changes a running instance
   * other people depend on. The page's own copy used to admit there was "no
   * confirmation step"; now the count in the dialog is computed from the diff.
   */
  it("counts the staged changes in the apply confirmation, then applies them", async () => {
    routes(
      pending({
        diff: {
          added_registries: ["npm"],
          removed_registries: ["legacy"],
          changed_registries: [{ name: "pypi", fields: [] }],
          access_config_changed: true,
          limits_changed: false,
        },
      }),
    );
    const wrapper = await mountPage();

    await applyButton(wrapper)!.trigger("click");
    expect(dialogFor(wrapper, "Apply").props("count")).toBe(4);

    await confirmDialog(wrapper, "Apply");

    expect(sdk.applyPendingReloadMock).toHaveBeenCalled();
    expect(wrapper.text()).toContain("Applied: +1 -0 registries");
    // The pending is gone, so the page must not still offer to apply it.
    expect(applyButton(wrapper)).toBeUndefined();
  });

  it("reports an apply failure instead of claiming the config changed", async () => {
    routes(pending());
    sdk.applyPendingReloadMock.mockResolvedValue({ error: { message: "validation failed" } });
    const wrapper = await mountPage();

    await applyButton(wrapper)!.trigger("click");
    await confirmDialog(wrapper, "Apply");

    expect(wrapper.text()).toContain("validation failed");
    expect(wrapper.text()).not.toContain("Applied:");
  });

  it("discards a pending reload without a confirmation, since nothing is lost", async () => {
    routes(pending());
    const wrapper = await mountPage();

    await button(wrapper, "Discard")!.trigger("click");
    await flushPromises();

    expect(sdk.discardPendingReloadMock).toHaveBeenCalled();
    expect(wrapper.text()).toMatch(/no pending reload/i);
  });

  it("reports a failed discard", async () => {
    routes(pending());
    sdk.discardPendingReloadMock.mockResolvedValue({ error: { message: "already gone" } });
    const wrapper = await mountPage();

    await button(wrapper, "Discard")!.trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("already gone");
  });

  /**
   * Force reload applies whatever is on disk *unseen*. That is the one action
   * here the operator cannot review first, so it is the one that must not be
   * reversible-by-default friction.
   */
  it("makes force-reload type-to-confirm, then reloads", async () => {
    routes(null);
    const wrapper = await mountPage();

    await button(wrapper, "Reload Now")!.trigger("click");
    const dialog = dialogFor(wrapper, "Force Reload Now");
    expect(dialog.props("reversible")).toBeFalsy();
    expect(dialog.props("confirmName")).toBe("reload");

    await confirmDialog(wrapper, "Force Reload Now");

    expect(sdk.reloadConfigMock).toHaveBeenCalled();
    expect(wrapper.text()).toContain("Reloaded: +0 -1 registries");
  });

  it("reports a failed force reload", async () => {
    routes(null);
    sdk.reloadConfigMock.mockResolvedValue({ error: { message: "config.toml is malformed" } });
    const wrapper = await mountPage();

    await button(wrapper, "Reload Now")!.trigger("click");
    await confirmDialog(wrapper, "Force Reload Now");

    expect(wrapper.text()).toContain("config.toml is malformed");
  });

  // ── The editor ──────────────────────────────────────────────────────────────

  it("stages nothing until the content has been validated", async () => {
    routes(null);
    const wrapper = await mountPage();

    expect(button(wrapper, "Create Pending Reload")?.attributes("disabled")).toBeDefined();

    await button(wrapper, "Validate")!.trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain('Config is valid. Click "Create Pending Reload"');
    const create = button(wrapper, "Create Pending Reload");
    expect(create, "Create Pending Reload is not rendered at all").toBeDefined();
    expect(create!.attributes("disabled")).toBeUndefined();
  });

  it("re-arms validation when the content is edited again", async () => {
    routes(null);
    const wrapper = await mountPage();

    await button(wrapper, "Validate")!.trigger("click");
    await flushPromises();
    await wrapper.get("textarea").setValue("[server]\nport = 9090\n");

    expect(button(wrapper, "Create Pending Reload")?.attributes("disabled")).toBeDefined();
  });

  it("shows what a valid candidate would warn about before it is staged", async () => {
    routes(null, { "/config/validate": { json: { warnings: [warning()] } } });
    const wrapper = await mountPage();

    await button(wrapper, "Validate")!.trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("registries.legacy.upstreams");
    expect(wrapper.text()).toContain("no upstreams configured");
  });

  it("surfaces the parser's complaint when validation fails", async () => {
    routes(null, {
      "/config/validate": { status: 400, json: { error: "expected `=` at line 3" } },
    });
    const wrapper = await mountPage();

    await button(wrapper, "Validate")!.trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("expected `=` at line 3");
    expect(wrapper.text()).not.toContain("Config is valid");
  });

  it("creates the pending reload and points at the review below", async () => {
    routes(null);
    const wrapper = await mountPage();

    await button(wrapper, "Validate")!.trigger("click");
    await flushPromises();
    await button(wrapper, "Create Pending Reload")!.trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("Pending reload created.");
  });

  /**
   * The server deduped: these exact bytes are already loaded, so nothing was
   * staged. A green "created" here sends the admin to an Apply button that
   * answers "no pending reload".
   */
  it("says nothing was staged when the content is identical to the running config", async () => {
    routes(null, { "/config/from-content": { json: { warnings: [], pending_created: false } } });
    const wrapper = await mountPage();

    await button(wrapper, "Validate")!.trigger("click");
    await flushPromises();
    await button(wrapper, "Create Pending Reload")!.trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("Nothing to stage");
    expect(wrapper.text()).not.toContain("Pending reload created.");
  });

  it("surfaces a failure to stage the content", async () => {
    routes(null, { "/config/from-content": { status: 500, json: { error: "disk full" } } });
    const wrapper = await mountPage();

    await button(wrapper, "Validate")!.trigger("click");
    await flushPromises();
    await button(wrapper, "Create Pending Reload")!.trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("disk full");
  });

  it("says why the config could not be read instead of showing an empty editor", async () => {
    routes(null, { "/config/content": { status: 500, json: {} } });
    const wrapper = await mountPage();

    expect(wrapper.text()).toContain("HTTP 500");
  });

  /**
   * RFC 0003 §6.4: a read-only config is a different screen, not this one with
   * the buttons greyed out.
   */
  it("swaps the editor for the read-only view when the config cannot be written", async () => {
    routes(null, {
      "/config/content": { json: { content: "[server]\nport = 8080\n", is_readonly: true } },
    });
    const wrapper = await mountPage();

    expect(wrapper.find("textarea").exists()).toBe(false);
    expect(button(wrapper, "Validate")).toBeUndefined();
    expect(wrapper.text()).toContain("read-only");
  });

  it("says hot reload is off, and withdraws every control that depends on it", async () => {
    routes(null, { "/config/pending": { status: 503, json: {} } });
    const wrapper = await mountPage();

    expect(wrapper.text()).toContain("Hot reload is disabled on this instance");
    expect(wrapper.text()).toContain("BATLEHUB_DISABLE_HOT_RELOAD=1");
    expect(button(wrapper, "Reload Now")).toBeUndefined();
    expect(button(wrapper, "Validate")?.attributes("disabled")).toBeDefined();
  });

  // ── Warnings about the config in force ──────────────────────────────────────

  it("lists the warnings the running config raises, and lets them be dismissed", async () => {
    routes(null, { "/config/warnings": { json: { warnings: [warning()] } } });
    const wrapper = await mountPage();

    expect(wrapper.text()).toContain("Configuration warnings (1)");

    await button(wrapper, "Dismiss")!.trigger("click");
    expect(wrapper.text()).not.toContain("Configuration warnings");
  });

  /**
   * The pruning filter, which had no coverage despite a test for this panel.
   *
   * `fetchWarnings` rebuilds `dismissedCodes` by intersecting it with the fresh
   * server list, so a warning that goes away and comes back is worth showing
   * again. Invert that predicate and you either resurrect every dismissal on
   * the next reload or suppress a warning that has genuinely returned — and the
   * dismissal test above, which ends at the click, passes either way.
   */
  it("keeps a dismissal while the warning is still raised", async () => {
    routes(null, { "/config/warnings": { json: { warnings: [warning()] } } });
    const wrapper = await mountPage();

    await button(wrapper, "Dismiss")!.trigger("click");
    expect(wrapper.text()).not.toContain("Configuration warnings");

    // Re-read with the same warning still raised: the dismissal must survive,
    // or every reload resurrects everything the operator has already read.
    await button(wrapper, "Reload Now")!.trigger("click");
    await confirmDialog(wrapper, "Force Reload Now");
    expect(wrapper.text()).not.toContain("Configuration warnings");
  });

  it("shows a dismissed warning again once it has gone and come back", async () => {
    routes(null, { "/config/warnings": { json: { warnings: [warning()] } } });
    const wrapper = await mountPage();

    await button(wrapper, "Dismiss")!.trigger("click");
    expect(wrapper.text()).not.toContain("Configuration warnings");

    // It clears — the dismissal has nothing left to apply to.
    routes(null, { "/config/warnings": { json: { warnings: [] } } });
    await button(wrapper, "Reload Now")!.trigger("click");
    await confirmDialog(wrapper, "Force Reload Now");
    expect(wrapper.text()).not.toContain("Configuration warnings");

    // And it comes back, so it must be visible rather than still dismissed.
    routes(null, { "/config/warnings": { json: { warnings: [warning()] } } });
    await button(wrapper, "Reload Now")!.trigger("click");
    await confirmDialog(wrapper, "Force Reload Now");
    expect(wrapper.text()).toContain("Configuration warnings (1)");
  });

  // ── The global banner ───────────────────────────────────────────────────────

  it("refuses to set an empty banner", async () => {
    routes(null);
    const wrapper = await mountPage();

    expect(button(wrapper, "Set Banner")?.attributes("disabled")).toBeDefined();
    await wrapper.get("#banner-message").setValue("   ");
    expect(button(wrapper, "Set Banner")?.attributes("disabled")).toBeDefined();
  });

  it("sets the banner at the chosen level and clears the field", async () => {
    routes(null);
    const wrapper = await mountPage();

    await wrapper.get("#banner-message").setValue("Upgrading at 22:00 UTC");
    await wrapper.get("#banner-level").setValue("warning");
    await button(wrapper, "Set Banner")!.trigger("click");
    await flushPromises();

    expect(sdk.setBannerMock).toHaveBeenCalledWith({
      body: { message: "Upgrading at 22:00 UTC", level: "warning" },
    });
    expect(wrapper.text()).toContain("Banner set");
    expect((wrapper.get("#banner-message").element as HTMLInputElement).value).toBe("");
  });

  it("reports a failure to set the banner", async () => {
    routes(null);
    sdk.setBannerMock.mockResolvedValue({ error: { message: "not permitted" } });
    const wrapper = await mountPage();

    await wrapper.get("#banner-message").setValue("hello");
    await button(wrapper, "Set Banner")!.trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("not permitted");
  });

  it("only offers to clear a banner that exists, and shows who set it", async () => {
    routes(null);
    const noBanner = await mountPage();
    expect(noBanner.text()).toContain("No banner currently set.");
    expect(button(noBanner, "Clear Banner")?.attributes("disabled")).toBeDefined();

    stubBanner({ message: "Read-only mode", level: "error", set_at: "now", set_by: "alice" });
    const wrapper = await mountPage();

    expect(wrapper.text()).toContain("Read-only mode");
    expect(wrapper.text()).toContain("alice");

    await button(wrapper, "Clear Banner")!.trigger("click");
    await flushPromises();

    expect(sdk.clearBannerMock).toHaveBeenCalled();
    expect(wrapper.text()).toContain("Banner cleared");
  });

  it("reports a failure to clear the banner", async () => {
    routes(null);
    stubBanner({ message: "Read-only mode", level: "error", set_at: "now", set_by: "alice" });
    sdk.clearBannerMock.mockResolvedValue({ error: { message: "storage unreachable" } });
    const wrapper = await mountPage();

    await button(wrapper, "Clear Banner")!.trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("storage unreachable");
  });

  // ── History ─────────────────────────────────────────────────────────────────

  it("says the history is empty rather than rendering an empty table", async () => {
    routes(null);
    const wrapper = await mountPage();

    expect(wrapper.text()).toContain("No changes recorded yet.");
    expect(wrapper.find("table").exists()).toBe(false);
  });

  it("lists past changes and expands one to its diff on click", async () => {
    routes(null);
    sdk.listConfigChangesMock.mockResolvedValue({
      data: {
        items: [
          {
            id: "row-1",
            triggered_at: "2026-08-12T10:00:00Z",
            triggered_by: "alice",
            status: "applied",
            summary: "+1 registry",
            diff: { added_registries: ["npm"] },
          },
          {
            id: "row-2",
            triggered_at: "2026-08-12T09:00:00Z",
            triggered_by: "file_watcher",
            status: "failed",
            summary: "invalid TOML",
            diff: {},
          },
        ],
      },
    });
    const wrapper = await mountPage();

    expect(wrapper.findAll("tbody tr")).toHaveLength(2);
    expect(wrapper.text()).toContain("alice");
    expect(wrapper.text()).toContain("invalid TOML");
    // A failed reload must not read like an applied one.
    const failed = wrapper.findAll("tbody tr").find((r) => r.text().includes("failed"))!;
    expect(failed.html()).toContain("text-destructive");

    // The disclosure is a button, not the row: a keyboard reaches it, and
    // `aria-expanded` is what says whether the diff below is open.
    const disclosure = wrapper.findAll("tbody tr")[0].find("button");
    expect(disclosure.exists()).toBe(true);
    expect(disclosure.attributes("aria-expanded")).toBe("false");

    await disclosure.trigger("click");
    expect(wrapper.find("pre").text()).toContain("added_registries");
    expect(wrapper.findAll("tbody tr")[0].find("button").attributes("aria-expanded")).toBe("true");
    // The button names the row it opens, and that row exists under that id.
    const controls = wrapper.findAll("tbody tr")[0].find("button").attributes("aria-controls")!;
    expect(wrapper.find(`#${controls}`).exists()).toBe(true);

    // Pressing it again collapses it.
    await wrapper.findAll("tbody tr")[0].find("button").trigger("click");
    expect(wrapper.find("pre").exists()).toBe(false);
    expect(wrapper.findAll("tbody tr")[0].find("button").attributes("aria-expanded")).toBe("false");
  });
});
