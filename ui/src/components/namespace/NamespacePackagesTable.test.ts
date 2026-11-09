import { mount, flushPromises } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { NamespacePackageDto, TeamNamespaceDto } from "@/client/types.gen";

const { listMock, setVisMock } = vi.hoisted(() => ({
  listMock: vi.fn(),
  setVisMock: vi.fn(),
}));
vi.mock("@/client/sdk.gen", () => ({
  myNamespacePackages: listMock,
  setPackageVisibility: setVisMock,
}));
vi.mock("@/composables/useAuth", async () => {
  const { ref } = await import("vue");
  const token = ref("t");
  return { useAuth: () => ({ token }) };
});

import NamespacePackagesTable from "./NamespacePackagesTable.vue";

const NS = { registry: "npm", prefix: "@acme", group_id: "team-a" } as TeamNamespaceDto;

const pkg = (name: string, extra: Partial<NamespacePackageDto> = {}) =>
  ({
    name,
    version: "1.0.0",
    visibility: "public",
    published_by: "alice",
    published_at: "2026-11-01T10:00:00Z",
    yanked: false,
    ...extra,
  }) as NamespacePackageDto;

/** The radix select only opens on pointer events jsdom does not lay out. */
const stubs = {
  Select: {
    props: ["modelValue", "options"],
    emits: ["update:modelValue"],
    template: `<select :value="modelValue" @change="$emit('update:modelValue', $event.target.value)">
      <option v-for="o in options" :key="o.value" :value="o.value">{{ o.label }}</option>
    </select>`,
  },
};

async function mountTable(rows: NamespacePackageDto[] | { error: unknown }) {
  listMock.mockResolvedValue(Array.isArray(rows) ? { data: rows } : rows);
  const wrapper = mount(NamespacePackagesTable, { props: { namespace: NS }, global: { stubs } });
  await flushPromises();
  return wrapper;
}

const button = (wrapper: Awaited<ReturnType<typeof mountTable>>, text: string) =>
  wrapper.findAll("button").find((b) => b.text() === text)!;

beforeEach(() => {
  listMock.mockReset();
  setVisMock.mockReset();
});

describe("NamespacePackagesTable", () => {
  it("shows Loading… until the list answers", async () => {
    listMock.mockReturnValue(new Promise(() => {}));
    const wrapper = mount(NamespacePackagesTable, { props: { namespace: NS }, global: { stubs } });
    await flushPromises();
    expect(wrapper.text()).toContain("Loading…");
  });

  it("shows the API's error", async () => {
    const wrapper = await mountTable({ error: { message: "namespace not yours" } });
    expect(wrapper.text()).toContain("namespace not yours");
  });

  it("says when nothing is published yet", async () => {
    const wrapper = await mountTable([]);
    expect(wrapper.text()).toContain("Nothing published here yet");
  });

  it("lists the packages, marking a yanked one and each visibility", async () => {
    const wrapper = await mountTable([
      pkg("@acme/a"),
      pkg("@acme/b", { yanked: true, visibility: "internal" }),
      pkg("@acme/c", { visibility: "team" }),
      pkg("@acme/d", { visibility: "private" as NamespacePackageDto["visibility"] }),
    ]);
    const rows = wrapper.findAll("tbody tr");
    expect(rows).toHaveLength(4);
    expect(rows[1].text()).toContain("(yanked)");
    expect(rows[1].classes()).toContain("opacity-50");
    expect(rows[0].classes()).not.toContain("opacity-50");
    expect(rows[2].html()).toContain("border-primary");
    expect(listMock).toHaveBeenCalledWith(
      expect.objectContaining({
        path: { registry: "npm", prefix: "@acme" },
        query: { page: 0, per_page: 50 },
      }),
    );
  });

  it("saves a new visibility", async () => {
    setVisMock.mockResolvedValue({ data: {} });
    const wrapper = await mountTable([pkg("@acme/a")]);
    await button(wrapper, "Edit visibility").trigger("click");
    await wrapper.find("select").setValue("team");
    await button(wrapper, "Save").trigger("click");
    await flushPromises();

    expect(setVisMock).toHaveBeenCalledWith({
      path: { registry: "npm", name: "@acme/a" },
      body: { visibility: "team" },
    });
    expect(wrapper.find("select").exists()).toBe(false);
    expect(wrapper.find("tbody").text()).toContain("team");
  });

  it("keeps the editor open with the error when the save is refused", async () => {
    setVisMock.mockResolvedValue({ error: { message: "forbidden" } });
    const wrapper = await mountTable([pkg("@acme/a")]);
    await button(wrapper, "Edit visibility").trigger("click");
    await button(wrapper, "Save").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("forbidden");
    expect(wrapper.find("select").exists()).toBe(true);
  });

  it("falls back to a generic message for an error without one", async () => {
    setVisMock.mockResolvedValue({ error: {} });
    const wrapper = await mountTable([pkg("@acme/a")]);
    await button(wrapper, "Edit visibility").trigger("click");
    await button(wrapper, "Save").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("API error");
  });

  it("names an unknown failure", async () => {
    setVisMock.mockRejectedValue("network down");
    const wrapper = await mountTable([pkg("@acme/a")]);
    await button(wrapper, "Edit visibility").trigger("click");
    await button(wrapper, "Save").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("Unknown error");
  });

  it("disables Save while the request is in flight", async () => {
    setVisMock.mockReturnValue(new Promise(() => {}));
    const wrapper = await mountTable([pkg("@acme/a")]);
    await button(wrapper, "Edit visibility").trigger("click");
    await button(wrapper, "Save").trigger("click");
    await flushPromises();
    expect(button(wrapper, "…").attributes("disabled")).toBeDefined();
  });

  it("cancels an edit, and the badge opens one too", async () => {
    const wrapper = await mountTable([pkg("@acme/a")]);
    await button(wrapper, "Edit visibility").trigger("click");
    await button(wrapper, "Cancel").trigger("click");
    expect(wrapper.find("select").exists()).toBe(false);

    await wrapper.find("tbody .capitalize").trigger("click");
    expect(wrapper.find("select").exists()).toBe(true);
  });

  it("pages forward when a page is full", async () => {
    const full = Array.from({ length: 50 }, (_, i) => pkg(`@acme/p${i}`));
    const wrapper = await mountTable(full);
    expect(wrapper.findComponent({ name: "Pagination" }).props("hasNext")).toBe(true);
    wrapper.findComponent({ name: "Pagination" }).vm.$emit("update:page", 1);
    await flushPromises();
    expect(listMock).toHaveBeenLastCalledWith(
      expect.objectContaining({ query: { page: 1, per_page: 50 } }),
    );
  });

  it("goes back to the first page when the namespace changes", async () => {
    const full = Array.from({ length: 50 }, (_, i) => pkg(`@acme/p${i}`));
    const wrapper = await mountTable(full);
    wrapper.findComponent({ name: "Pagination" }).vm.$emit("update:page", 2);
    await flushPromises();
    await wrapper.setProps({ namespace: { ...NS, prefix: "@other" } });
    await flushPromises();
    expect(listMock).toHaveBeenLastCalledWith(
      expect.objectContaining({
        path: { registry: "npm", prefix: "@other" },
        query: { page: 0, per_page: 50 },
      }),
    );
  });
});
