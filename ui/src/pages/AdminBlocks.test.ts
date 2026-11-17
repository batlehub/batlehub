import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { listUsers, listIps, blockUserMock, unblockUserMock, blockIpMock, unblockIpMock } =
  vi.hoisted(() => ({
    listUsers: vi.fn(),
    listIps: vi.fn(),
    blockUserMock: vi.fn(),
    unblockUserMock: vi.fn(),
    blockIpMock: vi.fn(),
    unblockIpMock: vi.fn(),
  }));

vi.mock("@/client/sdk.gen", () => ({
  listBlockedUsers: listUsers,
  listBlockedIps: listIps,
  blockUser: blockUserMock,
  unblockUser: unblockUserMock,
  blockIp: blockIpMock,
  unblockIp: unblockIpMock,
}));

import AdminBlocks from "./AdminBlocks.vue";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";

/** Seconds since the epoch, which is what `BlockedIpDto` carries. */
const epoch = (offsetMs: number) => Math.floor((Date.now() + offsetMs) / 1000);

/**
 * Teardown is load-bearing here, not hygiene.
 *
 * `Dialog` teleports its content to `document.body`, and `mountPage` attaches
 * there too. Without an unmount, the previous test's dialog is still in the
 * document, so `querySelector("#block-subject")` returns *its* input — the
 * next test types into a detached form and reads a count of zero from the
 * live one.
 */
let active: ReturnType<typeof mount> | null = null;

afterEach(() => {
  active?.unmount();
  active = null;
  document.body.innerHTML = "";
});

async function mountPage() {
  const wrapper = mount(AdminBlocks, {
    // `Dialog` teleports, so the tree has to be in the document for the
    // dialog's own content to be reachable at all.
    attachTo: document.body,
    global: { stubs: { RouterLink: { template: "<a><slot /></a>" } } },
  });
  await flushPromises();
  active = wrapper;
  return wrapper;
}

/**
 * The confirmation currently on screen.
 *
 * Both halves of this page route through `DestructiveConfirm` now — adding a
 * block and lifting one — so `findComponent` returns whichever is first in the
 * tree rather than whichever is open.
 */
function openConfirm(wrapper: ReturnType<typeof mount>) {
  const dialog = wrapper.findAllComponents(DestructiveConfirm).find((d) => d.props("open"));
  if (!dialog) throw new Error("no DestructiveConfirm is open");
  return dialog;
}

/** Open the adder for one kind and type a subject into it. */
async function openAdder(
  wrapper: ReturnType<typeof mount>,
  button: RegExp,
  subject: string,
): Promise<void> {
  await wrapper
    .findAll("button")
    .find((b) => button.test(b.text()))!
    .trigger("click");
  await flushPromises();

  if (subject) {
    // `Dialog` teleports its content out of the wrapper, so the form lives on
    // the document rather than inside the mounted tree.
    const input = document.querySelector<HTMLInputElement>("#block-subject")!;
    input.value = subject;
    input.dispatchEvent(new Event("input"));
    await flushPromises();
  }
}

describe("AdminBlocks", () => {
  beforeEach(() => {
    listUsers.mockReset().mockResolvedValue({
      data: [
        {
          user_id: "svc-ci-runner-eu-west-1",
          reason: "Leaked token",
          blocked_at: "2026-08-10T14:22:00Z",
          blocked_by: "alice",
        },
      ],
    });
    listIps.mockReset().mockResolvedValue({
      data: [
        {
          ip: "203.0.113.42",
          reason: "Sustained 429s against the npm upstream",
          blocked_at: epoch(-3_600_000),
          unblock_at: epoch(3_600_000),
        },
      ],
    });
    blockUserMock.mockReset().mockResolvedValue({ data: {} });
    unblockUserMock.mockReset().mockResolvedValue({ data: {} });
    blockIpMock.mockReset().mockResolvedValue({ data: {} });
    unblockIpMock.mockReset().mockResolvedValue({ data: {} });
  });

  /**
   * The point of the merge (RFC 0004 Phase 5): an operator arrives with a
   * symptom — "the EU CI runner started failing" — not with a mechanism, and
   * previously had to visit two routes because neither mentioned the other.
   */
  it("shows both mechanisms in one table, labelled by kind", async () => {
    const wrapper = await mountPage();
    const rows = wrapper.findAll("tbody tr");
    expect(rows).toHaveLength(2);
    expect(wrapper.text()).toContain("svc-ci-runner-eu-west-1");
    expect(wrapper.text()).toContain("203.0.113.42");
    expect(wrapper.text()).toMatch(/Account/);
    expect(wrapper.text()).toMatch(/Address/);
  });

  it("renders a block with no reason without inventing one", async () => {
    listUsers.mockResolvedValue({
      data: [{ user_id: "carol", reason: null, blocked_at: null, blocked_by: "admin" }],
    });
    listIps.mockResolvedValue({ data: [] });
    const wrapper = await mountPage();
    expect(wrapper.text()).toContain("carol");
    expect(wrapper.text()).toContain("—");
  });

  /**
   * An expired IP block is history, not a block. Both source pages kept it in
   * the list at reduced opacity with a working "Unblock" button — an action
   * offered for something already unblocked.
   */
  it("marks an expired address block and offers no verb for it", async () => {
    listUsers.mockResolvedValue({ data: [] });
    listIps.mockResolvedValue({
      data: [
        {
          ip: "198.51.100.7",
          reason: "auto",
          blocked_at: epoch(-7_200_000),
          unblock_at: epoch(-60_000),
        },
      ],
    });
    const wrapper = await mountPage();
    expect(wrapper.text()).toMatch(/expired/i);
    const row = wrapper.get("tbody tr");
    expect(row.findAll("button")).toHaveLength(0);
  });

  /**
   * The middleware writes `"auto"` when an address crosses the violation
   * threshold. Those are the entries an operator did *not* put there, and the
   * bare word told them nothing.
   */
  it("explains a machine-created block rather than printing 'auto'", async () => {
    listUsers.mockResolvedValue({ data: [] });
    listIps.mockResolvedValue({
      data: [
        {
          ip: "198.51.100.9",
          reason: "auto",
          blocked_at: epoch(-600_000),
          unblock_at: epoch(600_000),
        },
      ],
    });
    const wrapper = await mountPage();
    expect(wrapper.text()).toMatch(/automatic/i);
  });

  it("filters across both kinds by subject and by reason", async () => {
    const wrapper = await mountPage();
    await wrapper.get("input").setValue("429s");
    await flushPromises();
    const rows = wrapper.findAll("tbody tr");
    expect(rows).toHaveLength(1);
    expect(wrapper.text()).toContain("203.0.113.42");
  });

  /**
   * `DestructiveConfirm` names IP blocks in its own doc comment as one of the
   * four reasons it exists, and neither source page used it.
   */
  it("routes lifting a block through the destructive contract", async () => {
    const wrapper = await mountPage();
    const lift = wrapper.findAll("button").find((b) => /lift/i.test(b.text()))!;
    await lift.trigger("click");
    await flushPromises();

    // Both halves of this page use the contract now, so the one that is *open*
    // is the one under test — `findComponent` would always return the adder.
    const dialog = openConfirm(wrapper);
    expect(dialog.props("open")).toBe(true);
    expect(dialog.props("scope")).toBeTruthy();

    dialog.vm.$emit("confirm");
    await flushPromises();
    expect(unblockUserMock.mock.calls.length + unblockIpMock.mock.calls.length).toBe(1);
  });

  it("surfaces a failed block instead of closing silently", async () => {
    blockUserMock.mockResolvedValue({ error: { message: "not permitted" } });
    const wrapper = await mountPage();
    await openAdder(wrapper, /block account/i, "mallory");

    openConfirm(wrapper).vm.$emit("confirm");
    await flushPromises();
    expect(document.body.textContent).toContain("not permitted");
  });

  /**
   * PRODUCT.md principle 2 names blocking as one of the four actions that must
   * state scope, count and consequence. Adding one went through a bare
   * `Dialog` — no confirmation at all — while *lifting* one, seven lines below
   * it in the same file, got the full contract. The restorative half was
   * confirmed and the destructive half was not.
   */
  it("routes adding a block through the destructive contract too", async () => {
    const wrapper = await mountPage();
    await openAdder(wrapper, /block account/i, "mallory");

    const dialog = openConfirm(wrapper);
    expect(dialog.props("count")).toBe(1);
    expect(dialog.props("scope")).toBe("mallory");
    // A block is lifted by this very page, so it is reversible and takes no
    // typed-name step — uniform friction teaches people to type through it.
    expect(dialog.props("reversible")).toBe(true);

    dialog.vm.$emit("confirm");
    await flushPromises();
    expect(blockUserMock).toHaveBeenCalledTimes(1);
  });

  /** Zero until an address is named — which is also what disables the confirm. */
  it("confirms nothing while the subject is empty", async () => {
    const wrapper = await mountPage();
    await openAdder(wrapper, /block account/i, "");
    expect(openConfirm(wrapper).props("count")).toBe(0);
  });

  /**
   * The mechanism, stated. Both middlewares run *before* any rule is
   * evaluated, so a mistyped CIDR does not degrade a policy — it cuts off
   * every agent behind that egress. The dialog said none of it.
   */
  it("states what a block actually does, per kind", async () => {
    const wrapper = await mountPage();
    await openAdder(wrapper, /block account/i, "mallory");
    expect(document.body.textContent).toContain("401");

    await openAdder(wrapper, /block address/i, "203.0.113.9");
    expect(document.body.textContent).toContain("403");
  });

  /**
   * The duration was `<input type="number">` in raw seconds, defaulting to
   * 3600: the operator had to know a day is 86400 and type it correctly under
   * pressure, and one mistyped digit is the difference between an hour and
   * eleven days.
   */
  it("offers durations rather than raw seconds", async () => {
    const wrapper = await mountPage();
    await openAdder(wrapper, /block address/i, "203.0.113.9");

    const duration = document.querySelector("#block-duration");
    expect(duration, "the duration field exists").not.toBeNull();
    expect(duration!.getAttribute("type")).not.toBe("number");
  });

  /**
   * The self-lockout warning fired for *every* address typed —
   * `dialogKind === "ip" && subject.length > 0` — so it never told anyone
   * anything, and a warning that is always on teaches people to click past it.
   *
   * Comparing against the address the server actually saw needs an API that
   * exposes it; `MeResponse` carries only role, groups and user_id. Removed
   * rather than left lying, and reinstated when the endpoint exists.
   */
  it("does not warn about a self-lockout it cannot detect", async () => {
    const wrapper = await mountPage();
    await openAdder(wrapper, /block address/i, "203.0.113.9");
    expect(document.body.textContent).not.toMatch(/lock (yourself )?out/i);
  });

  it("says nobody is blocked when nobody is", async () => {
    listUsers.mockResolvedValue({ data: [] });
    listIps.mockResolvedValue({ data: [] });
    const wrapper = await mountPage();
    expect(wrapper.text()).toMatch(/nobody is blocked/i);
  });
});
