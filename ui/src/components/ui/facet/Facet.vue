<script setup lang="ts">
/**
 * A single-select filter list — the registry sidebar, extracted from
 * PackageExplorer where it was inlined.
 *
 * Selection is carried by ink and a 1px lit edge, not by a fill. That is not a
 * style preference: at this palette's lightness a fill step measures ~1.06:1
 * against the ground and ~1.11:1 on paper, so it is invisible to a great many
 * people while looking fine to the author (see The Undependable Fill Rule in
 * DESIGN.md). The fill stays as a secondary cue only.
 */
import { type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";
import type { FacetOption } from "./types";

const props = withDefaults(
  defineProps<{
    /** `null` means "all" — rendered as its own option when `allLabel` is set. */
    modelValue: string | null;
    options: FacetOption[];
    label: string;
    allLabel?: string;
    class?: HTMLAttributes["class"];
  }>(),
  { allLabel: "" },
);

const emit = defineEmits<{ "update:modelValue": [string | null] }>();

const isActive = (value: string | null): boolean => props.modelValue === value;

/** Counts are numbers, so they get the reader's locale rather than raw digits. */
const formatCount = (count: number): string => new Intl.NumberFormat().format(count);

/**
 * One cell, two orientations — a rail from `md` up, a scrolling row below it.
 *
 * This is the design proof's own answer to the narrow viewport
 * (`@media (max-width:900px)`: `.bay ul{display:flex;overflow-x:auto}`,
 * `.bay a{white-space:nowrap;border-left:0;border-bottom:2px solid transparent}`).
 * The page had been hiding the bay outright below `md`, which left a phone with
 * no way to change registry at all: the filter existed, the control did not.
 *
 * The lit edge rotates with the layout — left border in the rail, bottom border
 * in the row — because in a horizontal strip a left border reads as a divider
 * between neighbours rather than as a mark on the chosen one. What does *not*
 * change is that selection rides on ink and a 1px edge: at this palette's
 * lightness a fill step measures ~1.06:1 against the ground, so `bg-ground-raised`
 * stays a secondary cue in both orientations (The Undependable Fill Rule).
 *
 * Held as one string rather than repeated on both buttons: the "all" option and
 * a registry option drifting apart is exactly the kind of difference nobody
 * notices until one of them stops looking selected.
 */
const CELL =
  "flex w-auto shrink-0 items-baseline gap-2 whitespace-nowrap border-b-2 px-3 py-1.5 text-left text-sm transition-colors md:w-full md:shrink md:whitespace-normal md:border-b-0 md:border-l";

function cellState(active: boolean): string {
  return active
    ? "border-b-primary bg-ground-raised font-semibold text-foreground md:border-l-primary"
    : "border-b-transparent text-muted-foreground hover:text-foreground md:border-l-transparent";
}
</script>

<template>
  <div :class="cn('font-mono', props.class)">
    <p
      :id="`facet-${label}`"
      class="px-3 pb-2 text-xs uppercase tracking-wider text-muted-foreground"
    >
      {{ label }}
    </p>
    <!-- The scroll container needs no `tabindex`: every child is a `<button>`,
         so it is already reachable and scrollable by keyboard, which is what
         axe's `scrollable-region-focusable` actually asks for. Adding one would
         put an extra unnamed stop in the tab order for no gain. -->
    <ul
      class="m-0 flex list-none gap-1 overflow-x-auto p-0 md:block md:gap-0 md:overflow-x-visible"
      :aria-labelledby="`facet-${label}`"
    >
      <li v-if="allLabel" class="shrink-0 md:shrink">
        <button
          type="button"
          :class="[CELL, cellState(isActive(null))]"
          :aria-pressed="isActive(null)"
          @click="emit('update:modelValue', null)"
        >
          {{ allLabel }}
        </button>
      </li>
      <li v-for="option in options" :key="option.value" class="shrink-0 md:shrink">
        <button
          type="button"
          :class="[CELL, cellState(isActive(option.value))]"
          :aria-pressed="isActive(option.value)"
          @click="emit('update:modelValue', option.value)"
        >
          <span class="md:truncate">{{ option.label }}</span>
          <!-- `ml-auto` right-aligns the count against the rail's edge, which
               is what makes the column of numbers scannable. In the row there
               is no edge to align to — every cell is content-width — so it
               becomes a plain gap after the name, as the proof sets it
               (`.bay .ct{margin-left:var(--s2)}`). -->
          <span
            v-if="option.count !== undefined"
            class="ml-2 text-xs tabular-nums text-muted-foreground md:ml-auto"
          >
            {{ formatCount(option.count) }}
          </span>
        </button>
      </li>
    </ul>
  </div>
</template>
