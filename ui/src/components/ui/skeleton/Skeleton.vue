<script setup lang="ts">
/**
 * A loading placeholder shaped like the content it stands in for.
 *
 * Deliberately not a shimmer sweep, and no longer a pulse either: DESIGN's One
 * Motion Rule spends motion on an artifact resolving and on nothing else, and
 * a pulse was a second, unauthored one on every loading region. A dashed ruled
 * block at rest already reads as "not yet here" — dashed is the system's
 * at-rest line, solid its settled one.
 *
 * `aria-hidden`, because a skeleton has nothing to announce — the surface that
 * owns it announces the result through an Announcer when the data lands.
 */
import { type HTMLAttributes } from "vue";
import { cn } from "@/lib/utils";

const props = withDefaults(defineProps<{ lines?: number; class?: HTMLAttributes["class"] }>(), {
  lines: 3,
});

/** Ragged widths read as text; identical bars read as a loading graphic. */
const WIDTHS = ["100%", "92%", "96%", "78%"];
const widthFor = (index: number): string =>
  index === props.lines ? "62%" : WIDTHS[(index - 1) % WIDTHS.length];
</script>

<template>
  <div :class="cn('space-y-2', props.class)" aria-hidden="true" data-testid="skeleton">
    <div
      v-for="line in lines"
      :key="line"
      data-testid="skeleton-line"
      class="h-4 border border-dashed border-border bg-muted"
      :style="{ width: widthFor(line) }"
    />
  </div>
</template>
