import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { listFindingsMock, listRegistriesMock, setGateExemptionMock, replaceMock } = vi.hoisted(
  () => ({
    listFindingsMock: vi.fn(),
    listRegistriesMock: vi.fn(),
    setGateExemptionMock: vi.fn(),
    replaceMock: vi.fn(),
  }),
);

vi.mock("@/client/sdk.gen", () => ({
  listFindings: listFindingsMock,
  listRegistries: listRegistriesMock,
  setGateExemption: setGateExemptionMock,
}));
vi.mock("vue-router", async () => {
  const { reactive, defineComponent, h } = await import("vue");
  const route = reactive({
    path: "/admin/security/findings",
    query: {} as Record<string, string>,
  });
  const RouterLink = defineComponent({
    props: { to: { type: String, required: true } },
    setup:
      (props, { slots }) =>
      () =>
        h("a", { href: props.to }, slots.default?.()),
  });
  return { useRoute: () => route, useRouter: () => ({ replace: replaceMock }), RouterLink };
});

import { useRoute } from "vue-router";
import AdminFindings from "./AdminFindings.vue";
import { Select } from "@/components/ui/select";
import { Pagination } from "@/components/ui/pagination";

const row = (name: string, over: Record<string, unknown> = {}) => ({
  registry: "npm",
  name,
  version: "1.0.0",
  scanner: "osv",
  kind: "vulnerability",
  code: "VULNERABILITY",
  severity: "high",
  reference: "GHSA-35jh-r3h4-6jhm",
  summary: `advisory in ${name}`,
  verdict: "warned",
  can_exempt: false,
  ...over,
});

const page = (items: unknown[], total = items.length) => ({
  data: { items, total, page: 1, per_page: 50 },
  response: { status: 200, statusText: "OK" },
});

const mounted: { unmount: () => void }[] = [];

async function mountPage(query: Record<string, string> = {}) {
  const route = useRoute() as unknown as { query: Record<string, string> };
  route.query = query;
  const w = mount(AdminFindings, {
    attachTo: document.body,
    global: { stubs: { SectionTabs: true } },
  });
  mounted.push(w);
  await flushPromises();
  return w;
}

const lastQuery = () => listFindingsMock.mock.calls.at(-1)![0].query;
const selectById = (w: Awaited<ReturnType<typeof mountPage>>, id: string) =>
  w.findAllComponents(Select).find((s) => s.props("id") === id)!;

beforeEach(() => {
  listFindingsMock.mockReset();
  setGateExemptionMock.mockReset();
  replaceMock.mockReset();
  listRegistriesMock.mockReset();
  listRegistriesMock.mockResolvedValue({ data: [{ name: "pypi" }, { name: "npm" }] });
});

afterEach(() => {
  mounted.splice(0).forEach((w) => w.unmount());
  document.body.innerHTML = "";
  vi.useRealTimers();
});

describe("AdminFindings", () => {
  it("lists findings with severity and verdict badges and a package link", async () => {
    listFindingsMock.mockResolvedValue(
      page([row("evil", { severity: "critical", verdict: "denied" }), row("lodash")]),
    );
    const w = await mountPage();
    const rows = w.findAll('[data-testid="finding-row"]');
    expect(rows).toHaveLength(2);
    expect(rows[0]!.text()).toContain("npm/evil@1.0.0");
    expect(rows[0]!.text()).toContain("critical");
    expect(rows[0]!.text()).toContain("denied");
    expect(rows[0]!.find("a").attributes("href")).toBe("/packages/npm/evil");
    expect(w.find('[data-testid="group-header"]').exists()).toBe(false);
    expect(lastQuery()).toMatchObject({ page: 1, per_page: 50, group: undefined });
  });

  it("says nothing matches", async () => {
    listFindingsMock.mockResolvedValue(page([]));
    const w = await mountPage();
    expect(w.text()).toContain("No findings match these filters.");
  });

  it("explains a 403 without a retry", async () => {
    listFindingsMock.mockResolvedValue({
      error: { message: "forbidden" },
      response: { status: 403, statusText: "Forbidden" },
    });
    const w = await mountPage();
    expect(w.find('[data-testid="refused"]').text()).toContain(
      "You cannot read findings on any registry.",
    );
    expect(w.text()).toContain("findings:read permission");
    expect(w.find('[data-testid="load-error"]').exists()).toBe(false);
  });

  it("shows an error and retries", async () => {
    listFindingsMock.mockResolvedValueOnce({
      error: { message: "down" },
      response: { status: 503, statusText: "Service Unavailable" },
    });
    const w = await mountPage();
    const alert = w.get('[data-testid="load-error"]');
    expect(alert.text()).toContain("Could not load findings: 503 Service Unavailable.");
    listFindingsMock.mockResolvedValue(page([row("lodash")]));
    await alert.get("button").trigger("click");
    await flushPromises();
    expect(w.find('[data-testid="load-error"]').exists()).toBe(false);
    expect(w.findAll('[data-testid="finding-row"]')).toHaveLength(1);
  });

  it("groups consecutive rows by reference under ?group=reference", async () => {
    listFindingsMock.mockResolvedValue(
      page([
        row("lodash", { version: "2.4.1", severity: "medium" }),
        row("lodash", { version: "2.4.0" }),
        row("evil", { reference: "MAL-2026-1234", severity: "critical" }),
      ]),
    );
    const w = await mountPage({ group: "reference" });
    expect(lastQuery().group).toBe("reference");
    const headers = w.findAll('[data-testid="group-header"]');
    expect(headers).toHaveLength(2);
    expect(headers[0]!.text()).toContain("GHSA-35jh-r3h4-6jhm");
    expect(headers[0]!.text()).toContain("high");
    expect(headers[0]!.text()).toContain("2 version(s) on this page");
    expect(headers[1]!.text()).toContain("MAL-2026-1234");
  });

  it("writes the grouping into the URL", async () => {
    listFindingsMock.mockResolvedValue(page([]));
    const w = await mountPage();
    await w.get('[data-testid="group-reference"]').trigger("click");
    expect(replaceMock).toHaveBeenCalledWith({
      path: "/admin/security/findings",
      query: { group: "reference" },
    });
  });

  it("puts the filters into the query", async () => {
    vi.useFakeTimers();
    listFindingsMock.mockResolvedValue(page([]));
    const w = await mountPage();
    selectById(w, "findings-registry").vm.$emit("update:modelValue", "npm");
    selectById(w, "findings-severity").vm.$emit("update:modelValue", "high");
    selectById(w, "findings-verdict").vm.$emit("update:modelValue", "denied");
    await flushPromises();
    await w.get("#findings-reference").setValue("GHSA-35");
    await w.get("#findings-scanner").setValue("osv");
    await vi.advanceTimersByTimeAsync(300);
    await flushPromises();
    expect(lastQuery()).toMatchObject({
      registry: "npm",
      severity: "high",
      verdict: "denied",
      reference: "GHSA-35",
      scanner: "osv",
    });
    expect(w.get("#findings-reference").attributes("maxlength")).toBe("128");
  });

  it("pages forward", async () => {
    listFindingsMock.mockResolvedValue(page([row("a")], 120));
    const w = await mountPage();
    w.findComponent(Pagination).vm.$emit("update:page", 1);
    await flushPromises();
    expect(lastQuery().page).toBe(2);
  });

  it("draws [Exempt…] only where can_exempt", async () => {
    listFindingsMock.mockResolvedValue(
      page([row("held", { verdict: "denied", can_exempt: true }), row("served")]),
    );
    const w = await mountPage();
    const rows = w.findAll('[data-testid="finding-row"]');
    expect(rows[0]!.find('[data-testid="exempt"]').exists()).toBe(true);
    expect(rows[1]!.find('[data-testid="exempt"]').exists()).toBe(false);
  });

  it("validates the exemption, then PUTs security_verdict and reloads", async () => {
    listFindingsMock.mockResolvedValue(
      page([row("held", { verdict: "denied", can_exempt: true })]),
    );
    const w = await mountPage();
    await w.get('[data-testid="exempt"]').trigger("click");
    await flushPromises();
    const form = () => document.querySelector('[data-testid="exempt-form"]') as HTMLFormElement;
    const err = () => document.querySelector('[data-testid="exempt-error"]')?.textContent ?? "";
    const set = async (sel: string, v: string) => {
      const el = document.querySelector(sel) as HTMLInputElement;
      el.value = v;
      el.dispatchEvent(new Event("input"));
      await flushPromises();
    };
    const submit = async () => {
      form().dispatchEvent(new Event("submit"));
      await flushPromises();
    };

    await submit();
    expect(err()).toContain("Give a reason");
    await set("#exempt-reason", "false positive, reviewed");
    await set("#exempt-until", "2000-01-01");
    await submit();
    expect(err()).toContain("after today");
    expect(setGateExemptionMock).not.toHaveBeenCalled();

    setGateExemptionMock.mockResolvedValue({ data: {} });
    const calls = listFindingsMock.mock.calls.length;
    await set("#exempt-until", "2999-01-01");
    await submit();
    expect(setGateExemptionMock).toHaveBeenCalledWith({
      path: { registry: "npm", package: "held", version: "1.0.0", gate: "security_verdict" },
      body: { reason: "false positive, reviewed", exempt_until: "2999-01-01T23:59:59Z" },
    });
    expect(document.querySelector('[data-testid="exempt-form"]')).toBeNull();
    expect(listFindingsMock.mock.calls).toHaveLength(calls + 1);
  });

  it("keeps the dialog open with the server's message on failure", async () => {
    listFindingsMock.mockResolvedValue(
      page([row("held", { verdict: "denied", can_exempt: true })]),
    );
    setGateExemptionMock.mockResolvedValue({ error: { message: "gates:exempt required" } });
    const w = await mountPage();
    await w.get('[data-testid="exempt"]').trigger("click");
    await flushPromises();
    for (const [sel, v] of [
      ["#exempt-reason", "why"],
      ["#exempt-until", "2999-01-01"],
    ] as const) {
      const el = document.querySelector(sel) as HTMLInputElement;
      el.value = v;
      el.dispatchEvent(new Event("input"));
    }
    await flushPromises();
    document.querySelector('[data-testid="exempt-form"]')!.dispatchEvent(new Event("submit"));
    await flushPromises();
    expect(document.querySelector('[data-testid="exempt-error"]')?.textContent).toContain(
      "gates:exempt required",
    );
  });
});
