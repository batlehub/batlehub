import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { deleteCachedArtifactMock } = vi.hoisted(() => ({ deleteCachedArtifactMock: vi.fn() }));
vi.mock("@/client/sdk.gen", () => ({ deleteCachedArtifact: deleteCachedArtifactMock }));

import DeleteCachedArtifact from "./DeleteCachedArtifact.vue";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";

/**
 * Every wrapper mounted, so teardown can unmount them.
 *
 * The confirm dialog teleports to `document.body`, and without this the
 * previous test's dialog was still there: a probe found two live "Delete"
 * buttons at once, so selecting one by label reached a *stale* dialog whose
 * click did nothing.
 */
const mounted: ReturnType<typeof mount>[] = [];

function mountComp(registries = ["npm", "pypi"]) {
  const wrapper = mount(DeleteCachedArtifact, {
    props: { registries },
    // radix `Select`, stubbed native so the registry can be picked with `setValue`.
    global: {
      stubs: {
        Select: {
          props: ["modelValue", "options", "id"],
          emits: ["update:modelValue"],
          template: `<select :id="id" :value="modelValue" @change="$emit('update:modelValue', $event.target.value)">
            <option v-for="o in options" :key="o.value" :value="o.value">{{ o.label }}</option>
          </select>`,
        },
      },
    },
  });
  mounted.push(wrapper);
  return wrapper;
}

type Comp = ReturnType<typeof mountComp>;

const deleteButton = (w: Comp) =>
  w.findAll("button").find((b) => /^(Delete|Deleting)/.test(b.text()))!;

/** Fill the form the way an operator would: pick a registry, then the coordinate. */
async function fillPackage(w: Comp, registry = "npm", name = "lodash", version = "4.17.21") {
  await w.get("#del-registry").setValue(registry);
  await w.get("#del-name").setValue(name);
  await w.get("#del-version").setValue(version);
}

/**
 * Click the dialog's real confirm button rather than emitting `confirm` on the
 * component: emitting calls the parent handler directly, so `canConfirm` — the
 * thing that gates `:disabled` on this very button — never runs, and every
 * assertion about the gate checks only that a prop was passed.
 *
 * By label, not by position: the dialog renders Cancel, the action, then a
 * trailing "Close", so `.at(-1)` silently clicks a button that does nothing.
 */
function confirmButton(action: string): HTMLButtonElement {
  const buttons = [...document.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')];
  const button = buttons.find((b) => b.textContent?.trim().replace(/…$/, "") === action);
  expect(button, `no "${action}" button in the open dialog`).toBeTruthy();
  return button!;
}

async function confirmDialog(w: Comp) {
  const dialog = w.findComponent(DestructiveConfirm);
  expect(dialog.props("open")).toBe(true);
  const button = confirmButton(dialog.props("action") as string);
  expect(button.disabled, "the dialog's own gate refused the confirmation").toBe(false);
  button.click();
  await flushPromises();
}

/**
 * The component's whole reason for existing (see its doc comment) is that the
 * registry is *chosen* from the validated list and the deletion is confirmed
 * with its scope named — so those are what the tests hold onto.
 */
describe("DeleteCachedArtifact", () => {
  beforeEach(() => {
    deleteCachedArtifactMock
      .mockReset()
      .mockResolvedValue({ data: { deleted: true, artifact_key: "npm/lodash/4.17.21" } });
  });

  // Unmount before clearing the body: a teleported dialog whose nodes are
  // already gone makes Vue walk a detached fragment and throw.
  afterEach(() => {
    for (const wrapper of mounted.splice(0)) wrapper.unmount();
    document.body.innerHTML = "";
  });

  it("offers the registries it was given rather than a free-text field", () => {
    const wrapper = mountComp(["npm", "pypi", "maven"]);
    const options = wrapper.findAll("#del-registry option").map((o) => o.text());
    expect(options).toEqual(["Choose a registry…", "npm", "pypi", "maven"]);
    expect(wrapper.find("#del-registry").exists()).toBe(true);
  });

  it("keeps Delete disabled until a registry and a full coordinate are given", async () => {
    const wrapper = mountComp();
    expect(deleteButton(wrapper).attributes("disabled")).toBeDefined();

    await wrapper.get("#del-registry").setValue("npm");
    expect(deleteButton(wrapper).attributes("disabled")).toBeDefined();

    await wrapper.get("#del-name").setValue("lodash");
    expect(deleteButton(wrapper).attributes("disabled")).toBeDefined();

    await wrapper.get("#del-version").setValue("4.17.21");
    expect(deleteButton(wrapper).attributes("disabled")).toBeUndefined();
  });

  it("swaps the coordinate fields for a single path in path mode", async () => {
    const wrapper = mountComp();
    await wrapper.get("#del-mode").setValue("path");

    expect(wrapper.find("#del-name").exists()).toBe(false);
    expect(wrapper.find("#del-path").exists()).toBe(true);

    await wrapper.get("#del-registry").setValue("npm");
    await wrapper.get("#del-path").setValue("  lodash/-/lodash-4.17.21.tgz  ");
    expect(deleteButton(wrapper).attributes("disabled")).toBeUndefined();

    // The dialog names what is about to go, trimmed of the typing.
    await deleteButton(wrapper).trigger("click");
    expect(wrapper.findComponent(DestructiveConfirm).props("scope")).toBe(
      "npm · lodash/-/lodash-4.17.21.tgz",
    );
  });

  it("names registry and coordinate in the confirmation, then deletes it", async () => {
    const wrapper = mountComp();
    await fillPackage(wrapper);
    await deleteButton(wrapper).trigger("click");

    const dialog = wrapper.findComponent(DestructiveConfirm);
    expect(dialog.props("scope")).toBe("npm · lodash@4.17.21");
    expect(dialog.props("count")).toBe(1);

    await confirmDialog(wrapper);

    expect(deleteCachedArtifactMock).toHaveBeenCalledWith({
      path: { registry: "npm" },
      body: { name: "lodash", version: "4.17.21" },
    });
    expect(wrapper.text()).toContain("Deleted npm/lodash/4.17.21.");
  });

  it("sends a path body in path mode", async () => {
    const wrapper = mountComp();
    await wrapper.get("#del-mode").setValue("path");
    await wrapper.get("#del-registry").setValue("pypi");
    await wrapper.get("#del-path").setValue("packages/x/x-1.0.tar.gz");
    await deleteButton(wrapper).trigger("click");
    await confirmDialog(wrapper);

    expect(deleteCachedArtifactMock).toHaveBeenCalledWith({
      path: { registry: "pypi" },
      body: { path: "packages/x/x-1.0.tar.gz" },
    });
  });

  /**
   * "Nothing was there" and "the delete failed" are different answers, and an
   * operator who reads the first as the second re-runs a destructive action.
   */
  it("distinguishes a miss from a deletion", async () => {
    deleteCachedArtifactMock.mockResolvedValue({ data: { deleted: false } });
    const wrapper = mountComp();
    await fillPackage(wrapper);
    await deleteButton(wrapper).trigger("click");
    await confirmDialog(wrapper);

    expect(wrapper.text()).toContain("That artifact was not in the cache.");
  });

  it("surfaces the API error instead of claiming success", async () => {
    deleteCachedArtifactMock.mockResolvedValue({ error: { message: "storage offline" } });
    const wrapper = mountComp();
    await fillPackage(wrapper);
    await deleteButton(wrapper).trigger("click");
    await confirmDialog(wrapper);

    expect(wrapper.text()).toContain("storage offline");
    expect(wrapper.text()).not.toContain("Deleted");
  });

  it("shows progress while the delete is in flight and closes the dialog first", async () => {
    let release!: (v: unknown) => void;
    deleteCachedArtifactMock.mockReturnValue(
      new Promise((resolve) => {
        release = resolve;
      }),
    );
    const wrapper = mountComp();
    await fillPackage(wrapper);
    await deleteButton(wrapper).trigger("click");

    wrapper.findComponent(DestructiveConfirm).vm.$emit("confirm");
    await flushPromises();

    expect(wrapper.findComponent(DestructiveConfirm).props("open")).toBe(false);
    expect(deleteButton(wrapper).text()).toBe("Deleting…");
    expect(deleteButton(wrapper).attributes("disabled")).toBeDefined();

    release({ data: { deleted: true, artifact_key: "npm/lodash/4.17.21" } });
    await flushPromises();
    expect(deleteButton(wrapper).text()).toBe("Delete");
  });
});
