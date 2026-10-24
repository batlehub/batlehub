import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { auditLogMock } = vi.hoisted(() => ({ auditLogMock: vi.fn() }));
vi.mock("@/client/sdk.gen", () => ({
  auditLog: auditLogMock,
  // The registry filter's suggestions; the tests here are about the log.
  listRegistries: vi.fn().mockResolvedValue({ data: [{ name: "npm", type: "npm" }] }),
}));

import AuditLog from "./AuditLog.vue";

const event = (over: Record<string, unknown> = {}) => ({
  id: crypto.randomUUID(),
  package_id: { registry: "npm", name: "left-pad", version: "1.0.0" },
  user_id: "alice",
  user_role: "user",
  action: "download",
  result: { outcome: "allowed" },
  timestamp: "2026-08-12T10:00:00Z",
  ...over,
});

/** What the endpoint actually answers with: the paginated envelope. */
const envelope = (items: unknown[], over: Record<string, unknown> = {}) => ({
  items,
  total: items.length,
  page: 0,
  per_page: 100,
  ...over,
});

/**
 * Teardown is load-bearing since the filter boxes gained a debounce: a test
 * that types and does not wait leaves a timer behind, and it fires inside the
 * *next* test — which then sees a request for a filter it never set. It also
 * exercises the `onBeforeUnmount` that stops the same thing happening when a
 * reader navigates away mid-keystroke.
 */
let active: ReturnType<typeof mount> | null = null;

afterEach(() => {
  active?.unmount();
  active = null;
});

async function mountPage() {
  const wrapper = mount(AuditLog, {
    global: { stubs: { RouterLink: { template: "<a><slot /></a>" }, SectionTabs: true } },
  });
  await flushPromises();
  active = wrapper;
  return wrapper;
}

/**
 * The two free-text boxes are debounced by 300 ms — every keystroke used to be
 * a request. Tests that type into them have to let that settle, or they assert
 * against the query as it was before the character they just typed.
 */
async function typeFilter(
  wrapper: Awaited<ReturnType<typeof mountPage>>,
  selector: string,
  value: string,
) {
  await wrapper.find(selector).setValue(value);
  await new Promise((resolve) => setTimeout(resolve, 350));
  await flushPromises();
}

describe("AuditLog", () => {
  beforeEach(() => {
    auditLogMock
      .mockReset()
      .mockResolvedValue({ data: envelope([event(), event({ user_id: "bob" })]) });
  });

  /**
   * The regression this file exists for. The page declared
   * `useApi<AccessEvent[]>` against a hand-written interface while the endpoint
   * returns `{ items, total, page, per_page }`, so it rendered "No events
   * recorded yet." over a full page of events — verified in a browser against
   * six fixture rows before the fix.
   *
   * On an audit surface that is the worst failure mode available: it does not
   * look broken, it looks like nothing happened.
   */
  it("renders the events inside the response envelope", async () => {
    const wrapper = await mountPage();
    expect(wrapper.findAll("tbody tr")).toHaveLength(2);
    expect(wrapper.text()).toContain("alice");
    expect(wrapper.text()).toContain("bob");
    expect(wrapper.text()).not.toContain("No events recorded yet");
  });

  it("says nothing happened only when nothing happened", async () => {
    auditLogMock.mockResolvedValue({ data: envelope([]) });
    const wrapper = await mountPage();
    expect(wrapper.findAll("tbody tr")).toHaveLength(0);
  });

  /**
   * `total` is the server's count; `items.length` is the page. Reporting the
   * page as the total silently caps at `per_page`.
   */
  it("reports the server's total, not the loaded page's length", async () => {
    auditLogMock.mockResolvedValue({ data: envelope([event()], { total: 4_312 }) });
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("4312");
  });

  /**
   * The filters are the *server's* (RFC 0004-bis §6.1).
   *
   * This page used to fetch the newest hundred rows and filter them in the
   * browser, which answers "no events match" about a page while presenting it
   * as an answer about the log. The endpoint has accepted
   * `registry|user_id|from|to|denied_only|page|per_page` since it was written.
   */
  it("sends the user filter to the server rather than filtering the page", async () => {
    const wrapper = await mountPage();
    await typeFilter(wrapper, "#al-user", "bob");

    expect(auditLogMock).toHaveBeenLastCalledWith(
      expect.objectContaining({ query: expect.objectContaining({ user_id: "bob" }) }),
    );
  });

  /**
   * `denied_only` is the single most-used audit filter and had no control at
   * all. Filtering denials client-side is worse than not offering it: it
   * silently answers "no denials" for anything past the newest hundred rows.
   */
  it("sends denied_only to the server", async () => {
    const wrapper = await mountPage();
    // `Switch` is a `role="switch"` button, not a checkbox — it is clicked.
    await wrapper.find('[role="switch"]').trigger("click");
    await flushPromises();

    expect(auditLogMock).toHaveBeenLastCalledWith(
      expect.objectContaining({ query: expect.objectContaining({ denied_only: true }) }),
    );
  });

  it("returns to the first page when a filter changes", async () => {
    auditLogMock.mockResolvedValue({ data: envelope([event()], { total: 450 }) });
    const wrapper = await mountPage();

    // Page forward, then narrow. Staying on page 4 of a result set that just
    // shrank to one page is how a filter looks like it returned nothing.
    await wrapper.findAll("nav button").at(-1)!.trigger("click");
    await flushPromises();
    expect(auditLogMock).toHaveBeenLastCalledWith(
      expect.objectContaining({ query: expect.objectContaining({ page: 1 }) }),
    );

    await typeFilter(wrapper, "#al-user", "bob");
    expect(auditLogMock).toHaveBeenLastCalledWith(
      expect.objectContaining({ query: expect.objectContaining({ page: 0, user_id: "bob" }) }),
    );
  });

  /**
   * "No events match these filters" and "this instance has recorded nothing"
   * are different facts. An audit surface that conflates them tells an operator
   * the wrong one — the same defect as the envelope bug above, one level down.
   */
  it("distinguishes an empty filter result from an empty log", async () => {
    auditLogMock.mockResolvedValue({ data: envelope([]) });
    const wrapper = await mountPage();
    expect(wrapper.text()).toMatch(/no events recorded/i);

    await wrapper.find("#al-user").setValue("nobody");
    await flushPromises();
    expect(wrapper.text()).toMatch(/match/i);
    expect(wrapper.text()).not.toMatch(/no events recorded/i);
  });

  /**
   * `package_id` is null for account- and network-wide actions — blocking a
   * user, blocking an IP, purging the trail. The hand-written interface
   * declared it required, so the template dereferenced it unguarded and those
   * rows would have thrown once the envelope fix let them render at all. The
   * generated type is what made this visible.
   */
  it("renders an account-wide action, which carries no package", async () => {
    auditLogMock.mockResolvedValue({
      data: envelope([
        event({ package_id: null, action: "block_user", user_id: "admin" }),
        event(),
      ]),
    });
    const wrapper = await mountPage();
    expect(wrapper.findAll("tbody tr")).toHaveLength(2);
    expect(wrapper.text()).toContain("block_user");
    expect(wrapper.text()).toMatch(/account-wide/i);
  });

  it("surfaces an error rather than an empty log", async () => {
    auditLogMock.mockResolvedValue({ error: { message: "boom" } });
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("boom");
  });
});

/**
 * One request per *filter*, not one per keystroke.
 *
 * `query` is a dependency of `useApi`, so every character typed into either
 * free-text box was a round trip — `svc-ci-runner` fired thirteen, twelve of
 * them describing a query nobody had finished asking. On the audit log, where
 * a query can scan a large table, that is the page asking the database to do
 * twelve times the work for an answer nobody reads.
 */
describe("AuditLog filter debounce", () => {
  it("does not query on every keystroke", async () => {
    const wrapper = await mountPage();
    const before = auditLogMock.mock.calls.length;

    const box = wrapper.find("#al-user");
    for (const value of ["b", "bo", "bob", "bobb", "bobby"]) {
      await box.setValue(value);
    }
    await flushPromises();
    expect(auditLogMock.mock.calls.length, "mid-typing").toBe(before);

    await new Promise((resolve) => setTimeout(resolve, 350));
    await flushPromises();
    expect(auditLogMock.mock.calls.length - before, "once the typing settles").toBe(1);
    expect(auditLogMock).toHaveBeenLastCalledWith(
      expect.objectContaining({ query: expect.objectContaining({ user_id: "bobby" }) }),
    );
  });

  /** A button press is not typing; it should not wait out the debounce. */
  it("applies Clear filters immediately", async () => {
    const wrapper = await mountPage();
    await typeFilter(wrapper, "#al-user", "bob");
    const before = auditLogMock.mock.calls.length;

    await wrapper
      .findAll("button")
      .find((b) => /clear/i.test(b.text()))!
      .trigger("click");
    await flushPromises();

    expect(auditLogMock.mock.calls.length).toBeGreaterThan(before);
    expect(auditLogMock).toHaveBeenLastCalledWith(
      expect.objectContaining({ query: expect.not.objectContaining({ user_id: "bob" }) }),
    );
  });

  /**
   * The purge dialog states how many events it deletes. It said "1 retention
   * window" — a count of one for an action that can erase thousands.
   */
  it("counts the events a purge would delete before asking to confirm it", async () => {
    auditLogMock.mockResolvedValue({ data: envelope([event()], { total: 7_204 }) });
    const wrapper = await mountPage();
    await wrapper.get("#al-purge-before").setValue("2026-01-01T00:00");
    auditLogMock.mockResolvedValueOnce({ data: envelope([event()], { total: 1_337 }) });

    await wrapper
      .findAll("button")
      .find((b) => b.text().trim() === "Purge")!
      .trigger("click");
    await flushPromises();

    expect(auditLogMock).toHaveBeenLastCalledWith(
      expect.objectContaining({ query: expect.objectContaining({ per_page: 1 }) }),
    );
    const dialog = wrapper.findComponent({ name: "DestructiveConfirm" });
    expect(dialog.props("count")).toBe(1_337);
    expect(dialog.props("open")).toBe(true);
  });
});
