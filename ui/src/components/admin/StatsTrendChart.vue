<script setup lang="ts">
/**
 * One series as a row of bars, oldest first (RFC 0042 §4.5, Activity tab).
 *
 * `null` is a gap — an hour with no traffic is not an hour at zero, so it is
 * drawn as nothing rather than as a flat bar. The numbers are in the
 * accessible label; the bars are decoration for a sighted reader.
 */
import { computed } from "vue";

const props = defineProps<{
  label: string;
  values: (number | null)[];
  /** The top of the scale; the series' own maximum when absent. */
  max?: number;
  /** How a value reads in the label and in each bar's tooltip. */
  format?: (v: number) => string;
}>();

const top = computed(
  () => props.max ?? Math.max(0, ...props.values.filter((v): v is number => v != null)),
);
const fmt = (v: number) => (props.format ? props.format(v) : String(v));
const summary = computed(() => {
  const known = props.values.filter((v): v is number => v != null);
  if (!known.length) return props.label;
  return `${props.label}: ${fmt(Math.min(...known))}–${fmt(Math.max(...known))}`;
});
const height = (v: number) => (top.value > 0 ? Math.max(4, (v / top.value) * 100) : 4);
</script>

<template>
  <div class="space-y-1">
    <p class="text-xs text-muted-foreground">{{ label }}</p>
    <div class="flex h-12 items-end gap-px" role="img" :aria-label="summary">
      <template v-for="(v, i) in values" :key="i">
        <span v-if="v == null" class="h-full flex-1" data-testid="gap" />
        <span
          v-else
          class="flex-1 bg-foreground/60"
          :style="{ height: `${height(v)}%` }"
          :title="fmt(v)"
          data-testid="bar"
        />
      </template>
    </div>
  </div>
</template>
