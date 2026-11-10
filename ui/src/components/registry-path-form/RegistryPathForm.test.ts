import { describe, it, expect } from "vitest";
import { mount } from "@vue/test-utils";
import RegistryPathForm from "./RegistryPathForm.vue";
import { REGISTRY_PATH_TYPES } from "@/config/registryPathFields";

function mountForm(
  typeId: string,
  values: Record<string, string> = {},
  registries: { name: string; type: string }[] = [],
) {
  const typeDef = REGISTRY_PATH_TYPES.find((t) => t.id === typeId)!;
  const allValues = Object.fromEntries(
    typeDef.fields.map((f) => [f.key, values[f.key] ?? f.default ?? ""]),
  );
  return mount(RegistryPathForm, {
    props: {
      typeDef,
      registries,
      registryName: typeDef.id,
      values: allValues,
      "onUpdate:registryName": () => {},
      "onUpdate:values": () => {},
    },
  });
}

describe("RegistryPathForm", () => {
  it("renders a registry-name input and one input per field", () => {
    const wrapper = mountForm("npm");
    expect(wrapper.find("#npm-registry").exists()).toBe(true);
    expect(wrapper.find("#npm-package").exists()).toBe(true);
    expect(wrapper.find("#npm-version").exists()).toBe(true);
  });

  it("groups fields sharing a row number into one grid container", () => {
    const wrapper = mountForm("github");
    // owner+repo share row 1, ref+assetId share row 2 -> two sm:grid-cols-2 rows.
    const grids = wrapper.findAll(".sm\\:grid-cols-2");
    expect(grids).toHaveLength(2);
  });

  it("renders a 3-column grid for terraform's namespace/name/provider row", () => {
    const wrapper = mountForm("terraform");
    expect(wrapper.findAll(".sm\\:grid-cols-3")).toHaveLength(2);
  });

  it("renders the suffix text next to the label", () => {
    const wrapper = mountForm("npm");
    expect(wrapper.text()).toContain("(optional)");
  });

  /**
   * The note's markup is rendered, not pasted.
   *
   * `find("code").exists()` alone cannot tell this apart from the `v-html` it
   * replaced — it was true both before and after — so it also asserts what only
   * the new path guarantees: the `<code>` carries the class `RichText` owns
   * rather than one written into the data, and no markup reaches the reader as
   * visible text.
   */
  it("renders the note's markup rather than pasting it", () => {
    const wrapper = mountForm("maven");
    const code = wrapper.find("code");
    expect(code.exists()).toBe(true);
    expect(code.text()).toBe("com.google.guava");
    expect(code.classes()).toContain("font-mono");
    expect(wrapper.text()).not.toContain("<code>");
  });

  /**
   * A real listbox since RFC 0004-bis §6.2. The `<datalist>` this replaced
   * exposed nothing to assistive tech, behaved differently per browser, and
   * could not say "nothing matches" — which is the one behaviour the field
   * needed and the reason the sweep exists.
   */
  it("offers known registry names as listbox options", async () => {
    const wrapper = mountForm("npm", {}, [
      { name: "npm-mirror", type: "npm" },
      { name: "other", type: "npm" },
    ]);
    await wrapper.find('input[role="combobox"]').trigger("focus");

    const options = wrapper.findAll('[role="option"]');
    expect(options.map((o) => o.text())).toEqual(["npm-mirrornpm", "othernpm"]);
  });

  it("updates the bound values object when a field is edited", async () => {
    const typeDef = REGISTRY_PATH_TYPES.find((t) => t.id === "npm")!;
    const values = { package: "", version: "" };
    const wrapper = mount(RegistryPathForm, {
      props: {
        typeDef,
        registries: [],
        registryName: "npm",
        values,
        "onUpdate:registryName": () => {},
        "onUpdate:values": (v: Record<string, string>) => wrapper.setProps({ values: v }),
      },
    });
    await wrapper.find("#npm-package").setValue("left-pad");
    expect(wrapper.props("values").package).toBe("left-pad");
  });
});
