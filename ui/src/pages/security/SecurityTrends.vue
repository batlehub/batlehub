<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { useRoute, useRouter } from "vue-router";
import { securityCoverage, securityTrends } from "@/client/sdk.gen";
import type { SecurityTrends, TrendDay } from "@/client/types.gen";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import StatsTrendChart from "@/components/admin/StatsTrendChart.vue";
import { formatCount } from "@/lib/format";
import { useAuth } from "@/composables/useAuth";

/**
 * The daily security rollup (RFC 0050 §4.2, §5.2): one row per series, its
 * last value and a one-line reading. A day without a value is a gap, never a
 * zero; days are UTC days and the page says so. `?window=` and `?registry=`
 * are in the address, so a view can be shared.
 */
const { t } = useI18n();
const route = useRoute();
const router = useRouter();

const WINDOWS = ["7d", "30d", "90d"] as const;
const one = (v: unknown) => (Array.isArray(v) ? v[0] : v) as string | undefined;
const windowSel = computed<string>({
  get: () => {
    const w = one(route.query.window);
    return w && (WINDOWS as readonly string[]).includes(w) ? w : "30d";
  },
  set: (w) => void router.replace({ query: { ...route.query, window: w } }),
});
const registry = computed<string>({
  get: () => one(route.query.registry) ?? "",
  set: (r) => void router.replace({ query: { ...route.query, registry: r || undefined } }),
});

const { can } = useAuth();
/** The flag series and columns are the server's only with flags:read. */
const withFlags = computed(() => can("flags:read"));

const registries = ref<string[]>([]);
// The picker is a convenience: without it, "all registries" still works.
async function loadRegistries() {
  try {
    const { data } = await securityCoverage();
    registries.value = (data?.registries ?? []).filter((r) => r.protected).map((r) => r.registry);
  } catch {
    // Left empty: the picker then offers "all registries" alone.
  }
}
onMounted(loadRegistries);
const registryOptions = computed(() => [
  { value: "", label: t("securityTrends.allRegistries") },
  ...registries.value.map((r) => ({ value: r, label: r })),
]);

const trends = ref<SecurityTrends | null>(null);
const loading = ref(false);
const error = ref<string | null>(null);

// A later choice wins: an answer to an earlier one is dropped.
let latest = 0;
async function load() {
  const mine = ++latest;
  loading.value = true;
  error.value = null;
  try {
    const { data, error: err } = await securityTrends({
      query: { window: windowSel.value, registry: registry.value || undefined },
    });
    if (mine !== latest) return;
    if (err || !data) throw new Error(t("securityTrends.loadFailed"));
    trends.value = data;
  } catch (e) {
    if (mine === latest) error.value = (e as Error).message;
  } finally {
    if (mine === latest) loading.value = false;
  }
}
watch([windowSel, registry], load, { immediate: true });

type Pick = (d: TrendDay) => number | null | undefined;
const sum =
  (...picks: Pick[]) =>
  (d: TrendDay) => {
    const vs = picks.map((p) => p(d)).filter((v): v is number => v != null);
    return vs.length ? vs.reduce((a, b) => a + b, 0) : null;
  };
const col = (pick: Pick) => (trends.value?.days ?? []).map((d) => pick(d) ?? null);
const last = (values: (number | null)[]) =>
  [...values].reverse().find((v): v is number => v != null) ?? null;

interface Row {
  key: string;
  label: string;
  series: { label: string; values: (number | null)[] }[];
  reading: string;
}

const rows = computed<Row[]>(() => {
  const days = trends.value?.days ?? [];
  if (!days.length) return [];
  const held = col(
    sum(
      (d) => d.quarantined,
      (d) => d.denied,
    ),
  );
  const refused = col((d) => d.refusals);
  const out: Row[] = [
    {
      key: "held",
      label: t("securityTrends.held"),
      series: [{ label: t("securityTrends.held"), values: held }],
      reading: t("securityTrends.heldReading", {
        denied: formatCount(last(col((d) => d.denied))),
        waiting: formatCount(last(col((d) => d.quarantined))),
      }),
    },
    {
      key: "refused",
      label: t("securityTrends.refused"),
      series: [{ label: t("securityTrends.refused"), values: refused }],
      reading: t("securityTrends.refusedReading", {
        n: formatCount(refused.reduce<number>((a, v) => a + (v ?? 0), 0)),
      }),
    },
  ];
  // The flag series ride only with flags:read; the server leaves them out.
  if (withFlags.value) {
    const opened = col((d) => d.flags_opened);
    const revoked = col((d) => d.flags_revoked);
    const total = (vs: (number | null)[]) => vs.reduce<number>((a, v) => a + (v ?? 0), 0);
    out.push({
      key: "flags",
      label: t("securityTrends.flags"),
      series: [
        { label: t("securityTrends.opened"), values: opened },
        { label: t("securityTrends.revoked"), values: revoked },
      ],
      reading: `+${total(opened)} / −${total(revoked)}`,
    });
  }
  const done = col((d) => d.jobs_done);
  const failed = col((d) => d.jobs_failed);
  out.push({
    key: "scans",
    label: t("securityTrends.scans"),
    series: [
      { label: t("securityTrends.done"), values: done },
      { label: t("securityTrends.failed"), values: failed },
    ],
    reading: `${formatCount(done.reduce<number>((a, v) => a + (v ?? 0), 0))} / ${formatCount(
      failed.reduce<number>((a, v) => a + (v ?? 0), 0),
    )}`,
  });
  return out;
});

/** The same data as a table (RFC 0050 §11, criterion 2). */
const asTable = ref(false);
const cell = (v: number | null | undefined) => (v == null ? t("trendChart.noDataCell") : String(v));
</script>

<template>
  <div class="space-y-6">
    <PageHeader :title="t('securityNav.trends')" :description="t('securityTrends.description')" />

    <div class="flex flex-wrap items-end gap-4">
      <fieldset class="min-w-0 space-y-1">
        <legend class="text-xs text-muted-foreground">{{ t("securityTrends.window") }}</legend>
        <div class="flex">
          <Button
            v-for="w in WINDOWS"
            :key="w"
            size="sm"
            :variant="windowSel === w ? 'default' : 'outline'"
            :aria-pressed="windowSel === w"
            @click="windowSel = w"
            >{{ w }}</Button
          >
        </div>
      </fieldset>
      <div class="space-y-1">
        <Label for="trends-registry">{{ t("common.registry") }}</Label>
        <Select id="trends-registry" v-model="registry" :options="registryOptions" />
      </div>
      <p class="text-xs text-muted-foreground">{{ t("securityTrends.utc") }}</p>
      <Button variant="outline" size="sm" :aria-pressed="asTable" @click="asTable = !asTable">{{
        asTable ? t("securityTrends.showCharts") : t("securityTrends.showTable")
      }}</Button>
    </div>

    <AsyncState
      :loading="loading && !trends"
      :error="error"
      :empty="
        !!trends &&
        trends.history_enabled &&
        !trends.days.some((d) => d.allowed != null || d.refusals != null || d.jobs_done != null)
      "
      :empty-message="t('securityTrends.noHistoryYet')"
      :on-retry="load"
    >
      <p v-if="trends && !trends.history_enabled" class="text-sm" data-testid="history-off">
        {{ t("securityTrends.historyOff") }}
      </p>
      <template v-else-if="trends">
        <p v-if="trends.history_starts" class="text-xs text-muted-foreground">
          {{ t("securityTrends.historyStarts", { day: trends.history_starts }) }}
        </p>
        <div v-if="!asTable" class="space-y-6" data-testid="trend-charts">
          <div
            v-for="row in rows"
            :key="row.key"
            class="grid grid-cols-1 gap-2 md:grid-cols-[minmax(0,1fr)_16rem] md:items-end"
          >
            <StatsTrendChart :label="row.label" :series="row.series" />
            <p class="font-mono text-sm tabular-nums">{{ row.reading }}</p>
          </div>
          <p class="text-xs text-muted-foreground">{{ t("securityTrends.gapLegend") }}</p>
        </div>
        <Table v-else :label="t('securityNav.trends')" data-testid="trend-table">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("securityTrends.day") }}</TableHead>
              <TableHead class="text-right">{{ t("securityTrends.held") }}</TableHead>
              <TableHead class="text-right">{{ t("securityTrends.refused") }}</TableHead>
              <template v-if="withFlags">
                <TableHead class="text-right">{{ t("securityTrends.opened") }}</TableHead>
                <TableHead class="text-right">{{ t("securityTrends.revoked") }}</TableHead>
              </template>
              <TableHead class="text-right">{{ t("securityTrends.done") }}</TableHead>
              <TableHead class="text-right">{{ t("securityTrends.failed") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow v-for="d in trends.days" :key="d.day">
              <TableCell class="font-mono text-xs">{{ d.day }}</TableCell>
              <TableCell class="text-right tabular-nums">{{
                cell(
                  sum(
                    (x) => x.quarantined,
                    (x) => x.denied,
                  )(d),
                )
              }}</TableCell>
              <TableCell class="text-right tabular-nums">{{ cell(d.refusals) }}</TableCell>
              <template v-if="withFlags">
                <TableCell class="text-right tabular-nums">{{ cell(d.flags_opened) }}</TableCell>
                <TableCell class="text-right tabular-nums">{{ cell(d.flags_revoked) }}</TableCell>
              </template>
              <TableCell class="text-right tabular-nums">{{ cell(d.jobs_done) }}</TableCell>
              <TableCell class="text-right tabular-nums">{{ cell(d.jobs_failed) }}</TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </template>
    </AsyncState>
  </div>
</template>
