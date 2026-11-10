/**
 * RFC 0015 §4.8's page.
 *
 * The assertions here are about the three things the page exists to make
 * unmissable rather than about markup: a shadow that is currently serving what
 * the model refuses, a `deny` that is not what the request receives, and the
 * difference between "no shadow" and "a quiet shadow" — which look identical in
 * an empty list and mean opposite things.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const mocks = vi.hoisted(() => ({
  adminAuthzExplain: vi.fn(),
  authzShadow: vi.fn(),
  listExemptions: vi.fn(),
  auditLog: vi.fn(),
  listRegistries: vi.fn(),
  explorePackages: vi.fn(),
  explorePackageDetail: vi.fn(),
}));
vi.mock("@/client/sdk.gen", () => mocks);

let mockRoute: { query: Record<string, unknown> } = { query: {} };
vi.mock("vue-router", () => ({
  useRoute: () => mockRoute,
  RouterLink: { template: "<a><slot/></a>" },
}));

import AdminAuthorization from "./AdminAuthorization.vue";
import AuthzExemptionsPanel from "@/components/admin/authorization/AuthzExemptionsPanel.vue";

const noShadow = {
  data: { by_node: [], recent: [], kept: 500, no_shadow_configured: true },
};

const shadowServing = {
  data: {
    by_node: [
      {
        node: "namespace:@acme",
        shadow_until: "2099-12-01",
        count: 42,
        actions: ["releases:read"],
        subjects: ["role:user"],
        last_seen: "2026-08-29T00:00:00Z",
      },
    ],
    recent: [],
    kept: 500,
    no_shadow_configured: false,
  },
};

function explainAnswer(over: Record<string, unknown> = {}) {
  return {
    data: {
      decision: "deny",
      reason: "no grant for 'releases:read' on registry 'npm1'",
      resolved: [],
      tiers_walked: ["registry:npm1"],
      not_covered: ["per-package visibility (public/internal/team)"],
      attributes: {
        visibility: "public",
        prerelease_visibility: "public",
        immutable: "never",
        monotonic: false,
        versioning_dry_run: false,
        exempt_gates: [],
      },
      ...over,
    },
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  mockRoute = { query: {} };
  mocks.authzShadow.mockResolvedValue(noShadow);
  mocks.listExemptions.mockResolvedValue({ data: [] });
  mocks.auditLog.mockResolvedValue({ data: [] });
  mocks.listRegistries.mockResolvedValue({ data: [] });
  mocks.explorePackages.mockResolvedValue({ data: { packages: [] } });
  mocks.explorePackageDetail.mockResolvedValue({ data: { versions: [] } });
});

async function mountPage() {
  const wrapper = mount(AdminAuthorization, { global: { stubs: { SectionTabs: true } } });
  await flushPromises();
  return wrapper;
}

describe("the authorization page", () => {
  /**
   * The three standing-risk panels load without being asked.
   *
   * An operator who has to press a button to discover a shadow is serving
   * everything has a page that does not do its job.
   */
  it("loads shadow, exemptions and denials on mount", async () => {
    await mountPage();
    expect(mocks.authzShadow).toHaveBeenCalled();
    expect(mocks.auditLog).toHaveBeenCalled();
  });

  /**
   * "No shadow" and "a quiet shadow" say different things.
   *
   * They look identical in an empty list and mean opposite things: the first
   * says enforcing is safe, the second says nothing was measured.
   */
  it("distinguishes no shadow from a quiet one", async () => {
    const none = await mountPage();
    expect(none.text()).toContain("No node is in shadow");

    mocks.authzShadow.mockResolvedValue({
      data: { by_node: [], recent: [], kept: 500, no_shadow_configured: false },
    });
    const quiet = await mountPage();
    expect(quiet.text()).toContain("has served nothing yet");
    expect(quiet.text()).not.toContain("No node is in shadow");
  });

  /** A shadow that is serving is shown with its node, expiry and count. */
  it("shows what a shadow is serving", async () => {
    mocks.authzShadow.mockResolvedValue(shadowServing);
    const w = await mountPage();
    const panel = w.get('[data-testid="panel-shadow"]').text();
    expect(panel).toContain("namespace:@acme");
    expect(panel).toContain("2099-12-01");
    expect(panel).toContain("42");
    expect(panel).toContain("were SERVED");
  });

  /**
   * **The assertion that matters most.** A `deny` under an active shadow is not
   * what the request receives.
   *
   * An operator reading a bare `DENY` would conclude the coordinate is closed
   * while every request to it succeeds — the exact misreading a shadow makes
   * possible, and §11.6's *"a diagnostic that can disagree with reality is worse
   * than none, because it is trusted."*
   */
  it("says when a denial is being shadowed", async () => {
    mocks.adminAuthzExplain.mockResolvedValue(
      explainAnswer({ shadowed_by: { node: "registry:npm1", until: "2099-12-01" } }),
    );
    const w = await mountPage();
    (w.vm as unknown as { registry: string }).registry = "npm1";
    await w.get("form").trigger("submit");
    await flushPromises();

    expect(w.find('[data-testid="explain-shadow-note"]').exists()).toBe(true);
    const note = w.get('[data-testid="explain-shadow-note"]').text();
    expect(note).toContain("registry:npm1");
    expect(note).toContain("2099-12-01");
  });

  /** …and without one the note is absent, so its presence always means something. */
  it("omits the shadow note when there is none", async () => {
    mocks.adminAuthzExplain.mockResolvedValue(explainAnswer());
    const w = await mountPage();
    (w.vm as unknown as { registry: string }).registry = "npm1";
    await w.get("form").trigger("submit");
    await flushPromises();

    expect(w.get('[data-testid="explain-result"]').text()).toContain("DENY");
    expect(w.find('[data-testid="explain-shadow-note"]').exists()).toBe(false);
  });

  /**
   * `granted_by` is a column, not a detail.
   *
   * §4.8: a resolved set without provenance tells an operator what they have;
   * naming the tier tells them which line to edit.
   */
  it("shows where each verb came from", async () => {
    mocks.adminAuthzExplain.mockResolvedValue(
      explainAnswer({
        decision: "allow",
        reason: null,
        resolved: [
          { action: "releases:read", granted_by: "namespace:@acme", subject: "group:*:qa" },
        ],
      }),
    );
    const w = await mountPage();
    (w.vm as unknown as { registry: string }).registry = "npm1";
    await w.get("form").trigger("submit");
    await flushPromises();

    const result = w.get('[data-testid="explain-result"]').text();
    expect(result).toContain("ALLOW");
    expect(result).toContain("namespace:@acme");
    expect(result).toContain("group:*:qa");
  });

  /** The `self_approved` filter §4.8 asks the exemptions panel for by name. */
  it("filters exemptions to the self-approved ones", async () => {
    mocks.listExemptions.mockResolvedValue({
      data: [
        {
          package: "lodash",
          version: "1.0.0",
          gate: "cve_gate",
          exempt_until: "2099-01-01T00:00:00Z",
          reason: "reviewed by security",
          granted_by: "sec",
          self_approved: false,
          expired: false,
        },
        {
          package: "left-pad",
          version: "2.0.0",
          gate: "license_gate",
          exempt_until: "2099-01-01T00:00:00Z",
          reason: "own build",
          granted_by: "bob",
          self_approved: true,
          expired: false,
        },
      ],
    });
    mockRoute = { query: { registry: "npm1" } };
    const w = await mountPage();

    let panel = w.get('[data-testid="panel-exemptions"]').text();
    expect(panel).toContain("lodash");
    expect(panel).toContain("left-pad");

    (
      w.findComponent(AuthzExemptionsPanel).vm as unknown as { selfApprovedOnly: boolean }
    ).selfApprovedOnly = true;
    await flushPromises();
    panel = w.get('[data-testid="panel-exemptions"]').text();
    expect(panel).not.toContain("lodash");
    expect(panel).toContain("left-pad");
  });

  /**
   * A panel that failed and a panel that is empty are different answers.
   *
   * Each of the three standing-risk panels loads on its own, so one of them can
   * fail while the others populate. On this page the empty copy reads as an
   * all-clear — "No node is in shadow", "no exemption" — so a panel that could
   * not load and says nothing is the one misreading §4.8 exists to prevent.
   */
  it("shows the API's message when a panel fails to load", async () => {
    mocks.authzShadow.mockResolvedValue({ error: { message: "shadow report unavailable" } });
    mocks.listExemptions.mockResolvedValue({ error: { message: "exemptions unavailable" } });
    mocks.auditLog.mockResolvedValue({ error: "audit log unavailable" });
    mockRoute = { query: { registry: "npm1" } };
    const w = await mountPage();

    const shadow = w.get('[data-testid="panel-shadow"]').text();
    expect(shadow).toContain("shadow report unavailable");
    expect(shadow).not.toContain("No node is in shadow");
    expect(w.get('[data-testid="panel-exemptions"]').text()).toContain("exemptions unavailable");
    expect(w.get('[data-testid="panel-denials"]').text()).toContain("audit log unavailable");
  });

  /** …and the same when the request never returns an answer at all. */
  it("reports a thrown request as the panel's own error", async () => {
    mocks.authzShadow.mockRejectedValue(new Error("shadow: network down"));
    mocks.listExemptions.mockRejectedValue(new Error("exemptions: network down"));
    // Not an `Error`: the catch has to stringify whatever it caught rather than
    // read `.message` off it and render "undefined".
    mocks.auditLog.mockRejectedValue("audit: connection reset");
    mockRoute = { query: { registry: "npm1" } };
    const w = await mountPage();

    expect(w.get('[data-testid="panel-shadow"]').text()).toContain("shadow: network down");
    expect(w.get('[data-testid="panel-exemptions"]').text()).toContain("exemptions: network down");
    expect(w.get('[data-testid="panel-denials"]').text()).toContain("audit: connection reset");
  });

  /**
   * The whole coordinate travels in the query string.
   *
   * The page is linked to from a ticket and from the package detail page, and a
   * link that restores four of the five fields lands the operator on a question
   * that is not the one they were sent to ask.
   */
  it("takes the whole coordinate from the query string", async () => {
    mocks.adminAuthzExplain.mockResolvedValue(explainAnswer());
    mockRoute = {
      query: {
        registry: "npm1",
        package: "lodash",
        version: "1.0.0",
        subject: "role:admin",
        action: "releases:publish",
      },
    };
    const w = await mountPage();
    await w.get("form").trigger("submit");
    await flushPromises();

    expect(mocks.adminAuthzExplain).toHaveBeenCalledWith({
      query: {
        registry: "npm1",
        subject: "role:admin",
        action: "releases:publish",
        package: "lodash",
        version: "1.0.0",
      },
    });
  });

  /** A failed explain is not a verdict — the result block has to go. */
  it("shows the explain error instead of a verdict", async () => {
    mocks.adminAuthzExplain.mockResolvedValueOnce(explainAnswer());
    const w = await mountPage();
    (w.vm as unknown as { registry: string }).registry = "npm1";
    await w.get("form").trigger("submit");
    await flushPromises();
    expect(w.find('[data-testid="explain-result"]').exists()).toBe(true);

    mocks.adminAuthzExplain.mockResolvedValueOnce({ error: { error: "unknown action" } });
    await w.get("form").trigger("submit");
    await flushPromises();

    expect(w.get('[data-testid="panel-explain"]').text()).toContain("unknown action");
    expect(w.find('[data-testid="explain-result"]').exists()).toBe(false);
  });

  /** …including when the call throws rather than answering. */
  it("shows a thrown explain as its error", async () => {
    mocks.adminAuthzExplain.mockRejectedValue(new Error("explain: gateway timeout"));
    const w = await mountPage();
    (w.vm as unknown as { registry: string }).registry = "npm1";
    await w.get("form").trigger("submit");
    await flushPromises();

    expect(w.get('[data-testid="panel-explain"]').text()).toContain("explain: gateway timeout");
    expect(w.find('[data-testid="explain-result"]').exists()).toBe(false);
  });

  /**
   * The audit endpoint answers with an envelope, and the denials panel reads
   * either shape.
   *
   * A bare array is what the in-memory adapter returns and `{ events: [...] }`
   * is what the paginated one does; reading only the first renders an empty
   * "no denials" panel against a server that is refusing requests.
   */
  it("reads denials out of the events envelope", async () => {
    mocks.auditLog.mockResolvedValue({
      data: {
        events: [
          {
            timestamp: "2026-08-29T10:00:00Z",
            user_id: "alice",
            package_id: "npm1/lodash@1.0.0",
            action: "releases:read",
            reason: "no grant",
          },
          // Every optional column absent: the row still has to render, with a
          // placeholder rather than "undefined".
          { timestamp: "2026-08-29T11:00:00Z" },
        ],
      },
    });
    const w = await mountPage();

    const panel = w.get('[data-testid="panel-denials"]').text();
    expect(panel).toContain("alice");
    expect(panel).toContain("npm1/lodash@1.0.0");
    expect(panel).toContain("no grant");
    expect(panel).not.toContain("undefined");
  });

  /** All five panels §4.8 names are on the page. */
  it("renders every panel", async () => {
    const w = await mountPage();
    for (const panel of ["shadow", "exemptions", "explain", "denials", "retention"]) {
      expect(w.find(`[data-testid="panel-${panel}"]`).exists()).toBe(true);
    }
  });
});
