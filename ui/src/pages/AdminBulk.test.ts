import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { blockMock, unblockMock } = vi.hoisted(() => ({ blockMock: vi.fn(), unblockMock: vi.fn() }));
vi.mock("@/client/sdk.gen", () => ({
  bulkBlockPackages: blockMock,
  bulkUnblockPackages: unblockMock,
}));

import AdminBulk from "./AdminBulk.vue";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";

const CSV = "npm,left-pad,1.0.0,,CVE-2026-1\nnpm,is-odd,2.0.0,,CVE-2026-2";

async function mountPage() {
  const wrapper = mount(AdminBulk, {
    global: { stubs: { RouterLink: { template: "<a><slot /></a>" } } },
  });
  await flushPromises();
  return wrapper;
}

/** Paste rows and preview them, leaving the page armed to submit. */
async function stage(wrapper: Awaited<ReturnType<typeof mountPage>>) {
  await wrapper.get("textarea").setValue(CSV);
  const preview = wrapper.findAll("button").find((b) => /preview/i.test(b.text()))!;
  await preview.trigger("click");
  await flushPromises();
  return wrapper;
}

const submitButton = (wrapper: Awaited<ReturnType<typeof mountPage>>) =>
  wrapper.findAll("button").find((b) => /^Block \d|^Unblock \d/.test(b.text()));

/**
 * Confirm through the component's contract rather than by finding a button by
 * text: the page's own action selector also has buttons labelled "Block" and
 * "Unblock", and a text search finds those first.
 */
async function confirmDialog(wrapper: Awaited<ReturnType<typeof mountPage>>) {
  const dialog = wrapper.findComponent(DestructiveConfirm);
  expect(dialog.props("open")).toBe(true);
  dialog.vm.$emit("confirm");
  await flushPromises();
}

describe("AdminBulk", () => {
  beforeEach(() => {
    blockMock.mockReset().mockResolvedValue({
      data: { succeeded_count: 2, failed_count: 0, failures: [] },
    });
    unblockMock.mockReset().mockResolvedValue({
      data: { succeeded_count: 2, failed_count: 0, failures: [] },
    });
  });

  /**
   * PRODUCT.md principle 2: scope, count and consequence before confirmation.
   * Neither bulk path had any — one click blocked or released an arbitrary
   * number of coordinates, while the sibling tab confirmed the same verbs.
   */
  it("does not submit until the destructive contract is satisfied", async () => {
    const wrapper = await stage(await mountPage());
    await submitButton(wrapper)!.trigger("click");
    await flushPromises();

    expect(blockMock).not.toHaveBeenCalled();
    const dialog = wrapper.text();
    expect(dialog).toMatch(/2/); // the count is stated
  });

  it("submits once confirmed", async () => {
    const wrapper = await stage(await mountPage());
    await submitButton(wrapper)!.trigger("click");
    await flushPromises();

    await confirmDialog(wrapper);
    expect(blockMock).toHaveBeenCalledTimes(1);
    expect(wrapper.text()).toMatch(/2/);
  });

  /**
   * The generated client is built without `throwOnError`, so an HTTP failure
   * resolves rather than throws. The page assigned `res.data ?? null` and the
   * `catch` never ran — a bulk block returning 405 rendered nothing at all.
   */
  it("surfaces a failed request instead of rendering nothing", async () => {
    blockMock.mockResolvedValue({ error: { message: "stub is read-only" } });
    const wrapper = await stage(await mountPage());
    await submitButton(wrapper)!.trigger("click");
    await flushPromises();
    await confirmDialog(wrapper);
    expect(wrapper.text()).toContain("stub is read-only");
  });

  /**
   * The result header rendered a crimson success check unconditionally, so
   * "0 succeeded, 30 failed" was introduced by a tick. The icon is the first
   * thing read and it was reporting the opposite of what happened.
   */
  it("does not mark a total failure as a success", async () => {
    blockMock.mockResolvedValue({
      data: {
        succeeded_count: 0,
        failed_count: 2,
        failures: [{ registry: "npm", name: "left-pad", version: "1.0.0", error: "nope" }],
      },
    });
    const wrapper = await stage(await mountPage());
    await submitButton(wrapper)!.trigger("click");
    await flushPromises();
    await confirmDialog(wrapper);
    const header = wrapper.get(".text-base");
    expect(header.find(".text-primary").exists()).toBe(false);
    expect(wrapper.text()).toContain("left-pad");
  });

  /* RFC 0006 §6.8: the bulk path states what a block does once, above the
     list, rather than per row — including the artifact-scoped consequence,
     which is the one an operator is likely to be surprised by. */
  it("states what a block does when the block action is selected", async () => {
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("stop seeing it in version listings");
    expect(wrapper.text()).toContain("hides the whole version from listings");
  });
});
