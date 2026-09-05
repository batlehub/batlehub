import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { authFetchMock } = vi.hoisted(() => ({ authFetchMock: vi.fn() }));
vi.mock("@/composables/useAuthFetch", () => ({
  useAuthFetch: () => ({ authFetch: authFetchMock }),
}));

import AdminUpstream from "./AdminUpstream.vue";

type Row = {
  registry: string;
  package_name: string;
  version?: string | null;
  state: "missing" | "disappeared";
  first_missed_at: string;
  last_checked_at: string;
  confirmed_at?: string | null;
  consecutive_misses: number;
  last_error?: string | null;
};

const row = (over: Partial<Row>): Row => ({
  registry: "npm",
  package_name: "left-pad",
  version: "1.3.1",
  state: "missing",
  first_missed_at: "2026-09-04T04:00:00Z",
  last_checked_at: "2026-09-04T10:00:00Z",
  confirmed_at: null,
  consecutive_misses: 1,
  last_error: null,
  ...over,
});

function page(items: Row[], policy: "audit" | "block" = "audit") {
  return {
    items,
    total: items.length,
    page: 0,
    per_page: 20,
    policy,
    registries: ["npm"],
    counts: [
      {
        registry: "npm",
        missing: items.filter((r) => r.state === "missing").length,
        disappeared: items.filter((r) => r.state === "disappeared").length,
      },
    ],
  };
}

function serve(listing: unknown, recheck?: unknown, status = 200) {
  authFetchMock.mockReset();
  authFetchMock.mockImplementation((_url: string, init?: RequestInit) =>
    Promise.resolve({
      ok: status < 400,
      status,
      json: async () => (init?.method === "POST" ? recheck : listing),
    }),
  );
}

async function mountPage() {
  const wrapper = mount(AdminUpstream, { global: { stubs: { SectionTabs: true } } });
  await flushPromises();
  return wrapper;
}

/**
 * The page's question: "what has left its upstream, and what will this
 * instance do about it?" (RFC 0014 §4.6). Every state it can be in renders
 * as itself, and the first-sweep empty state says why it is empty.
 */
describe("AdminUpstream", () => {
  beforeEach(() => serve(page([])));

  it("says the audit is not running rather than showing an empty table", async () => {
    serve(undefined, undefined, 503);
    const w = await mountPage();
    expect(w.find('[data-testid="upstream-not-running"]').exists()).toBe(true);
    expect(w.find('[data-testid="upstream-policy"]').exists()).toBe(false);
  });

  it("renders the first-sweep empty state with the policy above it", async () => {
    const w = await mountPage();
    expect(w.find('[data-testid="upstream-policy"]').text()).toContain("audit");
    expect(w.find('[data-testid="upstream-empty"]').exists()).toBe(true);
    expect(w.find('[data-testid="upstream-empty"]').text()).toMatch(/first sweep/i);
    expect(w.findAll('[data-testid="upstream-row"]')).toHaveLength(0);
  });

  it("renders a missing row and a confirmed row as themselves, with the counts", async () => {
    serve(
      page(
        [
          row({ package_name: "wobbly", consecutive_misses: 1 }),
          row({
            package_name: "gone",
            version: null,
            state: "disappeared",
            consecutive_misses: 3,
            confirmed_at: "2026-09-04T10:00:00Z",
            last_error: "404 from upstream",
          }),
        ],
        "block",
      ),
    );
    const w = await mountPage();
    const rows = w.findAll('[data-testid="upstream-row"]');
    expect(rows).toHaveLength(2);
    const gone = rows.find((r) => r.text().includes("gone"))!;
    expect(gone.text()).toContain("disappeared");
    expect(gone.text()).toContain("every version");
    expect(gone.text()).toContain("404 from upstream");
    const wobbly = rows.find((r) => r.text().includes("wobbly"))!;
    expect(wobbly.text()).toContain("missing");
    expect(wobbly.text()).toContain("@1.3.1");
    expect(w.find('[data-testid="upstream-policy"]').text()).toContain("block");
    expect(w.find('[data-testid="upstream-count"]').text()).toContain("1 missing");
    expect(w.find('[data-testid="upstream-count"]').text()).toContain("1 disappeared");
  });

  it("filters are sent to the API", async () => {
    const w = await mountPage();
    await w.find('[data-testid="upstream-state-filter"]').setValue("disappeared");
    await flushPromises();
    const urls = authFetchMock.mock.calls.map((c) => String(c[0]));
    expect(urls.some((u) => u.includes("state=disappeared"))).toBe(true);
    expect(urls.some((u) => u.includes("page=0"))).toBe(true);
  });

  it("recheck posts the coordinate and reports the probe's outcome", async () => {
    serve(page([row({ package_name: "gone", state: "disappeared", consecutive_misses: 3 })]), {
      probed: 1,
      missing: 0,
      inconclusive: 0,
      transitions: ["reappeared"],
      status: null,
    });
    const w = await mountPage();
    await w.find('[data-testid="upstream-recheck"]').trigger("click");
    await flushPromises();
    const post = authFetchMock.mock.calls.find((c) => (c[1] as RequestInit)?.method === "POST")!;
    expect(String(post[0])).toContain("/api/v1/admin/upstream/recheck");
    expect(JSON.parse(String((post[1] as RequestInit).body))).toEqual({
      registry: "npm",
      package_name: "gone",
      version: "1.3.1",
    });
    expect(w.text()).toMatch(/reappeared/i);
  });
});
