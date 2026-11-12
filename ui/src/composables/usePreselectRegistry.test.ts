import { describe, expect, it, vi } from "vitest";
import { nextTick, ref } from "vue";

const query: Record<string, unknown> = {};
vi.mock("vue-router", () => ({ useRoute: () => ({ query }) }));

import { usePreselectRegistry } from "./usePreselectRegistry";

/**
 * RFC 0046 §4.2 and §6.3, shared by the upstream, warming, imports, health
 * and findings pages: one implementation, so one test of each rule.
 */
describe("usePreselectRegistry", () => {
  function setup(wanted: string | undefined) {
    query.registry = wanted;
    const names = ref<string[]>([]);
    const settled = ref(false);
    const apply = vi.fn();
    usePreselectRegistry(
      () => names.value,
      () => settled.value,
      apply,
    );
    return { names, settled, apply };
  }

  it("holds the value until the list settles, then applies it once", async () => {
    const { names, settled, apply } = setup("npm");
    await nextTick();
    expect(apply).not.toHaveBeenCalled();
    names.value = ["npm", "pypi"];
    settled.value = true;
    await nextTick();
    expect(apply).toHaveBeenCalledWith("npm");
    settled.value = false;
    await nextTick();
    settled.value = true;
    await nextTick();
    expect(apply).toHaveBeenCalledTimes(1);
  });

  it("drops a name the settled list lacks, and a path-shaped one (T5)", async () => {
    for (const wanted of ["nope", "../x", "npm/../../etc"]) {
      const { names, settled, apply } = setup(wanted);
      names.value = ["npm"];
      settled.value = true;
      await nextTick();
      expect(apply, wanted).not.toHaveBeenCalled();
    }
  });

  it("keeps waiting while the list has failed (it never settles)", async () => {
    const { apply } = setup("npm");
    await nextTick();
    expect(apply).not.toHaveBeenCalled();
  });

  it("does nothing without the query", async () => {
    const { names, settled, apply } = setup(undefined);
    names.value = ["npm"];
    settled.value = true;
    await nextTick();
    expect(apply).not.toHaveBeenCalled();
  });
});
