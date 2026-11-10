import { describe, it, expect } from "vitest";
import { mount } from "@vue/test-utils";

import type { ScanStatusDto } from "@/client/types.gen";
import ScanBadge from "./ScanBadge.vue";

function badge(props: { status?: ScanStatusDto | null; unavailable?: boolean; inList?: boolean }) {
  return mount(ScanBadge, { props });
}

describe("ScanBadge (RFC 0042 §4.5)", () => {
  it("draws nothing without a status: no [security], or no quarantine:read", () => {
    expect(badge({}).find('[data-testid="scan-badge"]').exists()).toBe(false);
    expect(badge({ status: null }).html()).not.toContain("scan-badge");
  });

  it("draws — with its tooltip when the batch failed", () => {
    const b = badge({ unavailable: true }).get('[data-testid="scan-badge"]');
    expect(b.text()).toBe("—");
    expect(b.attributes("title")).toContain("could not be read");
  });

  it.each([
    ["not_scanned", "not scanned"],
    ["queued", "queued"],
    ["scanning", "scanning"],
    ["failed", "scan failed"],
  ] as const)("labels %s in text", (state, label) => {
    const b = badge({ status: { state } }).get('[data-testid="scan-badge"]');
    expect(b.text()).toBe(label);
    expect(b.find("svg").attributes("aria-hidden")).toBe("true");
  });

  it.each([
    ["allowed", "border-secondary"],
    ["warned", "border-border"],
    ["quarantined", "border-copper"],
    ["denied", "border-destructive"],
  ] as const)("uses VerdictPanel's variant for scanned · %s", (verdict, cls) => {
    const b = badge({ status: { state: "scanned", verdict } }).get('[data-testid="scan-badge"]');
    expect(b.text()).toBe(`scanned · ${verdict}`);
    expect(b.classes()).toContain(cls);
  });

  it("says 'as last recorded' in a list", () => {
    const b = badge({ status: { state: "scanned", verdict: "allowed" }, inList: true });
    expect(b.get('[data-testid="scan-badge"]').attributes("title")).toBe("as last recorded");
  });

  it("carries the attempt and the error with findings:read", () => {
    const scanning = badge({ status: { state: "scanning", detail: { attempts: 1 } } });
    expect(scanning.get('[data-testid="scan-badge"]').attributes("aria-label")).toContain(
      "attempt 1",
    );
    const failed = badge({
      status: { state: "failed", detail: { attempts: 3, last_error: "tar: unexpected EOF" } },
    }).get('[data-testid="scan-badge"]');
    expect(failed.classes()).toContain("border-destructive");
    expect(failed.attributes("title")).toBe("3 attempts; last error: tar: unexpected EOF");
    const silent = badge({ status: { state: "failed", detail: { attempts: 3 } } });
    expect(silent.get('[data-testid="scan-badge"]').attributes("title")).toBe(
      "3 attempts; no error recorded",
    );
  });
});
