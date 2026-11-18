import { watchEffect } from "vue";
import { useStorage } from "@vueuse/core";

/**
 * Table density: `comfortable` (the default) or `compact`.
 *
 * A display preference like the theme, kept per browser under
 * `batlehub.density` and applied as `data-density` on `<html>`, which
 * `assets/index.css` reads to tighten table rows. `build/theme-init.js` applies
 * the stored value before the first paint, so this only has to keep the
 * attribute in step with later changes — and must resolve the same way: only
 * `compact` writes the attribute.
 */
export type Density = "comfortable" | "compact";

const density = useStorage<Density>("batlehub.density", "comfortable");

watchEffect(() => {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  if (density.value === "compact") root.dataset.density = "compact";
  else delete root.dataset.density;
});

export function useDensity() {
  return { density };
}
