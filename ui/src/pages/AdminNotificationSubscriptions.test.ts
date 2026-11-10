import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const mocks = vi.hoisted(() => ({
  listSubscriptions: vi.fn(),
  listNotificationChannels: vi.fn(),
  createSubscription: vi.fn(),
  updateSubscription: vi.fn(),
  deleteSubscription: vi.fn(),
  testSubscription: vi.fn(),
  listRegistries: vi.fn(),
  explorePackages: vi.fn(),
  explorePackageDetail: vi.fn(),
  listSubjects: vi.fn(),
}));
vi.mock("@/client/sdk.gen", () => mocks);

import AdminNotificationSubscriptions from "./AdminNotificationSubscriptions.vue";

const sub = (over: Record<string, unknown> = {}) => ({
  id: "sub-1",
  registry: "npm",
  package_name: null,
  event_types: ["package_published"],
  channel_name: "my-slack",
  enabled: true,
  ...over,
});

async function mountPage() {
  const wrapper = mount(AdminNotificationSubscriptions, {
    // `attachTo` because the edit form lives in a `Dialog`, which teleports.
    attachTo: document.body,
    global: { stubs: { SectionTabs: true, RouterLink: true } },
  });
  await flushPromises();
  return wrapper;
}

/** The dialog is teleported out of the wrapper, so read the document. */
const dialogText = () => document.body.textContent ?? "";
const dialogButtons = () => [...document.body.querySelectorAll("button")];

type Page = Awaited<ReturnType<typeof mountPage>>;

const editButton = (w: Page) => w.findAll("button").find((b) => /^edit$/i.test(b.text().trim()))!;

const rowButton = (w: Page, label: RegExp) =>
  w.findAll("button").find((b) => label.test(b.text().trim()))!;

/**
 * A button inside the teleported dialog, matched on its trimmed label.
 *
 * Scoped to the dialog itself: `attachTo: document.body` puts the page in the
 * same document, and the row's own "Delete" would otherwise answer a search for
 * the confirmation's.
 */
const inDialog = (label: RegExp) =>
  [...document.querySelectorAll('[role="dialog"] button')].find((b) =>
    label.test((b.textContent ?? "").trim()),
  ) as HTMLButtonElement;

async function clickInDialog(label: RegExp) {
  inDialog(label).click();
  await flushPromises();
}

async function fill(selector: string, value: string) {
  const el = document.querySelector<HTMLInputElement>(selector)!;
  el.value = value;
  el.dispatchEvent(new Event("input"));
  await flushPromises();
}

/** Open the create dialog from the page header. */
async function openCreate(w: Page) {
  await rowButton(w, /^New Subscription$/).trigger("click");
  await flushPromises();
}

/** The page's question: "what gets notified where, and does it work". */
describe("AdminNotificationSubscriptions", () => {
  beforeEach(() => {
    mocks.listSubscriptions.mockReset().mockResolvedValue({ data: [sub()] });
    mocks.listNotificationChannels
      .mockReset()
      .mockResolvedValue({ data: [{ name: "my-slack", kind: "slack" }] });
    mocks.createSubscription.mockReset().mockResolvedValue({ data: {} });
    mocks.updateSubscription.mockReset().mockResolvedValue({ data: {} });
    mocks.deleteSubscription.mockReset().mockResolvedValue({ data: {} });
    mocks.testSubscription.mockReset().mockResolvedValue({ data: {} });
    mocks.listRegistries.mockReset().mockResolvedValue({ data: [{ name: "npm", type: "npm" }] });
    mocks.explorePackages.mockReset().mockResolvedValue({ data: { items: [], total: 0 } });
    mocks.listSubjects.mockReset().mockResolvedValue({ data: { items: [], truncated: false } });
  });

  /**
   * §4.3: an event type outside `ALL_EVENT_TYPES` is not silently re-saved.
   *
   * `openEdit` copies `sub.event_types` verbatim and the chip row renders only
   * the types this console knows, so a type the *server* knows and the console
   * does not was invisible in the form and re-saved on submit — an operator
   * kept a rule they could neither see nor remove. A server that grows an event
   * type before the console does is the expected order of deployment.
   */
  // `attachTo: document.body` leaves the teleported dialog behind, and the
  // assertions read the document — so a previous test's DOM would answer them.
  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("names an event type it does not know rather than carrying it invisibly", async () => {
    mocks.listSubscriptions.mockResolvedValue({
      data: [sub({ event_types: ["package_published", "package_quarantined"] })],
    });
    const wrapper = await mountPage();
    await editButton(wrapper).trigger("click");
    await flushPromises();

    expect(dialogText()).toContain("package_quarantined");
    expect(dialogText()).toMatch(/does not know/i);
  });

  it("lets an unknown event type be removed", async () => {
    mocks.listSubscriptions.mockResolvedValue({
      data: [sub({ event_types: ["package_published", "package_quarantined"] })],
    });
    const wrapper = await mountPage();
    await editButton(wrapper).trigger("click");
    await flushPromises();

    const drop = dialogButtons().find((b) => b.textContent?.includes("package_quarantined"))!;
    drop.click();
    await flushPromises();

    expect(dialogText()).not.toContain("package_quarantined");
  });

  it("says nothing about unknown types when there are none", async () => {
    const wrapper = await mountPage();
    await editButton(wrapper).trigger("click");
    await flushPromises();
    expect(dialogText()).not.toMatch(/does not know/i);
  });

  /**
   * The colour of the test result used to be decided by
   * `testMsg.startsWith("Test failed")` — a translated sentence parsed for its
   * own meaning, correct in English and silently green in French.
   */
  it("reports a failed test as a failure, whatever language it is in", async () => {
    mocks.testSubscription.mockResolvedValue({ error: { message: "channel unreachable" } });
    const wrapper = await mountPage();
    await wrapper
      .findAll("button")
      .find((b) => /^test$/i.test(b.text().trim()))!
      .trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("channel unreachable");
    expect((wrapper.vm as unknown as { testFailed: boolean }).testFailed).toBe(true);
  });

  it("surfaces a load error rather than an empty list", async () => {
    mocks.listSubscriptions.mockResolvedValue({ error: { message: "db down" } });
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("db down");
  });

  it("says the test was sent when it was", async () => {
    const wrapper = await mountPage();
    await rowButton(wrapper, /^Test$/).trigger("click");
    await flushPromises();

    expect(mocks.testSubscription).toHaveBeenCalledWith({ path: { id: "sub-1" } });
    expect(wrapper.text()).toContain("Test sent successfully.");
    expect((wrapper.vm as unknown as { testFailed: boolean }).testFailed).toBe(false);
  });

  it("reports a test that threw before it reached the channel", async () => {
    mocks.testSubscription.mockRejectedValue(new Error("network unreachable"));
    const wrapper = await mountPage();
    await rowButton(wrapper, /^Test$/).trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("network unreachable");
    expect((wrapper.vm as unknown as { testFailed: boolean }).testFailed).toBe(true);
  });

  /** A subscription scoped to nothing in particular applies to everything. */
  it("renders an unscoped subscription as a wildcard rather than a blank", async () => {
    mocks.listSubscriptions.mockResolvedValue({
      data: [sub({ registry: null, package_name: null })],
    });
    const wrapper = await mountPage();
    const cells = wrapper.find("tbody tr").findAll("td");

    expect(cells[0].text()).toBe("*");
    expect(cells[1].text()).toBe("*");
  });

  it("says nothing is configured rather than showing an empty table", async () => {
    mocks.listSubscriptions.mockResolvedValue({ data: [] });
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("No subscriptions configured.");
  });

  // ── Enabling and disabling ──────────────────────────────────────────────────

  it("flips a subscription's enabled flag without losing the rest of it", async () => {
    const wrapper = await mountPage();

    await wrapper.find('button[role="switch"]').trigger("click");
    await flushPromises();

    expect(mocks.updateSubscription).toHaveBeenCalledWith({
      path: { id: "sub-1" },
      body: {
        registry: "npm",
        package_name: null,
        event_types: ["package_published"],
        channel_name: "my-slack",
        enabled: false,
      },
    });
    expect(mocks.listSubscriptions).toHaveBeenCalledTimes(2);
  });

  it("reports a toggle the server refused", async () => {
    mocks.updateSubscription.mockResolvedValue({ error: { message: "read-only mode" } });
    const wrapper = await mountPage();

    await wrapper.find('button[role="switch"]').trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("read-only mode");
  });

  // ── Creating and editing ────────────────────────────────────────────────────

  it("will not save a subscription with no channel or no events", async () => {
    const wrapper = await mountPage();
    await openCreate(wrapper);

    // No channel yet.
    expect(inDialog(/^Create$/).hasAttribute("disabled")).toBe(true);

    await clickInDialog(/^my-slack$/);
    expect(inDialog(/^Create$/).hasAttribute("disabled")).toBe(false);

    // Turning off the last event type disarms it again.
    await clickInDialog(/^published$/);
    expect(inDialog(/^Create$/).hasAttribute("disabled")).toBe(true);
  });

  it("creates a subscription, sending blank scopes as null rather than empty", async () => {
    const wrapper = await mountPage();
    await openCreate(wrapper);

    await clickInDialog(/^my-slack$/);
    await clickInDialog(/^yanked$/);
    await clickInDialog(/^Create$/);

    expect(mocks.createSubscription).toHaveBeenCalledWith({
      body: {
        registry: null,
        package_name: null,
        event_types: ["package_published", "package_yanked"],
        channel_name: "my-slack",
        enabled: true,
      },
    });
    expect(mocks.listSubscriptions).toHaveBeenCalledTimes(2);
  });

  it("scopes a new subscription to a package when one is named", async () => {
    const wrapper = await mountPage();
    await openCreate(wrapper);

    await fill("#notif-package-name", "  serde  ");
    await clickInDialog(/^my-slack$/);
    await clickInDialog(/^Create$/);

    expect(mocks.createSubscription).toHaveBeenCalledWith(
      expect.objectContaining({ body: expect.objectContaining({ package_name: "serde" }) }),
    );
  });

  it("keeps the dialog open and says why when a create is refused", async () => {
    mocks.createSubscription.mockResolvedValue({ error: { message: "duplicate subscription" } });
    const wrapper = await mountPage();
    await openCreate(wrapper);

    await clickInDialog(/^my-slack$/);
    await clickInDialog(/^Create$/);

    expect(dialogText()).toContain("duplicate subscription");
    expect(inDialog(/^Create$/)).toBeDefined();
  });

  it("reports a create that threw", async () => {
    mocks.createSubscription.mockRejectedValue(new Error("gateway timeout"));
    const wrapper = await mountPage();
    await openCreate(wrapper);

    await clickInDialog(/^my-slack$/);
    await clickInDialog(/^Create$/);

    expect(dialogText()).toContain("gateway timeout");
  });

  it("edits an existing subscription in place rather than creating a second", async () => {
    const wrapper = await mountPage();
    await editButton(wrapper).trigger("click");
    await flushPromises();

    expect(dialogText()).toContain("Edit Subscription");
    await clickInDialog(/^deleted$/);
    await clickInDialog(/^Update$/);

    expect(mocks.createSubscription).not.toHaveBeenCalled();
    expect(mocks.updateSubscription).toHaveBeenCalledWith({
      path: { id: "sub-1" },
      body: {
        registry: "npm",
        package_name: null,
        event_types: ["package_published", "package_deleted"],
        channel_name: "my-slack",
        enabled: true,
      },
    });
  });

  it("reports an edit the server refused", async () => {
    mocks.updateSubscription.mockResolvedValue({ error: { message: "channel was removed" } });
    const wrapper = await mountPage();
    await editButton(wrapper).trigger("click");
    await flushPromises();
    await clickInDialog(/^Update$/);

    expect(dialogText()).toContain("channel was removed");
  });

  it("abandons a subscription on cancel", async () => {
    const wrapper = await mountPage();
    await openCreate(wrapper);
    await clickInDialog(/^my-slack$/);
    await clickInDialog(/^Cancel$/);

    expect(mocks.createSubscription).not.toHaveBeenCalled();
    expect(document.querySelector("#notif-channel")).toBeNull();
  });

  /**
   * The channel field is required but was never validated — a typo saved
   * cleanly and the subscription silently never dispatched. A warning rather
   * than a block: an operator may be creating a subscription for a channel they
   * are about to add to `config.toml`.
   */
  it("warns about a channel that does not exist, without blocking the save", async () => {
    const wrapper = await mountPage();
    await openCreate(wrapper);

    await fill("#notif-channel", "my-slak");
    expect(dialogText()).toMatch(/No channel named my-slak is configured/);
    expect(inDialog(/^Create$/).hasAttribute("disabled")).toBe(false);

    await fill("#notif-channel", "my-slack");
    expect(dialogText()).not.toMatch(/No channel named/);
  });

  it("says where channels come from when none are configured", async () => {
    mocks.listNotificationChannels.mockResolvedValue({ data: [] });
    const wrapper = await mountPage();
    await openCreate(wrapper);

    expect(dialogText()).toContain("No channels configured.");
    expect(dialogText()).toContain("[[notifications.channels]]");
    // With no channel list to compare against, an unknown name cannot be judged.
    await fill("#notif-channel", "anything");
    expect(dialogText()).not.toMatch(/No channel named/);
  });

  // ── Deleting ────────────────────────────────────────────────────────────────

  it("deletes a subscription once the confirmation is given", async () => {
    const wrapper = await mountPage();

    await rowButton(wrapper, /^Delete$/).trigger("click");
    await flushPromises();
    expect(dialogText()).toContain("Delete 1 subscription of my-slack");

    // Irreversible, so the channel's name is typed before Delete enables.
    await fill("#destructive-confirm-name", "my-slack");
    await clickInDialog(/^Delete$/);

    expect(mocks.deleteSubscription).toHaveBeenCalledWith({ path: { id: "sub-1" } });
    expect(mocks.listSubscriptions).toHaveBeenCalledTimes(2);
  });

  it("keeps the confirmation open and says why when a delete fails", async () => {
    mocks.deleteSubscription.mockResolvedValue({ error: { message: "subscription is locked" } });
    const wrapper = await mountPage();

    await rowButton(wrapper, /^Delete$/).trigger("click");
    await flushPromises();
    await fill("#destructive-confirm-name", "my-slack");
    await clickInDialog(/^Delete$/);

    expect(dialogText()).toContain("subscription is locked");
  });

  it("keeps the subscription when the delete is cancelled", async () => {
    const wrapper = await mountPage();

    await rowButton(wrapper, /^Delete$/).trigger("click");
    await flushPromises();
    await clickInDialog(/^Cancel$/);

    expect(mocks.deleteSubscription).not.toHaveBeenCalled();
    expect(wrapper.findAll("tbody tr")).toHaveLength(1);
  });
});
