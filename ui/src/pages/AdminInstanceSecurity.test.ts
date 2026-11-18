import { describe, it, expect, vi, beforeEach } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";

const { instanceMock } = vi.hoisted(() => ({ instanceMock: vi.fn() }));
vi.mock("@/client/sdk.gen", () => ({ instanceSecurity: instanceMock }));
vi.mock("vue-router", () => ({ RouterLink: { props: ["to"], template: "<a><slot/></a>" } }));

import AdminInstanceSecurity from "./AdminInstanceSecurity.vue";

const NOW = new Date().toISOString();
const base = {
  window_hours: 24,
  computed_at: NOW,
  sign_ins: { state: "attention", rejected: 214, by_kind: { token: 211, sso: 3 } },
  token_sources: { new_source: 3 },
  static_tokens: { plain: 2, paths: ["auth[0].tokens[1]", "auth[0].tokens[3]"] },
};

async function mountWith(body: object) {
  instanceMock.mockResolvedValue({ data: body });
  const w = mount(AdminInstanceSecurity);
  await flushPromises();
  return w;
}

beforeEach(() => instanceMock.mockReset());

describe("AdminInstanceSecurity (RFC 0050 §4.5)", () => {
  it("without audit:read: counts and config paths, no address and no user id (T3)", async () => {
    const w = await mountWith(base);
    expect(w.get('[data-testid="line-sign-ins"]').text()).toContain("214 rejected credentials");
    expect(w.get('[data-testid="line-static"]').text()).toContain("auth[0].tokens[1]");
    // The blocks a verb gates are absent, not empty.
    expect(w.find('[data-testid="line-bans"]').exists()).toBe(false);
    expect(w.find('[data-testid="line-exemptions"]').exists()).toBe(false);
    expect(w.find('[data-testid="line-warnings"]').exists()).toBe(false);
  });

  it("with audit:read: addresses and the user ids of plain tokens", async () => {
    const w = await mountWith({
      ...base,
      sign_ins: {
        ...base.sign_ins,
        distinct_addresses: 9,
        addresses: [{ ip: "203.0.113.7", count: 200 }],
      },
      static_tokens: { ...base.static_tokens, user_ids: ["ci-bot", "alice-admin"] },
      bans: { active: 2, automatic: 1 },
      warnings: [{ code: "cors.any-origin", message: "…" }],
    });
    expect(w.get('[data-testid="line-sign-ins"]').text()).toContain("from 9 addresses");
    expect(w.get('[data-testid="line-sign-ins"]').text()).toContain("203.0.113.7");
    expect(w.get('[data-testid="line-static"]').text()).toContain("ci-bot");
    // The paths stay beside the user ids: the path is what the operator edits.
    expect(w.get('[data-testid="static-paths"]').text()).toContain("auth[0].tokens[1]");
    expect(w.get('[data-testid="line-bans"]').text()).toContain("2 addresses banned");
    expect(w.get('[data-testid="line-warnings"]').text()).toContain("cors.any-origin");
  });

  /** `[console].accepted`: still listed, no longer attention. */
  it("an accepted warning is named, and alone it leaves the line OK", async () => {
    const w = await mountWith({
      ...base,
      warnings: [
        { code: "proxy-trust.unconfigured", path: "server", message: "…", accepted: true },
      ],
    });
    const line = w.get('[data-testid="line-warnings"]');
    expect(line.find('[data-state="ok"]').exists()).toBe(true);
    expect(line.text()).toContain("0 security warnings");
    expect(w.get('[data-testid="warnings-accepted"]').text()).toContain("proxy-trust.unconfigured");
  });

  it("a store that did not answer is one unknown line, and the page is not quiet", async () => {
    const w = await mountWith({
      ...base,
      sign_ins: { state: "ok", rejected: 0, by_kind: {} },
      token_sources: { new_source: 0 },
      static_tokens: { plain: 0, paths: [] },
      bans: { active: 0, automatic: 0, unknown_reason: "redis is down" },
    });
    expect(w.get('[data-testid="line-bans"]').text()).toContain("Could not be read");
    expect(w.find('[data-testid="quiet"]').exists()).toBe(false);
  });

  it("asks the 7-day window when chosen", async () => {
    const w = await mountWith(base);
    await w
      .findAll("button")
      .find((b) => b.text() === "7 d")!
      .trigger("click");
    await flushPromises();
    expect(instanceMock).toHaveBeenLastCalledWith({ query: { window: "7d" } });
  });

  it("draws unreadable token sources as unknown, and a partial count as at least", async () => {
    const quietBody = {
      ...base,
      sign_ins: { state: "ok", rejected: 0, by_kind: {} },
      static_tokens: { plain: 0, paths: [] },
    };
    const w = await mountWith({
      ...quietBody,
      token_sources: { new_source: 0, unknown_reason: "timeout" },
    });
    expect(w.get('[data-testid="line-sources"]').text()).toContain("Could not be read");
    expect(w.find('[data-testid="quiet"]').exists()).toBe(false);
    const p = await mountWith({ ...quietBody, token_sources: { new_source: 4, partial: true } });
    expect(p.get('[data-testid="line-sources"]').text()).toMatch(/≥\s+4/);
  });

  it("drops a late answer for the window the viewer already left", async () => {
    let answer24h: (v: unknown) => void = () => {};
    instanceMock.mockImplementationOnce(() => new Promise((resolve) => (answer24h = resolve)));
    instanceMock.mockResolvedValue({
      data: { ...base, window_hours: 168, sign_ins: { ...base.sign_ins, rejected: 7 } },
    });
    const w = mount(AdminInstanceSecurity);
    await w
      .findAll("button")
      .find((b) => b.text() === "7 d")!
      .trigger("click");
    await flushPromises();
    answer24h({ data: base });
    await flushPromises();
    expect(w.get('[data-testid="line-sign-ins"]').text()).toContain("7 rejected");
  });
});
