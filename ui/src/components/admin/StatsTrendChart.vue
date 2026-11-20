<script setup lang="ts">
/**
 * One or two series as rows of bars on one x-axis, oldest first (RFC 0044
 * §4.5, Activity tab; RFC 0050 §6.6, Trends).
 *
 * `null` is a gap — a day with no data is not a day at zero — drawn as a dot,
 * never as a flat bar. Series are told apart by a marker, never by colour
 * alone: the first is filled, the second outlined (RFC 0050 §11, criterion 3).
 * The numbers are in the text alternative (min, max, last, and how many
 * points have no data); the bars are for a sighted reader. No transition, so
 * nothing moves under `prefers-reduced-motion` (criterion 5). The same data as
 * a table is the page's *Show as table* (criterion 2).
 */
import { computed } from "vue";
import { useI18n } from "vue-i18n";

interface Series {
  label: string;
  values: (number | null)[];
}

const props = defineProps<{
  label: string;
  /** One series, the RFC 0044 shape. Ignored when `series` is given. */
  values?: (number | null)[];
  /** Up to two series sharing the axis and the scale (RFC 0050). */
  series?: Series[];
  /** The top of the scale; the series' own maximum when absent. */
  max?: number;
  /** How a value reads in the label and in each bar's tooltip. */
  format?: (v: number) => string;
  /** What one point stands for: a UTC day (Trends) or an hour (the stats rollup). */
  unit?: "day" | "hour";
}>();

const { t } = useI18n();

const all = computed<Series[]>(
  () => props.series ?? [{ label: props.label, values: props.values ?? [] }],
);
const known = (s: Series) => s.values.filter((v): v is number => v != null);
const top = computed(() => props.max ?? Math.max(0, ...all.value.flatMap(known)));
const fmt = (v: number) => (props.format ? props.format(v) : String(v));
const length = computed(() => Math.max(0, ...all.value.map((s) => s.values.length)));

/** Criterion 1 and 6: min, max and last value per series, and the gaps, in words. */
const summary = computed(() =>
  all.value
    .map((s) => {
      const k = known(s);
      const gaps = s.values.length - k.length;
      const gapWords =
        props.unit === "hour"
          ? t("trendChart.noDataHours", { n: gaps })
          : t("trendChart.noData", { n: gaps });
      const gapText = gaps ? ` (${gapWords})` : "";
      if (!k.length) return `${s.label}: ${t("trendChart.noDataAtAll")}`;
      const last = [...s.values].reverse().find((v): v is number => v != null) ?? 0;
      return `${s.label}: ${t("trendChart.range", {
        min: fmt(Math.min(...k)),
        max: fmt(Math.max(...k)),
        last: fmt(last),
      })}${gapText}`;
    })
    .join("; "),
);
const height = (v: number) => (top.value > 0 ? Math.max(4, (v / top.value) * 100) : 4);
</script>

<template>
  <div class="space-y-1">
    <p class="text-xs text-muted-foreground">
      {{ label }}
      <template v-if="all.length > 1">
        <span v-for="(s, si) in all" :key="s.label" class="ml-2 inline-flex items-center gap-1">
          <span
            class="inline-block h-2 w-2"
            :class="si === 0 ? 'bg-foreground' : 'border border-foreground'"
            aria-hidden="true"
          />{{ s.label }}
        </span>
      </template>
    </p>
    <div class="flex h-12 items-end gap-px" role="img" :aria-label="summary">
      <div v-for="i in length" :key="i" class="flex h-full min-w-0 flex-1 items-end gap-px">
        <template v-for="(s, si) in all" :key="si">
          <span
            v-if="s.values[i - 1] == null"
            class="flex h-full min-w-0 flex-1 items-end justify-center overflow-hidden text-xs leading-none text-muted-foreground"
            data-testid="gap"
            aria-hidden="true"
            >·</span
          >
          <span
            v-else
            class="flex-1"
            :class="si === 0 ? 'bg-foreground' : 'border border-foreground'"
            :style="{ height: `${height(s.values[i - 1]!)}%` }"
            :title="`${s.label}: ${fmt(s.values[i - 1]!)}`"
            data-testid="bar"
          />
        </template>
      </div>
    </div>
  </div>
</template>
