import { computed, ref, toValue, type MaybeRefOrGetter } from "vue";

/**
 * A checkbox selection over table rows, kept as a set of row keys.
 *
 * "Select all" acts on the rows shown (`rows`): it adds them all, or, when
 * they are all already in, removes them, and leaves selected rows that a
 * filter currently hides alone.
 */
export function useRowSelection<T>(rows: MaybeRefOrGetter<T[]>, key: (row: T) => string) {
  const selected = ref<Set<string>>(new Set());

  const allSelected = computed(() => {
    const shown = toValue(rows);
    return shown.length > 0 && shown.every((r) => selected.value.has(key(r)));
  });

  function toggleAll() {
    const next = new Set(selected.value);
    const all = allSelected.value;
    for (const r of toValue(rows)) {
      if (all) next.delete(key(r));
      else next.add(key(r));
    }
    selected.value = next;
  }

  function toggle(row: T) {
    const next = new Set(selected.value);
    const k = key(row);
    if (next.has(k)) next.delete(k);
    else next.add(k);
    selected.value = next;
  }

  function clear() {
    selected.value = new Set();
  }

  const isSelected = (row: T) => selected.value.has(key(row));

  return { selected, allSelected, toggleAll, toggle, clear, isSelected };
}
