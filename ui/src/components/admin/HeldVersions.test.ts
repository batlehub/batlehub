import { describe, it, expect, vi, beforeEach } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { reactive } from "vue";

const { listMock, coverageMock, replaceMock, route } = vi.hoisted(() => ({
  listMock: vi.fn(),
  coverageMock: vi.fn(),
  replaceMock: vi.fn(),
  route: { query: {} as Record<string, string> },
}));
vi.mock("@/client/sdk.gen", () => ({ listVerdicts: listMock, securityCoverage: coverageMock }));
vi.mock("vue-router", () => ({
  RouterLink: { props: ["to"], template: "<a><slot/></a>" },
  useRoute: () => route,
  useRouter: () => ({ replace: replaceMock }),
}));

import HeldVersions from "./HeldVersions.vue";

const verdict = (name: string) => ({
  package: { registry: "npm-public", name, version: "1.0.0" },
  state: "denied",
  reason_codes: ["SOC_VERDICT"],
  findings: [],
  policy_ref: "npm-public/default",
  evaluated_at: new Date().toISOString(),
  scanners_done: [],
});

beforeEach(() => {
  listMock.mockReset();
  replaceMock.mockReset();
  coverageMock.mockResolvedValue({ data: { registries: [] } });
  route.query = reactive({});
});

/** A mass compromise: 1 400 SOC holds, walked page by page from the URL's filter. */
describe("HeldVersions", () => {
  it("asks the held state and the URL's reason, and pages to the end", async () => {
    route.query = reactive({ reason: "SOC_VERDICT", page: "2" });
    listMock.mockResolvedValue({
      data: { items: [verdict("a"), verdict("b")], total: 1400, page: 2, per_page: 50 },
    });
    const w = mount(HeldVersions);
    await flushPromises();
    expect(listMock).toHaveBeenLastCalledWith({
      query: { state: "held", registry: undefined, reason: "SOC_VERDICT", page: 2, per_page: 50 },
    });
    expect(w.get('[data-testid="held-pager"]').text()).toContain("Page 2 of 28");
    expect(w.get('[data-testid="held-rows"]').text()).toContain("npm-public/a@1.0.0");
    const next = w.findAll("button").find((b) => b.text() === "Next")!;
    await next.trigger("click");
    expect(replaceMock).toHaveBeenLastCalledWith({
      query: { reason: "SOC_VERDICT", page: "3" },
    });
  });
});
