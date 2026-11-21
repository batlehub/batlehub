import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { myQuotaMock, myLimitsMock } = vi.hoisted(() => ({
  myQuotaMock: vi.fn(),
  myLimitsMock: vi.fn(),
}));
vi.mock("@/client/sdk.gen", () => ({ myQuota: myQuotaMock, myLimits: myLimitsMock }));

import LimitsWidget from "./LimitsWidget.vue";

const quotaRow = (over: Record<string, unknown> = {}) => ({
  registry: "npm-local",
  bytes_used: 100,
  bytes_limit: 1000,
  packages_used: 1,
  packages_limit: 10,
  warn_threshold_pct: 80,
  state: "ok",
  bytes_state: "ok",
  packages_state: "ok",
  ...over,
});

const limitRow = (over: Record<string, unknown> = {}) => ({
  registry: "npm-public",
  scope: "user",
  group: null,
  used: 30,
  limit: 600,
  window_secs: 60,
  resets_at: new Date(Date.now() + 42_000).toISOString(),
  enforcement: "block",
  state: "ok",
  ...over,
});

async function mountWidget() {
  const wrapper = mount(LimitsWidget);
  await flushPromises();
  return wrapper;
}

const rings = (w: Awaited<ReturnType<typeof mountWidget>>, group: string) =>
  w.get(`[data-testid="limits-${group}"]`).findAll('[role="meter"]');

describe("LimitsWidget", () => {
  beforeEach(() => {
    myQuotaMock.mockReset().mockResolvedValue({ data: [quotaRow()] });
    myLimitsMock.mockReset().mockResolvedValue({ data: [limitRow()] });
  });

  it("draws downloads before publishing, one ring per registry", async () => {
    const wrapper = await mountWidget();
    const html = wrapper.html();
    expect(html.indexOf('data-testid="limits-downloads"')).toBeLessThan(
      html.indexOf('data-testid="limits-publishing"'),
    );
    expect(rings(wrapper, "downloads")).toHaveLength(1);
    expect(rings(wrapper, "publishing")).toHaveLength(1);
    const ring = rings(wrapper, "downloads")[0];
    expect(ring.attributes("aria-valuenow")).toBe("30");
    expect(ring.attributes("aria-valuemax")).toBe("600");
    expect(ring.attributes("aria-valuetext")).toBe("30 of 600 requests /min");
  });

  /** The window is the registry's own: every row says which. */
  it("writes each counter's window on its row", async () => {
    myLimitsMock.mockResolvedValue({
      data: [limitRow(), limitRow({ registry: "crates-io", limit: 5000, window_secs: 3600 })],
    });
    const wrapper = await mountWidget();
    const text = wrapper.get('[data-testid="limits-downloads"]').text();
    expect(text).toContain("30/600 /min");
    expect(text).toContain("30/5,000 /h");
  });

  it("names a group counter, which is not the caller's alone", async () => {
    myLimitsMock.mockResolvedValue({ data: [limitRow({ scope: "group", group: "ci" })] });
    const wrapper = await mountWidget();
    expect(wrapper.get('[data-testid="limits-downloads"]').text()).toContain("npm-public · ci");
  });

  it("says nothing extra while every limit is well clear", async () => {
    const wrapper = await mountWidget();
    expect(wrapper.get('[data-testid="limits-verdict"]').text()).toBe("Every limit is well clear.");
    expect(wrapper.find('[data-testid="limits-advice"]').exists()).toBe(false);
  });

  /** State is the server's, carried by glyph and word as well as hue. */
  it("carries a near and an at-limit state through with a word and advice", async () => {
    myLimitsMock.mockResolvedValue({ data: [limitRow({ used: 540, state: "warning" })] });
    myQuotaMock.mockResolvedValue({
      data: [quotaRow({ packages_used: 10, packages_state: "at_limit", state: "at_limit" })],
    });
    const wrapper = await mountWidget();
    expect(wrapper.get('[data-testid="limits-verdict"]').text()).toBe("2 limits need attention.");
    expect(rings(wrapper, "downloads")[0].attributes("data-state")).toBe("warning");
    expect(rings(wrapper, "publishing")[0].attributes("data-state")).toBe("at-limit");
    const advice = wrapper.get('[data-testid="limits-advice"]').text();
    expect(advice).toContain("Near limit");
    expect(advice).toContain("At limit");
    expect(advice).toMatch(/refused/);
    expect(advice).toMatch(/resets in \d+ s/);
  });

  /**
   * One ring per registry, for the dimension closer to its limit, with that
   * dimension's own state — versions past the mark while storage is fine is a
   * warning about versions, not about the registry's storage.
   */
  it("draws the tighter dimension with its own state and names it", async () => {
    myQuotaMock.mockResolvedValue({
      data: [quotaRow({ packages_used: 9, packages_state: "warning", state: "warning" })],
    });
    const wrapper = await mountWidget();
    const ring = rings(wrapper, "publishing")[0];
    expect(ring.attributes("data-state")).toBe("warning");
    expect(ring.attributes("aria-valuetext")).toBe("9/10 versions");

    myQuotaMock.mockResolvedValue({
      data: [quotaRow({ packages_limit: null, packages_state: null, bytes_used: 500 })],
    });
    const storage = await mountWidget();
    expect(rings(storage, "publishing")[0].attributes("aria-valuetext")).toContain("storage");
  });

  it("leaves out a group with nothing to measure", async () => {
    myQuotaMock.mockResolvedValue({ data: [] });
    const wrapper = await mountWidget();
    expect(wrapper.find('[data-testid="limits-publishing"]').exists()).toBe(false);
    expect(wrapper.find('[data-testid="limits-downloads"]').exists()).toBe(true);
  });

  it("renders nothing at all when no registry has a limit", async () => {
    myQuotaMock.mockResolvedValue({ data: [] });
    myLimitsMock.mockResolvedValue({ data: [] });
    const wrapper = await mountWidget();
    expect(wrapper.find("section").exists()).toBe(false);
    expect(wrapper.text()).toBe("");
  });

  /** One failed source leaves the other standing, and says so in its own group. */
  it("says a group could not be read while the other still renders", async () => {
    myLimitsMock.mockResolvedValue({ error: { message: "store down" } });
    const wrapper = await mountWidget();
    expect(wrapper.get('[data-testid="limits-downloads"]').text()).toContain("Could not");
    expect(rings(wrapper, "publishing")).toHaveLength(1);
  });

  it("surfaces an error when nothing could be read", async () => {
    myQuotaMock.mockResolvedValue({ error: { message: "boom" } });
    myLimitsMock.mockResolvedValue({ error: { message: "boom" } });
    const wrapper = await mountWidget();
    expect(wrapper.text()).toContain("boom");
    expect(wrapper.find('[role="meter"]').exists()).toBe(false);
  });
});
