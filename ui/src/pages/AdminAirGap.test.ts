import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { authFetch } = vi.hoisted(() => ({ authFetch: vi.fn() }));
vi.mock("@/composables/useAuthFetch", () => ({
  useAuthFetch: () => ({ authFetch }),
}));

import AdminAirGap from "./AdminAirGap.vue";

function ok(body: unknown) {
  return { ok: true, status: 200, json: async () => body };
}

function respond(bundles: unknown, missing: unknown) {
  authFetch.mockImplementation((url: unknown) =>
    Promise.resolve(String(url).includes("/bundle") ? ok(bundles) : ok(missing)),
  );
}

const BUNDLE = {
  bundle_id: "bundle-1",
  signer_key: "f3a9beef".padEnd(64, "0"),
  imported_at: "2026-09-04T10:00:00Z",
  imported_by: "ops",
  entries: 41,
  blobs: 41,
  rejected: 0,
};

const MISS = {
  registry: "npm-mirror",
  storage_key: "npm-mirror/left-pad/1.3.1/left-pad.tgz",
  kind: "artifact",
  requested_version: "1.3.1",
  held_versions: ["1.3.0"],
  first_seen: "2026-09-04T09:00:00Z",
  last_seen: "2026-09-04T11:00:00Z",
  count: 7,
};

describe("AdminAirGap", () => {
  beforeEach(() => authFetch.mockReset());

  it("shows what came across the gap and what the next bundle needs", async () => {
    respond({ items: [BUNDLE] }, { items: [MISS], total: 1, air_gapped: true });
    const w = mount(AdminAirGap, { global: { stubs: { SectionTabs: true } } });
    await flushPromises();

    const bundles = w.find('[data-testid="air-gap-bundles"]');
    expect(bundles.text()).toContain("bundle-1");
    expect(bundles.text()).toContain("f3a9beef");
    expect(bundles.text()).toContain("41 / 41");
    // The signer is shortened, not printed whole.
    expect(bundles.text()).not.toContain(BUNDLE.signer_key);

    const missing = w.find('[data-testid="air-gap-missing"]');
    expect(missing.text()).toContain("npm-mirror/left-pad/1.3.1/left-pad.tgz");
    expect(missing.text()).toContain("7");
    // RFC 0008-bis §4.4: the version asked for and what was held, side by side.
    expect(w.find('[data-testid="air-gap-requested"]').text()).toBe("1.3.1");
    expect(w.find('[data-testid="air-gap-held"]').text()).toBe("1.3.0");
  });

  /// An empty miss log means two different things, and the page says which.
  it("distinguishes an air-gapped instance from a connected one", async () => {
    respond({ items: [] }, { items: [], total: 0, air_gapped: true });
    const w = mount(AdminAirGap, { global: { stubs: { SectionTabs: true } } });
    await flushPromises();
    expect(w.find('[data-testid="air-gap-state"]').text()).toMatch(/never dials|jamais/i);
    expect(w.find('[data-testid="air-gap-no-misses"]').exists()).toBe(true);

    respond({ items: [] }, { items: [], total: 0, air_gapped: false });
    const w2 = mount(AdminAirGap, { global: { stubs: { SectionTabs: true } } });
    await flushPromises();
    expect(w2.find('[data-testid="air-gap-state"]').text()).toMatch(/not air-gapped|n'est pas/i);
    expect(w2.find('[data-testid="air-gap-no-misses"]').text()).toMatch(
      /only recorded|que lorsque/i,
    );
  });

  it("says an instance with no imports holds only what it was seeded with", async () => {
    respond({ items: [] }, { items: [], total: 0, air_gapped: true });
    const w = mount(AdminAirGap, { global: { stubs: { SectionTabs: true } } });
    await flushPromises();
    expect(w.find('[data-testid="air-gap-no-bundles"]').text()).toMatch(/seeded with|amorcée/i);
  });

  /**
   * An empty miss log on an air-gapped instance means "nobody has asked
   * yet". A registry with nothing cached means "this refuses everything".
   * They look identical on the page unless the second is said out loud.
   */
  it("names a registry that holds nothing at all", async () => {
    respond(
      { items: [] },
      {
        items: [],
        total: 0,
        air_gapped: true,
        empty_registries: ["npm-mirror", "gh"],
      },
    );
    const w = mount(AdminAirGap, { global: { stubs: { SectionTabs: true } } });
    await flushPromises();
    const line = w.find('[data-testid="air-gap-empty-registries"]');
    expect(line.exists()).toBe(true);
    expect(line.text()).toContain("npm-mirror");
    expect(line.text()).toContain("gh");

    // Nothing to say when every registry holds something.
    respond({ items: [] }, { items: [], total: 0, air_gapped: true });
    const w2 = mount(AdminAirGap, { global: { stubs: { SectionTabs: true } } });
    await flushPromises();
    expect(w2.find('[data-testid="air-gap-empty-registries"]').exists()).toBe(false);
  });

  it("offers the miss list as the next plan's input", async () => {
    const clipboard = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText: clipboard } });
    respond({ items: [] }, { items: [MISS], total: 1, air_gapped: true });
    const w = mount(AdminAirGap, { global: { stubs: { SectionTabs: true } } });
    await flushPromises();
    await w.find('[data-testid="air-gap-copy"]').trigger("click");
    await flushPromises();
    expect(clipboard).toHaveBeenCalledWith("npm-mirror\tnpm-mirror/left-pad/1.3.1/left-pad.tgz\t7");
  });
});
