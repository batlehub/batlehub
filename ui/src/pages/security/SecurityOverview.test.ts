import { describe, it, expect, vi, beforeEach } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";

const { overviewMock, verbs } = vi.hoisted(() => ({
  overviewMock: vi.fn(),
  verbs: new Set<string>(),
}));
vi.mock("@/client/sdk.gen", () => ({ securityOverview: overviewMock }));
vi.mock("vue-router", () => ({
  RouterLink: { props: ["to"], template: "<a :data-to='JSON.stringify(to)'><slot/></a>" },
}));
vi.mock("@/composables/useAuth", () => ({ useAuth: () => ({ can: (v: string) => verbs.has(v) }) }));

import SecurityOverview from "./SecurityOverview.vue";

const NOW = new Date().toISOString();
const holds = {
  state: "attention",
  held: 13,
  denied: 9,
  quarantined: 4,
  malware: 4,
  waiting: 2,
  pending_over_hour: 0,
};
const flags = { state: "attention", live: 7, blocking: 3, pulled_after_flag: 2 };
const scanners = {
  state: "ok",
  live_workers: 1,
  uncovered: [],
  queued: 0,
  deferred: 0,
  deferred_over_day: 0,
  failed: 1,
  scanners_attention: 0,
  never_ran: 0,
  breaker_open: false,
};
const coverage = {
  state: "attention",
  registries: 4,
  upstream: 4,
  protected: 3,
  unprotected: ["go-public"],
};

async function mountWith(body: object) {
  overviewMock.mockResolvedValue({ data: body });
  const w = mount(SecurityOverview);
  await flushPromises();
  return w;
}

beforeEach(() => overviewMock.mockReset());

describe("SecurityOverview (RFC 0050 §4.5)", () => {
  it("draws one line per domain, state first, with a sentence", async () => {
    const w = await mountWith({
      computed_at: NOW,
      holds,
      flags,
      scanners,
      coverage,
      decisions: { total: 0, items: [] },
    });
    expect(w.get('[data-testid="status-holds"]').text()).toContain("Attention");
    expect(w.get('[data-testid="status-holds"]').text()).toContain(
      "13 versions held, 4 for malware",
    );
    expect(w.get('[data-testid="status-flags"]').text()).toContain("2 downloaded after the flag");
    expect(w.get('[data-testid="status-scanners"]').text()).toContain("OK");
    expect(w.get('[data-testid="status-coverage"]').text()).toContain("1 of 4 registries");
  });

  it("says workers in the plural past one, and a cve_gate-only estate is not 'unprotected'", async () => {
    const w = await mountWith({
      computed_at: NOW,
      scanners: { ...scanners, live_workers: 3 },
      coverage: { state: "ok", registries: 2, upstream: 2, protected: 0, unprotected: [] },
      decisions: { total: 0, items: [] },
    });
    expect(w.get('[data-testid="status-coverage"]').text()).not.toContain(
      "No registry is protected",
    );
    expect(w.find('[data-testid="unprotected"]').exists()).toBe(false);
    const w2 = await mountWith({
      computed_at: NOW,
      scanners: { ...scanners, live_workers: 3 },
      coverage,
      decisions: { total: 0, items: [] },
    });
    expect(w2.get('[data-testid="status-scanners"]').text()).toContain("3 workers running");
  });

  it("names a SOC's hard blocks and links to every one of them", async () => {
    const w = await mountWith({
      computed_at: NOW,
      holds: { ...holds, held: 1400, soc_verdict: 1400 },
      decisions: { total: 1400, by_kind: { hold_on_hard_block: 1400 }, items: [] },
    });
    expect(w.get('[data-testid="status-holds"]').text()).toContain(
      "1400 versions held, 1400 on a SOC's hard block",
    );
    const all = w.get('[data-testid="all-hold_on_hard_block"]');
    expect(all.text()).toContain("(1400)");
    expect(JSON.parse(all.attributes("data-to")!)).toBe("/security/holds?reason=SOC_VERDICT");
  });

  /** Critique 2026-11-18: by version, each reason opening the page that acts on it. */
  it("groups a version's reasons and routes each to the page that acts on it", async () => {
    verbs.clear();
    verbs.add("audit:read");
    const item = (kind: string) => ({
      kind,
      registry: "npm-public",
      name: "event-stream",
      version: "3.3.6",
      at: NOW,
      pulls_after: 6,
    });
    const w = await mountWith({
      computed_at: NOW,
      holds,
      decisions: {
        total: 2,
        by_kind: { hold_on_hard_block: 1, pulled_after_flag: 1 },
        items: [item("hold_on_hard_block"), item("pulled_after_flag")],
      },
    });
    const list = w.get('[data-testid="decisions"]');
    expect(list.text().match(/event-stream@3\.3\.6/g)).toHaveLength(1);
    expect(list.findAll('[data-testid="decision"]')).toHaveLength(2);
    expect(
      JSON.parse(w.get('[data-testid="act-hold_on_hard_block"]').attributes("data-to")!),
    ).toEqual({
      path: "/security/holds",
      query: { reason: "SOC_VERDICT", registry: "npm-public" },
    });
    expect(
      JSON.parse(w.get('[data-testid="act-pulled_after_flag"]').attributes("data-to")!),
    ).toEqual({
      path: "/security/exposure",
      query: { registry: "npm-public", package: "event-stream", when: "after_flag" },
    });
    // Everything shown: no "→ all N".
    expect(w.find('[data-testid^="all-"]').exists()).toBe(false);

    // Without audit:read, who downloaded it is not theirs to see: the package.
    verbs.clear();
    const w2 = await mountWith({
      computed_at: NOW,
      holds,
      decisions: {
        total: 1,
        by_kind: { pulled_after_flag: 1 },
        items: [item("pulled_after_flag")],
      },
    });
    expect(JSON.parse(w2.get('[data-testid="act-pulled_after_flag"]').attributes("data-to")!)).toBe(
      "/packages/npm-public/event-stream",
    );
  });

  it("has no line for a block the viewer may not read (T1)", async () => {
    const w = await mountWith({ computed_at: NOW, flags, decisions: { total: 0, items: [] } });
    expect(w.find('[data-testid="status-holds"]').exists()).toBe(false);
    expect(w.find('[data-testid="status-flags"]').exists()).toBe(true);
  });

  it("never draws an unknown source as OK", async () => {
    const w = await mountWith({
      computed_at: NOW,
      holds: { ...holds, state: "unknown", unknown_reason: "timeout" },
      flags,
      decisions: { total: 0, items: [] },
    });
    const line = w.get('[data-testid="status-holds"]');
    expect(line.text()).toContain("Unknown");
    expect(line.text()).toContain("Could not be read");
    expect(line.text()).not.toContain("OK");
  });

  it("turns an unprotected instance into a finding, keeping Flags", async () => {
    const w = await mountWith({
      computed_at: NOW,
      holds: { ...holds, state: "ok", held: 0 },
      flags: { ...flags, state: "ok", blocking: 0 },
      coverage: {
        state: "attention",
        registries: 2,
        upstream: 2,
        protected: 0,
        unprotected: ["a", "b"],
      },
      decisions: { total: 0, items: [] },
    });
    expect(w.find('[data-testid="status-holds"]').exists()).toBe(false);
    expect(w.get('[data-testid="status-coverage"]').text()).toContain("No registry is protected");
    expect(w.find('[data-testid="status-flags"]').exists()).toBe(true);
    expect(w.find('[data-testid="unprotected"]').exists()).toBe(true);
  });

  it("lists the decisions with their reason code beside the label", async () => {
    const w = await mountWith({
      computed_at: NOW,
      holds,
      decisions: {
        total: 12,
        items: [
          {
            kind: "pulled_after_flag",
            registry: "npm-public",
            name: "event-stream",
            version: "3.3.6",
            at: NOW,
            reason_code: "SOC_VERDICT",
            pulls_after: 6,
          },
        ],
      },
    });
    const list = w.get('[data-testid="decisions"]');
    expect(list.text()).toContain("event-stream@3.3.6");
    expect(list.text()).toContain("SOC verdict");
    expect(list.text()).toContain("SOC_VERDICT");
    expect(list.text()).toContain("downloaded 6 times after it was flagged");
    expect(w.text()).toContain("1 of 12");
  });

  it("reads an exemption's end forward, never as 'Just now'", async () => {
    const ends = new Date(Date.now() + 5 * 86_400_000 + 60_000).toISOString();
    const w = await mountWith({
      computed_at: NOW,
      holds,
      decisions: {
        total: 1,
        items: [
          {
            kind: "exemption_expiring",
            registry: "npm-public",
            name: "lodash",
            version: "4.17.20",
            at: NOW,
            expires_at: ends,
          },
        ],
      },
    });
    const list = w.get('[data-testid="decisions"]');
    expect(list.text()).toContain("exemption ends in 5d");
    expect(list.text()).not.toContain("Just now");
  });

  it("says the list may be incomplete, naming the sources that did not answer", async () => {
    const w = await mountWith({
      computed_at: NOW,
      holds,
      decisions: { total: 0, items: [], unknown: ["exemptions", "flags"] },
    });
    const note = w.get('[data-testid="decisions-incomplete"]');
    expect(note.text()).toContain("Unknown");
    expect(note.text()).toContain("exemptions, flags");
  });

  it("counts the registries serving upstream, not the local ones, as protected", async () => {
    const w = await mountWith({
      computed_at: NOW,
      holds,
      scanners,
      coverage: { state: "ok", registries: 5, upstream: 3, protected: 3, unprotected: [] },
      decisions: { total: 0, items: [] },
    });
    expect(w.get('[data-testid="status-coverage"]').text()).toContain(
      "All 3 registries are protected",
    );
  });
});
