import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const getVerdict = vi.fn();
const rescanVerdict = vi.fn();
vi.mock("@/client/sdk.gen", () => ({
  getVerdict: (...args: unknown[]) => getVerdict(...args),
  rescanVerdict: (...args: unknown[]) => rescanVerdict(...args),
}));

import VerdictPanel from "./VerdictPanel.vue";

function verdict(overrides: Record<string, unknown> = {}) {
  return {
    package: { registry: "npm-sec", name: "left-pad", version: "1.3.1" },
    state: "quarantined",
    reason_codes: ["MIN_AGE_NOT_MET"],
    findings: [],
    policy_ref: "npm-sec/default",
    available_at: "2026-09-05T14:12:00Z",
    evaluated_at: "2026-09-04T14:12:00Z",
    last_scanned_at: null,
    scanners_done: [],
    findings_withheld: false,
    ...overrides,
  };
}

function panel(canRescan = false) {
  return mount(VerdictPanel, {
    props: { registry: "npm-sec", name: "left-pad", version: "1.3.1", canRescan },
  });
}

describe("VerdictPanel", () => {
  beforeEach(() => {
    getVerdict.mockReset();
    rescanVerdict.mockReset();
  });

  /**
   * A 404 is three things the server does not tell apart — never seen, no
   * security profile, no `quarantine:read` — and the panel says nothing for
   * any of them rather than guess.
   */
  it("says nothing when there is no verdict to read", async () => {
    getVerdict.mockResolvedValue({ data: undefined, error: { error: "Not Found" } });
    const w = panel();
    await flushPromises();
    expect(w.find('[data-testid="verdict-panel"]').exists()).toBe(false);
  });

  it("shows the state, the codes and when a hold lifts", async () => {
    getVerdict.mockResolvedValue({ data: verdict(), error: undefined });
    const w = panel();
    await flushPromises();
    expect(w.find('[data-testid="verdict-state"]').text()).toContain("quarantined");
    expect(w.text()).toContain("MIN_AGE_NOT_MET");
    expect(w.text()).toContain("Held until");
    expect(getVerdict).toHaveBeenCalledWith({
      path: { registry: "npm-sec", name: "left-pad", version: "1.3.1" },
    });
  });

  /** "None" and "not for you" are different facts, and the panel keeps them apart. */
  it("says findings are withheld rather than absent", async () => {
    getVerdict.mockResolvedValue({
      data: verdict({ state: "denied", reason_codes: ["VULNERABILITY"], findings_withheld: true }),
      error: undefined,
    });
    const w = panel();
    await flushPromises();
    expect(w.text()).toContain("withheld");
    expect(w.find('[data-testid="verdict-findings"]').exists()).toBe(false);
  });

  it("lists the findings for a reader who may see them, and rescans for an admin", async () => {
    getVerdict.mockResolvedValue({
      data: verdict({
        state: "denied",
        reason_codes: ["VULNERABILITY"],
        findings: [
          {
            scanner: "osv",
            kind: "vulnerability",
            code: "VULNERABILITY",
            severity: "critical",
            reference: "GHSA-xxxx",
            summary: "remote code execution",
            raw: null,
          },
        ],
      }),
      error: undefined,
    });
    rescanVerdict.mockResolvedValue({
      data: { queued: true, trigger: "rescan" },
      error: undefined,
    });
    const w = panel(true);
    await flushPromises();
    const findings = w.find('[data-testid="verdict-findings"]');
    expect(findings.exists()).toBe(true);
    expect(findings.text()).toContain("GHSA-xxxx");
    await w.find('[data-testid="verdict-rescan"]').trigger("click");
    await flushPromises();
    expect(rescanVerdict).toHaveBeenCalledOnce();
    expect(w.text()).toContain("Rescan queued");
  });

  /**
   * RFC 0041 §4.5: an unrated finding keeps "unknown" in its text — what the
   * scanner said — and is coloured by the level it was judged at.
   */
  it("shows an unrated finding's judged level beside its stored one", async () => {
    getVerdict.mockResolvedValue({
      data: verdict({
        state: "denied",
        reason_codes: ["VULNERABILITY"],
        findings: [
          {
            scanner: "osv",
            kind: "vulnerability",
            code: "VULNERABILITY",
            severity: "unknown",
            judged_as: "high",
            reference: "GO-2024-2687",
            summary: "x/net",
            raw: null,
          },
          {
            scanner: "osv",
            kind: "vulnerability",
            code: "VULNERABILITY",
            severity: "medium",
            reference: "GHSA-yyyy",
            summary: "y",
            raw: null,
          },
        ],
      }),
      error: undefined,
    });
    const w = panel();
    await flushPromises();
    const items = w.findAll('[data-testid="verdict-findings"] li');
    expect(items[0].text()).toContain("Unknown, judged as High by unknown_severity");
    expect(items[1].text()).toContain("Medium");
    expect(items[1].text()).not.toContain("judged");
  });

  it("does not offer a rescan to a reader without the grant", async () => {
    getVerdict.mockResolvedValue({ data: verdict(), error: undefined });
    const w = panel(false);
    await flushPromises();
    expect(w.find('[data-testid="verdict-rescan"]').exists()).toBe(false);
  });
});
