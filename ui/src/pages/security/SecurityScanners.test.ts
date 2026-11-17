import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { listScansMock, listVerdictsMock, rescanVerdictMock, listRegistriesMock, replaceMock } =
  vi.hoisted(() => ({
    listScansMock: vi.fn(),
    listVerdictsMock: vi.fn(),
    rescanVerdictMock: vi.fn(),
    listRegistriesMock: vi.fn(),
    replaceMock: vi.fn(),
  }));

vi.mock("@/client/sdk.gen", () => ({
  listScans: listScansMock,
  listVerdicts: listVerdictsMock,
  rescanVerdict: rescanVerdictMock,
  listRegistries: listRegistriesMock,
  // The health block on top (RFC 0050 §4.5) — its own tests are the block's.
  securityScanners: () =>
    Promise.resolve({
      data: {
        computed_at: "",
        workers: [],
        queue: { queued: 0, deferred: 0, deferred_over_day: 0, failed: 0 },
        scanners: [],
      },
    }),
}));
vi.mock("vue-router", async () => {
  const { reactive } = await import("vue");
  const route = reactive({ path: "/admin/security/scans", query: {} as Record<string, string> });
  return { useRoute: () => route, useRouter: () => ({ replace: replaceMock }) };
});

import { useRoute } from "vue-router";
import SecurityScanners from "./SecurityScanners.vue";
import { Select } from "@/components/ui/select";
import { Tabs } from "@/components/ui/tabs";

const job = (name: string, over: Record<string, unknown> = {}) => ({
  registry: "npm",
  name,
  version: "1.0.0",
  state: "queued",
  trigger: "first_seen",
  attempts: 0,
  since: "2026-11-10T12:00:00Z",
  error_withheld: false,
  can_rescan: false,
  ...over,
});

const page = (items: unknown[]) => ({
  data: { items, total: items.length, page: 1, per_page: 50 },
});

/** The queue answers `open`, the failures list `failed`. */
function serve(open: unknown[], failed: unknown[] = []) {
  listScansMock.mockImplementation(({ query }: { query: { state?: string } }) =>
    Promise.resolve(page(query.state === "failed" ? failed : open)),
  );
}

/* Unmounted after each test: the route is one shared reactive object, and a
   live page from the previous test would react to the next test's `?tab=`. */
const mounted: { unmount: () => void }[] = [];

async function mountPage(query: Record<string, string> = {}) {
  const route = useRoute() as unknown as { query: Record<string, string> };
  route.query = query;
  const w = mount(SecurityScanners, { global: { stubs: {} } });
  mounted.push(w);
  await flushPromises();
  return w;
}

const selectById = (w: Awaited<ReturnType<typeof mountPage>>, id: string) =>
  w.findAllComponents(Select).find((s) => s.props("id") === id)!;

beforeEach(() => {
  listScansMock.mockReset();
  listVerdictsMock.mockReset();
  rescanVerdictMock.mockReset();
  replaceMock.mockReset();
  listRegistriesMock.mockReset();
  listRegistriesMock.mockResolvedValue({ data: [{ name: "pypi" }, { name: "npm" }] });
});

afterEach(() => {
  mounted.splice(0).forEach((w) => w.unmount());
  vi.useRealTimers();
});

describe("SecurityScanners", () => {
  it("lists the queue, scanning first, with both tab counts", async () => {
    serve(
      [
        job("react", { state: "scanning", leased_by: "worker-2", attempts: 1 }),
        job("left-pad", { deferred_since: "2026-11-10T11:00:00Z" }),
      ],
      [job("huge", { state: "failed" })],
    );
    const w = await mountPage();
    const rows = w.findAll("tbody tr");
    expect(rows).toHaveLength(2);
    expect(rows[0]!.text()).toContain("npm/react@1.0.0");
    expect(rows[0]!.text()).toContain("worker-2");
    expect(rows[0]!.find('[data-testid="scan-badge"]').attributes("data-state")).toBe("scanning");
    expect(rows[1]!.text()).toContain("deferred since");
    expect(rows[1]!.text()).toContain("—");
    expect(w.text()).toContain("Queue (2)");
    expect(w.text()).toContain("Failures (1)");
    expect(w.find('[data-testid="polling"]').exists()).toBe(true);
  });

  it("says the queue is empty and does not poll", async () => {
    vi.useFakeTimers();
    serve([]);
    const w = await mountPage();
    expect(w.text()).toContain("Queue empty — every known version has a verdict.");
    expect(w.find('[data-testid="polling"]').exists()).toBe(false);
    const calls = listScansMock.mock.calls.length;
    await vi.advanceTimersByTimeAsync(15_000);
    expect(listScansMock.mock.calls).toHaveLength(calls);
  });

  it("polls every 5 s while the queue holds a transient row", async () => {
    vi.useFakeTimers();
    serve([job("react")]);
    await mountPage();
    const calls = listScansMock.mock.calls.length;
    await vi.advanceTimersByTimeAsync(5_000);
    expect(listScansMock.mock.calls).toHaveLength(calls + 2);
  });

  it("shows failures with the error, the withheld case and the lease-expired case", async () => {
    serve(
      [],
      [
        job("weird", { state: "failed", attempts: 3, last_error: "tar: unexpected EOF" }),
        job("hidden", { state: "failed", error_withheld: true }),
        job("huge", { state: "failed", can_rescan: true }),
      ],
    );
    const w = await mountPage({ tab: "failures" });
    const rows = w.findAll("tbody tr");
    expect(rows[0]!.text()).toContain("tar: unexpected EOF");
    expect(rows[1]!.text()).toContain("withheld (needs findings:read)");
    expect(rows[2]!.text()).toContain("— (lease expired; no error recorded)");
    // [Rescan] only where the caller holds gates:exempt.
    expect(rows[0]!.find("button").exists()).toBe(false);
    expect(rows[1]!.find("button").exists()).toBe(false);
    expect(rows[2]!.find("button").text()).toBe("Rescan");
  });

  it("rescans a failure and re-reads both lists", async () => {
    serve([], [job("huge", { state: "failed", can_rescan: true })]);
    rescanVerdictMock.mockResolvedValue({ data: { queued: true } });
    const w = await mountPage({ tab: "failures" });
    const before = listScansMock.mock.calls.length;
    serve([job("huge")], []);
    await w.find("tbody tr button").trigger("click");
    await flushPromises();
    expect(rescanVerdictMock).toHaveBeenCalledWith({
      path: { registry: "npm", name: "huge", version: "1.0.0" },
    });
    expect(listScansMock.mock.calls).toHaveLength(before + 2);
    expect(w.text()).toContain("Queue (1)");
    expect(w.text()).toContain("Failures (0)");
  });

  it("names a refused rescan", async () => {
    serve([], [job("huge", { state: "failed", can_rescan: true })]);
    rescanVerdictMock.mockResolvedValue({ error: { message: "forbidden" } });
    const w = await mountPage({ tab: "failures" });
    await w.find("tbody tr button").trigger("click");
    await flushPromises();
    expect(w.find('[data-testid="rescan-error"]').text()).toContain("npm/huge@1.0.0");
  });

  it("shows the load error with its status, and Retry recovers", async () => {
    listScansMock.mockResolvedValue({
      error: {},
      response: { status: 503, statusText: "Service Unavailable" },
    });
    const w = await mountPage();
    expect(w.find('[data-testid="load-error"]').text()).toContain(
      "Could not load the scan queue: 503 Service Unavailable.",
    );
    serve([job("react")]);
    await w.find('[data-testid="load-error"] button').trigger("click");
    await flushPromises();
    expect(w.find('[data-testid="load-error"]').exists()).toBe(false);
    expect(w.text()).toContain("npm/react@1.0.0");
  });

  it("falls back to the queue on an unknown tab and writes the tab with replace", async () => {
    serve([job("react")]);
    const w = await mountPage({ tab: "bogus" });
    expect(w.text()).toContain("npm/react@1.0.0");
    w.findComponent(Tabs).vm.$emit("update:modelValue", "verdicts");
    expect(replaceMock).toHaveBeenCalledWith({
      path: "/admin/security/scans",
      query: { tab: "verdicts" },
    });
  });

  it("filters both lists by registry and by queue state", async () => {
    serve([job("a")]);
    const w = await mountPage();
    expect(selectById(w, "scans-registry").props("options")).toEqual([
      { value: "", label: "All registries" },
      { value: "npm", label: "npm" },
      { value: "pypi", label: "pypi" },
    ]);
    selectById(w, "scans-registry").vm.$emit("update:modelValue", "pypi");
    await flushPromises();
    expect(listScansMock).toHaveBeenLastCalledWith({
      query: { registry: "pypi", state: "failed", page: 1, per_page: 50 },
    });
    selectById(w, "scans-state").vm.$emit("update:modelValue", "scanning");
    await flushPromises();
    expect(listScansMock.mock.calls.map((c) => c[0].query.state)).toContain("scanning");
  });

  /** RFC 0050 §6.7: no state filter over an empty, unfiltered queue. */
  it("hides the state filter while the queue is empty", async () => {
    serve([]);
    const w = await mountPage();
    expect(w.find("#scans-state").exists()).toBe(false);
  });

  /** RFC 0050 §6.6: the verdicts tab left for Security › Holds. */
  it("has no verdicts tab any more", async () => {
    serve([]);
    const w = await mountPage({ tab: "verdicts" });
    expect(w.text()).not.toContain("Verdicts");
    expect(listVerdictsMock).not.toHaveBeenCalled();
  });
});
