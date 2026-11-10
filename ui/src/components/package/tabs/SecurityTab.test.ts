import { describe, it, expect, vi } from "vitest";
import { mount } from "@vue/test-utils";

vi.mock("@/client/sdk.gen", () => ({
  getVerdict: vi.fn().mockResolvedValue({ error: { status: 404 } }),
  rescanVerdict: vi.fn(),
}));

import type { ScanStatusDto } from "@/client/types.gen";
import SecurityTab from "./SecurityTab.vue";

function tab(
  over: {
    status?: ScanStatusDto | null;
    unavailable?: boolean;
    isAdmin?: boolean;
    pollStopped?: "timeout" | "failed" | null;
  } = {},
) {
  return mount(SecurityTab, {
    props: {
      registry: "npm-q",
      name: "left-pad",
      version: "1.3.0",
      unavailable: false,
      isAdmin: false,
      pollStopped: null,
      ...over,
    },
  });
}

describe("SecurityTab (RFC 0042 §4.5)", () => {
  it("tells an admin the registry scans nothing when no status came back", () => {
    expect(tab({ isAdmin: true }).find('[data-testid="no-scanning"]').exists()).toBe(true);
    // A reader without the verb cannot tell that from being withheld: nothing.
    const reader = tab();
    expect(reader.find('[data-testid="no-scanning"]').exists()).toBe(false);
    expect(reader.find('[data-testid="scan-line"]').exists()).toBe(false);
  });

  it("says it updates while scanning, with the attempt", () => {
    const w = tab({ status: { state: "scanning", detail: { attempts: 1 } } });
    expect(w.text()).toContain("attempt 1");
    expect(w.text()).toContain("Updates automatically.");
    expect(w.find('[aria-live="polite"]').exists()).toBe(true);
  });

  it.each([
    ["timeout", "Stopped updating after 10 minutes."],
    ["failed", "Stopped updating: the server did not answer."],
  ] as const)("offers Refresh when the poll stopped (%s)", async (why, text) => {
    const w = tab({ status: { state: "queued" }, pollStopped: why });
    expect(w.text()).toContain(text);
    await w.get('[data-testid="poll-refresh"]').trigger("click");
    expect(w.emitted("refresh")).toHaveLength(1);
  });

  it("draws — when the batch failed, and no poll line on a settled state", () => {
    expect(tab({ unavailable: true }).get('[data-testid="scan-badge"]').text()).toBe("—");
    const done = tab({ status: { state: "scanned", verdict: "allowed" } });
    expect(done.text()).not.toContain("Updates automatically.");
  });
});
