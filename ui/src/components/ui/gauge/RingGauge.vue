<script setup lang="ts">
/**
 * A bounded quantity against its limit, drawn as a ring — `Meter`'s twin for a
 * place where several sit side by side (the home page's limits).
 *
 * Same contract as `Meter`: `role="meter"`, named by its visible label through
 * `aria-labelledby`, the raw numbers in `aria-value*`, and the sentence a
 * sighted reader gets in `aria-valuetext`. The ring is not the state channel:
 * the caller writes the numbers and the state word beside it, and the hue only
 * confirms them (DESIGN.md, the Undependable Fill Rule).
 */
import { computed, useId } from "vue";
import type { MeterState } from "@/components/ui/meter";

const props = withDefaults(
  defineProps<{
    value: number;
    /** The limit. 0 or less renders an empty ring. */
    max: number;
    /** Visible label under the ring, and its accessible name. */
    label: string;
    /** Announced value, e.g. `"540 of 600 requests per minute"`. */
    valueText: string;
    /** Short text in the ring's centre, e.g. `"90%"` or `"7/12"`. */
    center: string;
    state?: MeterState;
  }>(),
  { state: "ok" },
);

const labelId = `gauge-label-${useId()}`;
const R = 25;
const C = 2 * Math.PI * R;

/** Clamped: usage past the limit is possible under `warn`, and `at-limit` says it. */
const percent = computed(() => {
  if (!Number.isFinite(props.max) || props.max <= 0) return 0;
  return Math.min(100, Math.max(0, (props.value / props.max) * 100));
});

const strokeClass = computed(
  () =>
    ({
      ok: "stroke-muted-foreground",
      warning: "stroke-copper",
      "at-limit": "stroke-destructive",
    })[props.state],
);
</script>

<template>
  <div
    class="grid min-w-0 content-start justify-items-center gap-1 text-center"
    :data-state="state"
  >
    <svg
      viewBox="0 0 64 64"
      class="h-16 w-16"
      role="meter"
      :aria-labelledby="labelId"
      :aria-valuenow="value"
      aria-valuemin="0"
      :aria-valuemax="max"
      :aria-valuetext="valueText"
      :data-state="state"
    >
      <circle cx="32" cy="32" :r="R" fill="none" stroke-width="5" class="stroke-rule-soft" />
      <circle
        cx="32"
        cy="32"
        :r="R"
        fill="none"
        stroke-width="5"
        :class="strokeClass"
        :stroke-dasharray="`${(C * percent) / 100} ${C}`"
        transform="rotate(-90 32 32)"
      />
      <text
        x="32"
        y="36"
        text-anchor="middle"
        class="font-mono text-xs tabular-nums"
        :class="state === 'ok' ? 'fill-muted-foreground' : 'fill-foreground'"
        aria-hidden="true"
      >
        {{ center }}
      </text>
    </svg>
    <span
      :id="labelId"
      class="font-mono text-xs [overflow-wrap:anywhere]"
      :class="state === 'ok' ? 'text-muted-foreground' : 'text-foreground'"
      >{{ label }}</span
    >
    <slot />
  </div>
</template>
