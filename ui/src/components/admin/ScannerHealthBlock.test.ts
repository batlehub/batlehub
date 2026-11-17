import { describe, it, expect, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";

const { scannersMock } = vi.hoisted(() => ({ scannersMock: vi.fn() }));
vi.mock("@/client/sdk.gen", () => ({ securityScanners: scannersMock }));
vi.mock("vue-router", () => ({ RouterLink: { props: ["to"], template: "<a><slot/></a>" } }));

import ScannerHealthBlock from "./ScannerHealthBlock.vue";

const NOW = new Date().toISOString();
const queue = { queued: 0, deferred: 0, deferred_over_day: 0, failed: 1 };

async function mountWith(body: object) {
  scannersMock.mockResolvedValue({ data: body });
  const w = mount(ScannerHealthBlock);
  await flushPromises();
  return w;
}

describe("ScannerHealthBlock (RFC 0050 §4.5)", () => {
  it("lists live workers and what they cover, the queue, each scanner", async () => {
    const w = await mountWith({
      computed_at: NOW,
      workers: [{ worker_id: "worker-7f2c", last_seen: NOW, registries: [], live: true }],
      queue,
      scanners: [
        {
          name: "osv",
          state: "ok",
          last_ok_at: NOW,
          last_error_at: NOW,
          last_error_class: "http_503",
        },
        { name: "yara", state: "never_ran" },
      ],
    });
    expect(w.get('[data-testid="workers"]').text()).toContain("worker-7f2c");
    expect(w.get('[data-testid="workers"]').text()).toContain("covers every registry");
    expect(w.get('[data-testid="queue"]').text()).toContain("1 failed");
    const rows = w.findAll('[data-testid="scanner-rows"] tbody tr');
    expect(rows[0]!.text()).toContain("http_503");
    expect(rows[1]!.text()).toContain("never ran");
    expect(rows[1]!.get("[data-state]").attributes("data-state")).toBe("attention");
  });

  it("says no worker runs, and that new versions stay held", async () => {
    const w = await mountWith({ computed_at: NOW, workers: [], queue, scanners: [] });
    expect(w.get('[data-testid="workers"]').text()).toContain("new versions stay held");
    expect(w.get('[data-testid="no-scanner"]').text()).toContain("No scanner is configured");
  });
});
