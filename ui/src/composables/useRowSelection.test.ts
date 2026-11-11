import { describe, expect, it } from "vitest";
import { ref } from "vue";
import { useRowSelection } from "./useRowSelection";

describe("useRowSelection", () => {
  it("toggles one row, then all shown rows, keeping hidden selections", () => {
    const shown = ref(["a", "b"]);
    const s = useRowSelection(shown, (r) => r);
    s.toggle("z"); // selected, then hidden by a filter
    s.toggle("a");
    expect(s.allSelected.value).toBe(false);
    s.toggleAll();
    expect([...s.selected.value].sort()).toEqual(["a", "b", "z"]);
    expect(s.allSelected.value).toBe(true);
    s.toggleAll();
    expect([...s.selected.value]).toEqual(["z"]);
    s.clear();
    expect(s.selected.value.size).toBe(0);
  });

  it("is never 'all selected' over no rows", () => {
    expect(useRowSelection([] as string[], (r) => r).allSelected.value).toBe(false);
  });
});
