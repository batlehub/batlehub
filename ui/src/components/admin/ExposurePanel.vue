<script setup lang="ts">
import { computed, ref, onMounted, watch } from "vue";
import { watchDebounced } from "@vueuse/core";
import { useI18n } from "vue-i18n";
import { useRoute, useRouter } from "vue-router";

import { exposureReport } from "@/client/sdk.gen";
import type { ExposureCoverage, ExposureRow } from "@/client/types.gen";
import { useAuthFetch } from "@/composables/useAuthFetch";
import { API_BASE_URL } from "@/config";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { AsyncState } from "@/components/ui/async-state";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";
import { formatDate } from "@/lib/format";
import CodeLabel from "@/components/admin/CodeLabel.vue";
import { FLAG_EFFECT_LABEL_KEYS } from "@/config/codeLabels";
import { DateTime } from "@/components/ui/date-time";

/**
 * The exposure report (RFC 0002 §4.6): who pulled a flagged version, one
 * row per consumer × coordinate × flag, newest pull first, with how many of
 * the pulls preceded the flag — the retroactive case. Pages by the cursor
 * the endpoint returns; the coverage block is printed beside the rows
 * because a report silent about its blind spots reads as a fact.
 */
const { t } = useI18n();
const { authFetch } = useAuthFetch();

const route = useRoute();
const router = useRouter();
const one = (v: unknown) => (typeof v === "string" && v ? v : undefined);
const WHENS = ["any", "before_flag", "after_flag"];

const source = ref("");
const minEffect = ref("");
// A link from the overview opens on one version's downloads after its flag.
const when = ref(WHENS.includes(one(route.query.when) ?? "") ? one(route.query.when)! : "any");
const scope = ref({ registry: one(route.query.registry), package: one(route.query.package) });

function clearScope() {
  scope.value = { registry: undefined, package: undefined };
  void router.replace({ query: { ...route.query, registry: undefined, package: undefined } });
  void load();
}

const rows = ref<ExposureRow[]>([]);
const next = ref<string | null>(null);
const coverage = ref<ExposureCoverage | null>(null);
const loading = ref(false);
const error = ref<string | null>(null);
/** Kept apart from `error`: a failed export says nothing about the rows on screen. */
const exportError = ref<string | null>(null);

const effectOptions = computed(() => [
  { value: "", label: t("exposurePanel.anyEffect") },
  { value: "warn", label: "warn" },
  { value: "gate", label: "gate" },
  { value: "hard_block", label: "hard_block" },
]);

const whenOptions = computed(() => [
  { value: "any", label: t("exposurePanel.whenAny") },
  { value: "before_flag", label: t("exposurePanel.whenBefore") },
  { value: "after_flag", label: t("exposurePanel.whenAfter") },
]);

function query(after?: string) {
  return {
    source: source.value.trim() || undefined,
    min_effect: minEffect.value || undefined,
    registry: scope.value.registry,
    package_name: scope.value.package,
    when: when.value,
    after,
    limit: 50,
  };
}

/** A failed fresh load drops the previous query's rows: they answered another question. */
function fail(append: boolean) {
  error.value = t("exposurePanel.loadFailed");
  if (append) return;
  rows.value = [];
  next.value = null;
  coverage.value = null;
}

// A later filter wins: an answer to an earlier one is dropped.
let latest = 0;
async function load(append = false) {
  const mine = ++latest;
  loading.value = true;
  error.value = null;
  try {
    const { data, error: err } = await exposureReport({
      query: query(append ? (next.value ?? undefined) : undefined),
    });
    if (mine !== latest) return;
    if (err || !data) {
      fail(append);
      return;
    }
    rows.value = append ? [...rows.value, ...data.rows] : data.rows;
    next.value = data.next ?? null;
    coverage.value = data.coverage;
  } catch {
    if (mine === latest) fail(append);
  } finally {
    if (mine === latest) loading.value = false;
  }
}

async function exportCsv() {
  exportError.value = null;
  const params = new URLSearchParams({ format: "csv", when: when.value });
  if (source.value.trim()) params.set("source", source.value.trim());
  if (minEffect.value) params.set("min_effect", minEffect.value);
  if (scope.value.registry) params.set("registry", scope.value.registry);
  if (scope.value.package) params.set("package_name", scope.value.package);
  const resp = await authFetch(`${API_BASE_URL}/api/v1/admin/exposure/export?${params}`);
  if (!resp.ok) {
    exportError.value = t("exposurePanel.exportFailed");
    return;
  }
  const blob = await resp.blob();
  const a = Object.assign(document.createElement("a"), {
    href: URL.createObjectURL(blob),
    download: `exposure-${new Date().toISOString().slice(0, 10)}.csv`,
  });
  a.click();
  URL.revokeObjectURL(a.href);
}

function effectVariant(effect: string) {
  switch (effect) {
    case "hard_block":
      return "destructive";
    case "gate":
      return "copper";
    default:
      return "secondary";
  }
}

/** The last scan the coverage block knows, or nothing: "never" is a fact. */
const lastScanLine = computed(() => {
  const c = coverage.value;
  if (!c) return "";
  if (!c.last_scan?.length) return t("exposurePanel.neverScanned");
  const latest = [...c.last_scan].sort((a, b) => b.last_scan_at.localeCompare(a.last_scan_at))[0];
  return t("exposurePanel.lastScan", {
    registry: latest.registry,
    when: formatDate(latest.last_scan_at),
  });
});

onMounted(() => load());
// Filters apply as they change, as on every other console page; the source
// field waits for typing to pause (Enter applies it at once).
watch([minEffect, when], () => void load());
watchDebounced(source, () => void load(), { debounce: 300 });
</script>

<template>
  <Card data-testid="exposure-panel">
    <CardHeader>
      <CardTitle>{{ t("exposurePanel.title") }}</CardTitle>
    </CardHeader>
    <CardContent class="space-y-4">
      <p class="text-sm text-muted-foreground max-w-[70ch]">{{ t("exposurePanel.intro") }}</p>

      <p
        v-if="scope.registry || scope.package"
        class="flex flex-wrap items-baseline gap-x-3 text-sm"
        data-testid="exposure-scope"
      >
        <span
          >{{ t("exposurePanel.scoped") }}
          <span class="font-mono">{{
            [scope.registry, scope.package].filter(Boolean).join("/")
          }}</span></span
        >
        <button
          type="button"
          class="text-primary underline underline-offset-[3px]"
          @click="clearScope"
        >
          {{ t("exposurePanel.clearScope") }}
        </button>
      </p>

      <div class="grid grid-cols-1 sm:grid-cols-3 gap-4 max-w-3xl">
        <div class="space-y-2">
          <Label for="exposure-source">{{ t("exposurePanel.source") }}</Label>
          <Input
            id="exposure-source"
            v-model="source"
            :placeholder="t('exposurePanel.anySource')"
            @keyup.enter="load()"
          />
        </div>
        <div class="space-y-2">
          <Label for="exposure-effect">{{ t("exposurePanel.minEffect") }}</Label>
          <Select id="exposure-effect" v-model="minEffect" :options="effectOptions" />
        </div>
        <div class="space-y-2">
          <Label for="exposure-when">{{ t("exposurePanel.when") }}</Label>
          <Select id="exposure-when" v-model="when" :options="whenOptions" />
        </div>
      </div>

      <div class="flex flex-wrap items-center gap-2">
        <Button
          size="sm"
          variant="outline"
          :disabled="loading"
          data-testid="exposure-export"
          @click="exportCsv"
        >
          {{ t("exposurePanel.exportCsv") }}
        </Button>
      </div>

      <!-- A failed export, or a failed "load more" under rows that still
           stand, is an action error beside the table, not a failed load. -->
      <p v-if="exportError || (error && rows.length)" class="text-sm text-destructive">
        {{ exportError ?? error }}
      </p>

      <AsyncState
        :loading="loading && !rows.length"
        :error="rows.length ? null : error"
        :empty="!rows.length"
        :empty-message="t('exposurePanel.empty')"
        :on-retry="() => load()"
      >
        <Table :label="t('exposurePanel.title')" data-testid="exposure-rows">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("exposurePanel.consumer") }}</TableHead>
              <TableHead>{{ t("common.package") }}</TableHead>
              <TableHead>{{ t("exposurePanel.flag") }}</TableHead>
              <TableHead>{{ t("exposurePanel.pulls") }}</TableHead>
              <TableHead>{{ t("exposurePanel.beforeFlag") }}</TableHead>
              <TableHead>{{ t("exposurePanel.lastPull") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow
              v-for="r in rows"
              :key="`${r.consumer}|${r.registry}|${r.package_name}|${r.version}|${r.flag_id}`"
            >
              <TableCell class="font-mono text-sm">{{ r.consumer }}</TableCell>
              <TableCell class="font-mono text-sm">
                {{ r.registry }}/{{ r.package_name }}@{{ r.version }}
              </TableCell>
              <TableCell class="text-sm">
                <Badge :variant="effectVariant(r.effect)" class="text-xs mr-1">
                  <CodeLabel :code="r.effect" :labels="FLAG_EFFECT_LABEL_KEYS" />
                </Badge>
                <span class="font-mono text-xs text-muted-foreground">
                  {{ r.source }}:{{ r.external_id }}
                </span>
              </TableCell>
              <TableCell class="tabular-nums">{{ r.pulls }}</TableCell>
              <TableCell class="tabular-nums">{{ r.pulls_before_flag }}</TableCell>
              <TableCell class="whitespace-nowrap text-xs tabular-nums">
                <DateTime :iso="r.last_pull" />
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </AsyncState>

      <Button
        v-if="next"
        size="sm"
        variant="outline"
        :disabled="loading"
        data-testid="exposure-more"
        @click="load(true)"
      >
        {{ t("exposurePanel.more") }}
      </Button>

      <!-- The blind spots, in words: how many registries the CVE scan can
           reach, when it last did, and which sources have said anything. -->
      <dl
        v-if="coverage"
        class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs text-muted-foreground"
        data-testid="exposure-coverage"
      >
        <dt>{{ t("exposurePanel.coverageRegistries") }}</dt>
        <dd class="tabular-nums">
          {{
            t("exposurePanel.coverageLine", {
              total: coverage.registries_total,
              sbom: coverage.sbom_configured,
              security: coverage.security_profiles,
            })
          }}
        </dd>
        <dt>{{ t("exposurePanel.coverageScan") }}</dt>
        <dd>{{ lastScanLine }}</dd>
        <dt>{{ t("exposurePanel.coverageSources") }}</dt>
        <dd class="font-mono">
          <span v-if="!coverage.flag_sources?.length">{{ t("exposurePanel.noSource") }}</span>
          <span v-for="s in coverage.flag_sources ?? []" :key="s.source" class="mr-3">
            {{ s.source }} ({{ s.live_flags }})
          </span>
        </dd>
      </dl>
    </CardContent>
  </Card>
</template>
