import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const listFlags = vi.fn();
vi.mock("@/client/sdk.gen", () => ({
  listFlags: (...args: unknown[]) => listFlags(...args),
}));

import FlagsPanel from "./FlagsPanel.vue";

function flag(overrides: Record<string, unknown> = {}) {
  return {
    id: "22222222-2222-2222-2222-222222222222",
    source: "soc",
    external_id: "CASE-7",
    registry: "npm",
    package_name: "left-pad",
    version: "*",
    kind: "malware",
    effect: "hard_block",
    summary: "steals tokens",
    first_seen: "2026-09-04T10:00:00Z",
    updated_at: "2026-09-04T10:00:00Z",
    ...overrides,
  };
}

describe("FlagsPanel", () => {
  beforeEach(() => listFlags.mockReset());

  it("lists the flags with their state", async () => {
    listFlags.mockResolvedValue({
      data: {
        items: [flag(), flag({ id: "3", external_id: "OLD", revoked_at: "2026-09-04T12:00:00Z" })],
        total: 2,
        page: 0,
        per_page: 25,
      },
      error: undefined,
    });
    const w = mount(FlagsPanel);
    await flushPromises();
    const rows = w.find('[data-testid="flags-rows"]');
    expect(rows.text()).toContain("soc:CASE-7");
    expect(rows.text()).toContain("npm/left-pad@*");
    expect(rows.text()).toContain("hard_block");
    expect(rows.text()).toMatch(/revoked|révoqué/i);
  });

  it("re-queries with tombstones when asked", async () => {
    listFlags.mockResolvedValue({
      data: { items: [], total: 0, page: 0, per_page: 25 },
      error: undefined,
    });
    const w = mount(FlagsPanel);
    await flushPromises();
    expect(w.find('[data-testid="flags-empty"]').exists()).toBe(true);
    await w.find('[data-testid="flags-include-dead"]').trigger("click");
    await flushPromises();
    expect(listFlags).toHaveBeenLastCalledWith(
      expect.objectContaining({ query: expect.objectContaining({ include_dead: true }) }),
    );
  });

  it("names the missing permission on a 403", async () => {
    listFlags.mockResolvedValue({
      data: undefined,
      error: { error: "Forbidden" },
      response: { status: 403 },
    });
    const w = mount(FlagsPanel);
    await flushPromises();
    expect(w.find('[data-testid="flags-error"]').text()).toContain("flags:read");
  });
});
