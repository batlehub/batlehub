import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const authFetch = vi.fn();
vi.mock("@/composables/useAuthFetch", () => ({
  useAuthFetch: () => ({ authFetch }),
}));

import MovingRefsPanel from "./MovingRefsPanel.vue";

function ok(body: unknown) {
  return { ok: true, json: async () => body };
}

function panel() {
  return mount(MovingRefsPanel, { props: { registry: "gh", name: "cli/cli" } });
}

describe("MovingRefsPanel", () => {
  beforeEach(() => authFetch.mockReset());

  it("lists what the instance resolved, with short SHAs and what moved", async () => {
    authFetch.mockResolvedValue(
      ok({
        registry: "gh",
        package: "cli/cli",
        remembered: true,
        refs: [
          {
            git_ref: "main",
            ref_kind: "branch",
            sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            resolved_at: "2026-09-04T10:00:00Z",
            previous_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          },
          {
            git_ref: "v2.60.0",
            ref_kind: "tag",
            sha: "cccccccccccccccccccccccccccccccccccccccc",
            resolved_at: "2026-09-03T10:00:00Z",
          },
        ],
      }),
    );
    const w = panel();
    await flushPromises();
    const rows = w.find('[data-testid="moving-refs-rows"]');
    expect(rows.exists()).toBe(true);
    expect(rows.text()).toContain("main");
    expect(rows.text()).toContain("aaaaaaa");
    expect(rows.text()).not.toContain("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    expect(rows.text()).toContain("bbbbbbb");
    expect(rows.text()).toContain("v2.60.0");
  });

  it("renders nothing on a registry that is not a forge", async () => {
    authFetch.mockResolvedValue({ ok: false, json: async () => ({}) });
    const w = panel();
    await flushPromises();
    expect(w.find('[data-testid="moving-refs-panel"]').exists()).toBe(false);
  });

  it("says when this deployment remembers nothing rather than showing an empty list", async () => {
    authFetch.mockResolvedValue(
      ok({ registry: "gh", package: "cli/cli", remembered: false, refs: [] }),
    );
    const w = panel();
    await flushPromises();
    expect(w.find('[data-testid="moving-refs-none"]').exists()).toBe(true);
  });
});
