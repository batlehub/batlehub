import { mount, flushPromises } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { listGrantsMock, putGrantMock, deleteGrantMock } = vi.hoisted(() => ({
  listGrantsMock: vi.fn(),
  putGrantMock: vi.fn(),
  deleteGrantMock: vi.fn(),
}));

vi.mock("@/client/sdk.gen", () => ({
  listGrants: listGrantsMock,
  putGrant: putGrantMock,
  deleteGrant: deleteGrantMock,
}));

vi.mock("@/composables/useAuth", () => ({
  useAuth: () => ({ token: { value: "t" }, identity: { value: { role: "admin" } } }),
}));

import PackageGrants from "./PackageGrants.vue";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";

let active: ReturnType<typeof mount> | null = null;

afterEach(() => {
  active?.unmount();
  active = null;
  document.body.innerHTML = "";
});

const OWNERSHIP_ROW = {
  node_kind: "package",
  node_key: "pkg",
  subject: "user:alice",
  actions: ["releases:publish", "owners:read", "owners:write"],
  granted_by: null,
  from_ownership: true,
};

const EDITOR_ROW = {
  node_kind: "package",
  node_key: "pkg",
  subject: "group:oidc1:eng",
  actions: ["releases:read"],
  granted_by: "root",
  from_ownership: false,
};

const VERSION_ROW = {
  node_kind: "version",
  node_key: "pkg@2.4.0-rc.1",
  subject: "group:oidc1:release-managers",
  actions: ["releases:read"],
  granted_by: "root",
  from_ownership: false,
};

async function mountPanel(grants: unknown[] = [EDITOR_ROW]) {
  listGrantsMock.mockResolvedValue({ data: { grants } });
  const wrapper = mount(PackageGrants, {
    attachTo: document.body,
    props: { registry: "reg", name: "pkg" },
  });
  await flushPromises();
  active = wrapper;
  return wrapper;
}

const buttonIn = (root: ParentNode, label: string) =>
  [...root.querySelectorAll("button")].find((b) => b.textContent?.trim() === label);

describe("PackageGrants", () => {
  beforeEach(() => {
    putGrantMock
      .mockReset()
      .mockResolvedValue({ data: { actions: ["releases:read"], warnings: [] } });
    deleteGrantMock.mockReset().mockResolvedValue({ data: { removed: true } });
  });

  it("lists the package-tier grants", async () => {
    const wrapper = await mountPanel();
    expect(wrapper.text()).toContain("group:oidc1:eng");
    expect(wrapper.text()).toContain("releases:read");
  });

  /**
   * An ownership row is the projection's, and the server answers `409` for both
   * an edit and a removal (RFC 0017 §4.3). Offering the controls anyway would
   * let an operator fill in a form and be refused after the fact.
   */
  it("offers no edit or remove control on an ownership row", async () => {
    const wrapper = await mountPanel([OWNERSHIP_ROW]);
    expect(wrapper.text()).toContain("From ownership");
    expect(
      wrapper.findAll("button").filter((b) => b.attributes("aria-label")?.includes("user:alice")),
    ).toHaveLength(0);
  });

  it("still offers them on an editor row", async () => {
    const wrapper = await mountPanel([EDITOR_ROW]);
    expect(
      wrapper
        .findAll("button")
        .filter((b) => b.attributes("aria-label")?.includes("group:oidc1:eng")).length,
    ).toBeGreaterThan(0);
  });

  /**
   * §11 open question 3 defers the version tier's *editor*, not its display:
   * hiding the rows would answer "who can reach this package" with half of them.
   */
  it("shows version-tier rows read-only, and says where to edit them", async () => {
    const wrapper = await mountPanel([EDITOR_ROW, VERSION_ROW]);
    expect(wrapper.text()).toContain("2.4.0-rc.1");
    expect(wrapper.text()).toContain("group:oidc1:release-managers");
    expect(wrapper.text()).toContain("batlehub-cli grant set");
  });

  it("writes a grant through the editor", async () => {
    const wrapper = await mountPanel([]);
    buttonIn(wrapper.element as ParentNode, "Add a grant")!.click();
    await flushPromises();

    const dialog = document.querySelector('[role="dialog"]') as ParentNode;
    const subject = dialog.querySelector("#grant-subject") as HTMLInputElement;
    subject.value = "group:oidc1:qa";
    subject.dispatchEvent(new Event("input"));
    const actions = dialog.querySelector("#grant-actions") as HTMLInputElement;
    actions.value = "releases:read, releases:list";
    actions.dispatchEvent(new Event("input"));
    await flushPromises();

    buttonIn(dialog, "Save")!.click();
    await flushPromises();

    expect(putGrantMock).toHaveBeenCalledWith(
      expect.objectContaining({
        body: expect.objectContaining({
          package: "pkg",
          subject: "group:oidc1:qa",
          actions: ["releases:read", "releases:list"],
        }),
      }),
    );
  });

  /** The server's expansion, not the form's text: `releases:*` stores several. */
  it("announces what was stored rather than what was typed", async () => {
    putGrantMock.mockResolvedValue({
      data: { actions: ["releases:read", "releases:list", "releases:publish"], warnings: [] },
    });
    const wrapper = await mountPanel([]);
    buttonIn(wrapper.element as ParentNode, "Add a grant")!.click();
    await flushPromises();

    const dialog = document.querySelector('[role="dialog"]') as ParentNode;
    for (const [id, value] of [
      ["#grant-subject", "user:bob"],
      ["#grant-actions", "releases:*"],
    ]) {
      const el = dialog.querySelector(id) as HTMLInputElement;
      el.value = value;
      el.dispatchEvent(new Event("input"));
    }
    await flushPromises();
    buttonIn(dialog, "Save")!.click();
    await flushPromises();

    const live = document.querySelector("[aria-live]");
    expect(live?.textContent).toContain("releases:publish");
  });

  /** A legal-but-inert grant is reported, not swallowed. */
  it("surfaces the server's warnings", async () => {
    putGrantMock.mockResolvedValue({
      data: { actions: ["releases:read"], warnings: ["subject already holds releases:read"] },
    });
    const wrapper = await mountPanel([]);
    buttonIn(wrapper.element as ParentNode, "Add a grant")!.click();
    await flushPromises();

    const dialog = document.querySelector('[role="dialog"]') as ParentNode;
    for (const [id, value] of [
      ["#grant-subject", "user:bob"],
      ["#grant-actions", "releases:read"],
    ]) {
      const el = dialog.querySelector(id) as HTMLInputElement;
      el.value = value;
      el.dispatchEvent(new Event("input"));
    }
    await flushPromises();
    buttonIn(dialog, "Save")!.click();
    await flushPromises();

    expect(wrapper.text()).toContain("already holds");
  });

  it("confirms before removing, then removes", async () => {
    const wrapper = await mountPanel([EDITOR_ROW]);
    const bin = wrapper
      .findAll("button")
      .find((b) => b.attributes("aria-label")?.startsWith("Remove the grant"))!;
    await bin.trigger("click");
    await flushPromises();

    expect(deleteGrantMock).not.toHaveBeenCalled();
    const dialog = wrapper.findComponent(DestructiveConfirm);
    expect(dialog.props("open")).toBe(true);
    expect(dialog.props("scope")).toBe("group:oidc1:eng");

    dialog.vm.$emit("confirm");
    await flushPromises();
    expect(deleteGrantMock).toHaveBeenCalledWith(
      expect.objectContaining({
        body: expect.objectContaining({ package: "pkg", subject: "group:oidc1:eng" }),
      }),
    );
  });
});
