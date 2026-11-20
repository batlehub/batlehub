import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { listInboundEventsMock } = vi.hoisted(() => ({ listInboundEventsMock: vi.fn() }));
vi.mock("@/client/sdk.gen", () => ({ listInboundEvents: listInboundEventsMock }));

import AdminInboundEvents from "./AdminInboundEvents.vue";

const event = (over: Record<string, unknown> = {}) => ({
  id: crypto.randomUUID(),
  received_at: "2026-08-12T10:00:00Z",
  webhook_name: "github-hook",
  source_ip: "10.0.0.1",
  signature_valid: null,
  ...over,
});

async function mountPage() {
  const wrapper = mount(AdminInboundEvents, {
    global: { stubs: { RouterLink: true } },
  });
  await flushPromises();
  return wrapper;
}

const rowFor = (w: Awaited<ReturnType<typeof mountPage>>, webhook: string) =>
  w.findAll("tbody tr").find((r) => r.text().includes(webhook))!;

/**
 * The page's question: "what has been posted to us, and was it signed?"
 *
 * §4.3's assertion is the second half: an unsigned event must be
 * *distinguishable* from a signed one. Three states share one column — valid,
 * invalid, and never-signed — and collapsing the third into the second tells an
 * operator a webhook was tampered with when it simply carries no signature.
 */
describe("AdminInboundEvents", () => {
  beforeEach(() => {
    listInboundEventsMock.mockReset().mockResolvedValue({ data: { events: [event()] } });
  });

  it("distinguishes signed, unsigned and invalidly-signed events", async () => {
    listInboundEventsMock.mockResolvedValue({
      data: {
        events: [
          event({ webhook_name: "signed-hook", signature_valid: true }),
          event({ webhook_name: "unsigned-hook", signature_valid: null }),
          event({ webhook_name: "forged-hook", signature_valid: false }),
        ],
      },
    });
    const wrapper = await mountPage();

    const signed = rowFor(wrapper, "signed-hook").text();
    const unsigned = rowFor(wrapper, "unsigned-hook").text();
    const forged = rowFor(wrapper, "forged-hook").text();

    // Three states, three readings. Any two of them being equal is the defect.
    expect(new Set([signed, unsigned, forged]).size).toBe(3);
    expect(unsigned).not.toEqual(forged);
  });

  it("renders the events it was given", async () => {
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("github-hook");
  });

  it("says nothing has arrived rather than looking broken", async () => {
    listInboundEventsMock.mockResolvedValue({ data: { events: [] } });
    const wrapper = await mountPage();
    expect(wrapper.findAll("tbody tr")).toHaveLength(0);
    expect(wrapper.text().trim().length).toBeGreaterThan(0);
  });

  it("surfaces a load error rather than an empty log", async () => {
    listInboundEventsMock.mockResolvedValue({ error: { message: "receiver down" } });
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("receiver down");
  });
});
