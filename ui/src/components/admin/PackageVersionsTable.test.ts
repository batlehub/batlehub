import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";
import { createRouter, createMemoryHistory } from "vue-router";
import type { PackageVersionDetail } from "@/client/types.gen";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";

const {
  blockPackageMock,
  unblockPackageMock,
  bulkBlockPackagesMock,
  bulkUnblockPackagesMock,
  invalidatePackageMock,
  listRegistriesMock,
} = vi.hoisted(() => ({
  blockPackageMock: vi.fn(),
  unblockPackageMock: vi.fn(),
  bulkBlockPackagesMock: vi.fn(),
  bulkUnblockPackagesMock: vi.fn(),
  invalidatePackageMock: vi.fn(),
  listRegistriesMock: vi.fn(),
}));

vi.mock("@/client/sdk.gen", () => ({
  blockPackage: blockPackageMock,
  unblockPackage: unblockPackageMock,
  bulkBlockPackages: bulkBlockPackagesMock,
  bulkUnblockPackages: bulkUnblockPackagesMock,
  invalidatePackage: invalidatePackageMock,
  listRegistries: listRegistriesMock,
}));

import PackageVersionsTable from "./PackageVersionsTable.vue";

function version(over: Partial<PackageVersionDetail> = {}): PackageVersionDetail {
  return {
    id: "v1",
    version: "1.0.0",
    artifact: "pkg-1.0.0.tgz",
    cached: false,
    cached_at: null,
    access_count: 0,
    last_accessed: null,
    last_accessed_by: null,
    socket_badge_url: null,
    status: { status: "available" },
    storage_backend: null,
    storage_key: "npm/pkg/1.0.0",
    license: null,
    vulnerabilities: [],
    ...over,
  } as PackageVersionDetail;
}

async function mountComp(
  versions: PackageVersionDetail[],
  at: { registry: string; name: string } = { registry: "npm", name: "pkg" },
) {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: "/", component: { template: "<div />" } },
      { path: "/packages/:registry/:name", component: { template: "<div />" } },
    ],
  });
  await router.push("/");
  await router.isReady();
  const wrapper = mount(PackageVersionsTable, {
    props: { ...at, versions },
    attachTo: document.body,
    global: { plugins: [router] },
  });
  active = wrapper;
  return { wrapper, router };
}

/**
 * The four destructive verbs went through `prompt()` and `confirm()` and now
 * go through `DestructiveConfirm`. These two helpers are what that costs the
 * tests: the dialog teleports, so its field is on the document rather than
 * inside the wrapper, and confirming is an event rather than a return value.
 */
let active: ReturnType<typeof mount> | null = null;

/** Type a block reason into the dialog's field. */
async function typeReason(text: string) {
  const input = document.querySelector<HTMLInputElement>("#version-block-reason");
  if (!input) throw new Error("the dialog has no reason field");
  input.value = text;
  input.dispatchEvent(new Event("input"));
  await flushPromises();
}

/** Confirm whatever is open, optionally filling the reason first. */
async function confirmDialog(wrapper: ReturnType<typeof mount>, reason?: string) {
  if (reason !== undefined) await typeReason(reason);
  wrapper.findComponent(DestructiveConfirm).vm.$emit("confirm");
  await flushPromises();
}

/** Press one of the row or toolbar buttons by its label. */
const press = (wrapper: ReturnType<typeof mount>, label: string) =>
  wrapper
    .findAll("button")
    .find((b) => b.text() === label)!
    .trigger("click");

describe("PackageVersionsTable", () => {
  beforeEach(() => {
    blockPackageMock.mockReset().mockResolvedValue({});
    unblockPackageMock.mockReset().mockResolvedValue({});
    bulkBlockPackagesMock
      .mockReset()
      .mockResolvedValue({ data: { succeeded_count: 1, failed_count: 0 } });
    bulkUnblockPackagesMock
      .mockReset()
      .mockResolvedValue({ data: { succeeded_count: 1, failed_count: 0 } });
    invalidatePackageMock.mockReset().mockResolvedValue({});
    listRegistriesMock.mockReset().mockResolvedValue({
      data: [
        { name: "npm", type: "npm" },
        { name: "gh", type: "github" },
        { name: "mv", type: "mise-versions" },
      ],
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    active?.unmount();
    active = null;
    document.body.innerHTML = "";
  });

  it("shows the empty state when there are no versions", async () => {
    const { wrapper } = await mountComp([]);
    expect(wrapper.text()).toContain("No versions tracked yet.");
  });

  it("renders a row per version with a pre-release badge", async () => {
    const { wrapper } = await mountComp([version(), version({ id: "v2", version: "2.0.0-rc.1" })]);
    const rows = wrapper.findAll("tbody tr");
    expect(rows).toHaveLength(2);
    expect(rows[1].text()).toContain("pre-release");
    expect(rows[0].text()).not.toContain("pre-release");
  });

  it("shows vulnerability badges by severity", async () => {
    const { wrapper } = await mountComp([
      version({
        vulnerabilities: [
          {
            osv_id: "OSV-1",
            summary: "bad",
            severity: "critical",
            fixed_version: "1.0.1",
            purl: "pkg:npm/test@1.0.0",
          },
          {
            osv_id: "OSV-2",
            summary: "meh",
            severity: "low",
            fixed_version: null,
            purl: "pkg:npm/test@1.0.0",
          },
        ],
      }),
    ]);
    expect(wrapper.text()).toContain("critical");
    expect(wrapper.text()).toContain("low");
  });

  it("navigates to the detail page on View", async () => {
    const { wrapper, router } = await mountComp([version()]);
    const push = vi.spyOn(router, "push");
    const viewBtn = wrapper.findAll("button").find((b) => b.text() === "View")!;
    await viewBtn.trigger("click");
    /* The package is named by the path now; version and artifact stay as query
       because they select within a package rather than name a different one. */
    expect(push).toHaveBeenCalledWith({
      path: "/packages/npm/pkg",
      query: { version: "1.0.0", artifact: "pkg-1.0.0.tgz" },
    });
  });

  it("blocks a version with the reason the dialog collected", async () => {
    const { wrapper } = await mountComp([version()]);
    await press(wrapper, "Block");
    expect(blockPackageMock, "the click alone must not block").not.toHaveBeenCalled();

    await confirmDialog(wrapper, "bad license");
    expect(blockPackageMock).toHaveBeenCalledWith({
      body: {
        registry: "npm",
        name: "pkg",
        version: "1.0.0",
        artifact: "pkg-1.0.0.tgz",
        reason: "bad license",
      },
    });
    expect(wrapper.emitted("reload")).toHaveLength(1);
  });

  /* RFC 0006 §6.8: a block has two halves, and the operator should read that
     where the block is made rather than discover it from a listing. It was the
     most useful thing the old prompt said, and it survived the move — as the
     dialog's `consequence`, which is what that prop is for. */
  it("states both halves of a block in the dialog", async () => {
    const { wrapper } = await mountComp([version({ artifact: null })]);
    await press(wrapper, "Block");

    const shown = wrapper.findComponent(DestructiveConfirm).props("consequence") as string;
    expect(shown).toContain("stop seeing it in version listings");
    expect(shown).toContain("403");
    expect(document.body.textContent).toContain("403");
  });

  /* The asymmetry a reviewer will want to argue about, so the console says it:
     blocking one file hides the whole version from listings. */
  it("states the whole-version listing consequence for an artifact-scoped block", async () => {
    const { wrapper } = await mountComp([version({ artifact: "pkg-1.0.0.tgz" })]);
    await press(wrapper, "Block");

    const shown = wrapper.findComponent(DestructiveConfirm).props("consequence") as string;
    expect(shown).toContain("hides the whole 1.0.0 version from version listings");
    expect(shown).toContain("stay downloadable by exact coordinate");
  });

  /* RFC 0043 decision 1: a forge release has a second name in mise's list. */
  it("offers the matching mise block on a forge release, pre-filled and opt-in", async () => {
    const { wrapper } = await mountComp([version({ version: "jq-1.8.2", artifact: null })], {
      registry: "gh",
      name: "jqlang/jq",
    });
    await press(wrapper, "Block");
    const offer = document.querySelector<HTMLInputElement>("#mise-block-0");
    expect(offer, "one offer for the one mise-versions registry").not.toBeNull();
    expect(document.body.textContent).toContain("Also block it in mv");

    // Unchosen, confirming blocks the forge release only.
    await confirmDialog(wrapper, "CVE-2026-0000");
    expect(blockPackageMock).toHaveBeenCalledTimes(1);

    blockPackageMock.mockClear();
    await press(wrapper, "Block");
    document.querySelector<HTMLInputElement>("#mise-block-0")!.click();
    await flushPromises();
    expect(document.querySelector<HTMLInputElement>("#mise-block-tool-0")!.value).toBe("jq");
    const ver = document.querySelector<HTMLInputElement>("#mise-block-version-0")!;
    expect(ver.value).toBe("1.8.2");
    ver.value = "1.8.2-edited";
    ver.dispatchEvent(new Event("input"));
    await confirmDialog(wrapper, "CVE-2026-0000");
    expect(blockPackageMock).toHaveBeenNthCalledWith(2, {
      body: { registry: "mv", name: "jq", version: "1.8.2-edited", reason: "CVE-2026-0000" },
    });
  });

  it("offers no mise block outside a forge registry", async () => {
    const { wrapper } = await mountComp([version()]);
    await press(wrapper, "Block");
    expect(document.querySelector("#mise-block-0")).toBeNull();
  });

  it("says which mise block was refused after the forge block landed", async () => {
    blockPackageMock
      .mockResolvedValueOnce({})
      .mockResolvedValueOnce({ error: { message: "forbidden" } });
    const { wrapper } = await mountComp([version({ version: "v2.0.0", artifact: null })], {
      registry: "gh",
      name: "cli/cli",
    });
    await press(wrapper, "Block");
    document.querySelector<HTMLInputElement>("#mise-block-0")!.click();
    await confirmDialog(wrapper, "policy");
    expect(blockPackageMock).toHaveBeenCalledTimes(2);
    const dialog = wrapper.findComponent(DestructiveConfirm);
    expect(dialog.props("open")).toBe(true);
    expect(dialog.props("error")).toContain("mv refused");
  });

  it("does not block when the dialog is dismissed", async () => {
    const { wrapper } = await mountComp([version()]);
    await press(wrapper, "Block");
    wrapper.findComponent(DestructiveConfirm).vm.$emit("update:open", false);
    await flushPromises();
    expect(blockPackageMock).not.toHaveBeenCalled();
  });

  /**
   * A block reason is read back by whoever hits the 403, so an empty one is
   * refused *in* the dialog rather than by closing it — the operator would
   * otherwise be looking at a dismissed dialog and an unblocked version, with
   * nothing said.
   */
  it("refuses an empty reason without closing the dialog", async () => {
    const { wrapper } = await mountComp([version()]);
    await press(wrapper, "Block");
    await confirmDialog(wrapper, "   ");

    expect(blockPackageMock).not.toHaveBeenCalled();
    const dialog = wrapper.findComponent(DestructiveConfirm);
    expect(dialog.props("open")).toBe(true);
    expect(dialog.props("error")).toContain("reason");
  });

  it("unblocks a blocked version", async () => {
    const { wrapper } = await mountComp([
      version({ status: { status: "blocked", blocked_at: "t", blocked_by: "admin", reason: "r" } }),
    ]);
    expect(wrapper.text()).toContain("Blocked");
    expect(wrapper.text()).toContain("r");
    const unblockBtn = wrapper.findAll("button").find((b) => b.text() === "Unblock")!;
    await unblockBtn.trigger("click");
    await flushPromises();
    expect(unblockPackageMock).toHaveBeenCalled();
    expect(wrapper.emitted("reload")).toHaveLength(1);
  });

  it("purges the cache once confirmed", async () => {
    const { wrapper } = await mountComp([version({ cached: true, cached_at: "2026-01-01" })]);
    expect(wrapper.text()).toContain("Cached");
    await press(wrapper, "Purge cache");
    expect(invalidatePackageMock, "the click alone must not purge").not.toHaveBeenCalled();

    await confirmDialog(wrapper);
    expect(invalidatePackageMock).toHaveBeenCalled();
    expect(wrapper.emitted("reload")).toHaveLength(1);
  });

  /* A purge is not irreversible — the bytes come back on the next download —
     and saying otherwise would be the loudest possible way to be wrong. What
     it costs is one upstream round trip, and that is what the dialog says. */
  it("states a purge as recoverable rather than permanent", async () => {
    const { wrapper } = await mountComp([version({ cached: true })]);
    await press(wrapper, "Purge cache");

    const dialog = wrapper.findComponent(DestructiveConfirm);
    expect(dialog.props("reversible")).toBe(true);
    expect(dialog.props("consequence") as string).toContain("re-fetches");
  });

  it("skips purge when the dialog is dismissed", async () => {
    const { wrapper } = await mountComp([version({ cached: true })]);
    await press(wrapper, "Purge cache");
    wrapper.findComponent(DestructiveConfirm).vm.$emit("update:open", false);
    await flushPromises();
    expect(invalidatePackageMock).not.toHaveBeenCalled();
  });

  it("selects all versions and bulk-blocks them", async () => {
    const { wrapper } = await mountComp([version(), version({ id: "v2", version: "2.0.0" })]);
    const selectAll = wrapper.find('input[aria-label="Select all versions"]');
    await selectAll.setValue(true);
    /* Real plural forms now, rather than "(s)" — vue-i18n picks the form. */
    expect(wrapper.text()).toContain("2 versions selected");

    await press(wrapper, "Block selected");
    expect(bulkBlockPackagesMock, "the click alone must not block").not.toHaveBeenCalled();
    await confirmDialog(wrapper, "bad license");

    expect(bulkBlockPackagesMock).toHaveBeenCalledWith({
      body: {
        items: [
          {
            registry: "npm",
            name: "pkg",
            version: "1.0.0",
            artifact: "pkg-1.0.0.tgz",
            reason: "bad license",
          },
          {
            registry: "npm",
            name: "pkg",
            version: "2.0.0",
            artifact: "pkg-1.0.0.tgz",
            reason: "bad license",
          },
        ],
      },
    });
    // The catalogue owns the whole sentence and pluralises the noun, so the
    // singular case reads "1 version" rather than the assembled "1 version(s)".
    expect((wrapper.vm as unknown as { bulkMsg: string }).bulkMsg).toBe("Blocked 1 version.");
    // Rendered, not only set: it lived in the bar the success just unmounted.
    expect(wrapper.find("output").text()).toBe("Blocked 1 version.");
    expect(wrapper.emitted("reload")).toHaveLength(1);
  });

  it("bulk-unblocks selected versions once confirmed", async () => {
    const { wrapper } = await mountComp([version()]);
    await wrapper.find(`input[aria-label="Select version 1.0.0"]`).setValue(true);
    await press(wrapper, "Unblock selected");
    expect(bulkUnblockPackagesMock, "the click alone must not unblock").not.toHaveBeenCalled();

    await confirmDialog(wrapper);
    expect(bulkUnblockPackagesMock).toHaveBeenCalled();
    expect((wrapper.vm as unknown as { bulkMsg: string }).bulkMsg).toBe("Unblocked 1 version.");
  });

  it("clears the selection via Clear", async () => {
    const { wrapper } = await mountComp([version()]);
    await wrapper.find(`input[aria-label="Select version 1.0.0"]`).setValue(true);
    const clearBtn = wrapper.findAll("button").find((b) => b.text() === "Clear")!;
    await clearBtn.trigger("click");
    expect(wrapper.text()).not.toContain("selected");
  });

  it("shows the declared licence", async () => {
    const { wrapper } = await mountComp([version({ license: "MIT OR Apache-2.0" })]);
    expect(wrapper.text()).toContain("MIT OR Apache-2.0");
  });

  /**
   * RFC 0004-bis §13.1: a null licence means the manifest was never read, not
   * that the package is unlicensed. Rendering nothing would make the two
   * indistinguishable — the §2.4 defect, a blank that reads as a fact — so the
   * absence is stated and the title says why.
   */
  it("states that an absent licence is unknown, not absent", async () => {
    const { wrapper } = await mountComp([version({ license: null })]);
    expect(wrapper.text()).toContain("licence unknown");
    const cell = wrapper.findAll("p").find((p) => p.text() === "licence unknown")!;
    expect(cell.attributes("title")).toContain("not the same as unlicensed");
  });
});
