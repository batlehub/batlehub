<script setup lang="ts">
import { useI18n } from "vue-i18n";
/**
 * The operator's landing page, rebuilt around the two questions they actually
 * arrive with — *is it healthy* and *is it saving me anything* — rather than a
 * grid of four equal-weight counters where a degraded registry looked exactly
 * like a healthy one (RFC 0003 §6.4).
 *
 * The verdict leads. Numbers support it. A wall of stats is the shape you use
 * when you do not know which of them matters, and here we do: if a registry is
 * erroring, nothing else on this page is the story.
 */
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { adminStats, adminStatsHistory, registryHealth } from "@/client/sdk.gen";
import type { StatsResponse, StatsHistoryResponse, RegistryHealthDto } from "@/client/types.gen";
import { useApi } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { formatBytes as fmtBytes, formatCount } from "@/lib/format";
import { PageHeader } from "@/components/ui/page-header";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/ui/empty-state";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";

import { Resolution } from "@/components/ui/resolution";
import { AsyncState } from "@/components/ui/async-state";
const { t } = useI18n();

const { token } = useAuth();

const {
  data: stats,
  loading: statsLoading,
  error: statsError,
  reload: reloadStats,
} = useApi<StatsResponse>(
  () => adminStats() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);

const {
  data: health,
  loading: healthLoading,
  error: healthError,
} = useApi<RegistryHealthDto[]>(
  () => registryHealth() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);

const registries = computed<RegistryHealthDto[]>(() => health.value ?? []);

/**
 * A *fault*, not a refusal.
 *
 * `recent_errors[].error_type` is `"denied"` or `"error"`: the first is RBAC
 * turning a developer away, which is the policy engine doing its job. Counting
 * both meant the page could raise an alarm because the product worked — and at
 * 3am an alarm that fires on correct behaviour is worse than none.
 */
const degraded = computed(() =>
  registries.value.filter((r) => r.recent_errors?.some((e) => e.error_type === "error")),
);

/**
 * Empty is only empty when we could actually read.
 *
 * `useApi` sets `data = null` on error, so an unreadable health probe made
 * `registries` empty and this true — and the page then told an operator whose
 * instance was serving 205k downloads that they had no registries configured,
 * with a "add a [[registries]] block" call to action. A probe fault is not an
 * empty instance.
 */
const isFresh = computed(
  () => !healthLoading.value && !healthError.value && registries.value.length === 0,
);

/** One sentence, and it is the first thing on the page. */
const verdict = computed(() => {
  if (healthLoading.value) return null;
  if (healthError.value) return { tone: "unknown" as const, text: t("dashboard.healthUnknown") };
  if (isFresh.value) return null;
  if (degraded.value.length === 0) {
    return {
      tone: "ok" as const,
      text: t("dashboard.allAnswering", { count: registries.value.length }),
    };
  }
  return {
    tone: "bad" as const,
    text: t("dashboard.someDegraded", {
      failing: degraded.value.length,
      total: registries.value.length,
      names: degraded.value.map((r) => r.registry).join(", "),
    }),
  };
});

/**
 * What the cache actually saved. `artifact_hits` is the count of downloads
 * served without going upstream; expressing it as a share of all downloads is
 * the number an operator can act on, where a raw hit count is not.
 */
const totalRequests = computed(
  () => (stats.value?.aggregate.artifact_hits ?? 0) + (stats.value?.aggregate.artifact_misses ?? 0),
);
const hitRate = computed(() => stats.value?.aggregate.hit_rate ?? null);
const fmtPct = (n: number | null): string => (n == null ? "—" : `${(n * 100).toFixed(1)}%`);

/**
 * The trend, from the persisted rollup rather than the live counters
 * (RFC 0004 §2.3, §4.3).
 *
 * `/api/v1/admin/stats` is `since_startup` and resets with the process, so the
 * sentence above cannot answer "better or worse than before" — a deploy sets it
 * to zero. This series is bounded per hour and survives one.
 */
const { data: history } = useApi<StatsHistoryResponse>(
  () =>
    adminStatsHistory({ query: { window: "30d" } }) as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);

/**
 * A sentence a reader can act on, not a sparkline that only says "something
 * changed" — the same rule the verdict above follows.
 *
 * Three distinct outcomes, and the third is the one worth being careful about:
 * with no previous window to compare against, the honest answer is that there
 * is not enough history yet, *not* that nothing changed.
 */
const trend = computed(() => {
  const t0 = history.value?.trend;
  if (!t0 || t0.hit_rate == null) return null;
  const days = history.value?.window_days ?? 30;
  if (t0.previous_hit_rate == null || t0.delta == null) {
    return { tone: "unknown" as const, days, rate: t0.hit_rate, delta: null };
  }
  // Under a tenth of a point either way is noise, and calling it a direction
  // would have an operator chasing a rounding artefact.
  const direction = t0.delta > 0 ? "up" : "down";
  const tone = Math.abs(t0.delta) < 0.001 ? "flat" : direction;
  return { tone, days, rate: t0.hit_rate, delta: t0.delta };
});

const trendText = computed(() => {
  const tr = trend.value;
  if (!tr) return "";
  const rate = fmtPct(tr.rate);
  if (tr.tone === "unknown") {
    return t("adminDashboard.trendNoBaseline", { rate, days: tr.days });
  }
  if (tr.tone === "flat") {
    return t("adminDashboard.trendFlat", { rate, days: tr.days });
  }
  const points = Math.abs((tr.delta ?? 0) * 100).toFixed(1);
  return tr.tone === "up"
    ? t("adminDashboard.trendUp", { rate, days: tr.days, points })
    : t("adminDashboard.trendDown", { rate, days: tr.days, points });
});
</script>

<template>
  <div class="space-y-6">
    <PageHeader variant="display" :title="t('dashboard.title')" />

    <Skeleton v-if="healthLoading" :lines="3" />

    <!-- The verdict, before any number. -->
    <div
      v-else-if="verdict"
      class="border px-4 py-3"
      :class="verdict.tone === 'bad' ? 'border-destructive' : 'border-border'"
    >
      <p class="font-mono text-sm" :class="verdict.tone === 'bad' ? 'text-destructive' : ''">
        {{ verdict.text }}
      </p>
      <p v-if="verdict.tone === 'bad'" class="mt-2">
        <Button as-child size="sm" variant="outline">
          <RouterLink to="/admin/observability/health">{{
            t("adminDashboard.openHealth")
          }}</RouterLink>
        </Button>
      </p>
    </div>

    <EmptyState
      v-if="isFresh"
      :title="t('adminDashboard.noRegistriesConfiguredYet')"
      :description="t('adminDashboard.nothingIsCachedOrServed')"
    >
      <template #action>
        <Button as-child size="sm">
          <RouterLink to="/admin/operations/config-reload">{{
            t("adminDashboard.openConfig")
          }}</RouterLink>
        </Button>
      </template>
    </EmptyState>

    <template v-else>
      <AsyncState
        v-if="statsError"
        :loading="false"
        :error="statsError"
        :title="t('adminDashboard.statsCouldNotLoad')"
        :on-retry="reloadStats"
      />
      <Skeleton v-else-if="statsLoading" :lines="2" />

      <!-- What the cache saved, stated as a sentence rather than as four tiles. -->
      <p v-else-if="stats" class="text-sm text-muted-foreground">
        <i18n-t keypath="adminDashboard.cacheSummary" tag="span">
          <template #rate
            ><span class="font-mono text-foreground">{{ fmtPct(hitRate) }}</span></template
          >
          <template #total
            ><span class="font-mono text-foreground">{{
              formatCount(totalRequests)
            }}</span></template
          >
          <template #since
            ><span class="font-mono">{{
              new Date(stats.since_startup).toLocaleDateString()
            }}</span></template
          >
          <template #size
            ><span class="font-mono text-foreground">{{
              fmtBytes(stats.aggregate.cached_bytes)
            }}</span></template
          >
        </i18n-t>
      </p>

      <!-- What the counters above structurally cannot say: better or worse than
           before. Absent until there is a rollup to read, rather than showing a
           zero that would read as "no change".

           Copper on `down` is the degradation job DESIGN.md's Secondary entry
           authorises — a measured value moving the wrong way but not refused.
           This page and QuotaWidget reached for it before the world had it;
           RFC 0004-bis §11/O1 gave copper the job rather than a sixth hue. -->
      <p
        v-if="trendText"
        class="text-sm"
        :class="trend?.tone === 'down' ? 'text-copper' : 'text-muted-foreground'"
        data-testid="dashboard-trend"
      >
        {{ trendText }}
      </p>

      <section v-if="stats && stats.per_registry.length > 0" class="space-y-2">
        <h2 class="font-mono text-xs uppercase tracking-wider text-muted-foreground">
          {{ t("adminDashboard.perRegistry") }}
        </h2>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("common.registry") }}</TableHead>
              <TableHead class="text-right">{{ t("adminDashboard.hitRate") }}</TableHead>
              <TableHead class="text-right">{{ t("common.hits") }}</TableHead>
              <TableHead class="text-right">{{ t("common.misses") }}</TableHead>
              <TableHead class="text-right">{{ t("common.cached") }}</TableHead>
              <TableHead>{{ t("common.state") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow v-for="r in stats.per_registry" :key="r.registry">
              <TableCell class="font-mono text-sm">{{ r.registry }}</TableCell>
              <TableCell class="text-right font-mono text-sm">
                {{ fmtPct(r.hit_rate ?? null) }}
              </TableCell>
              <TableCell class="text-right font-mono text-sm text-muted-foreground">
                {{ formatCount(r.artifact_hits) }}
              </TableCell>
              <TableCell class="text-right font-mono text-sm text-muted-foreground">
                {{ formatCount(r.artifact_misses) }}
              </TableCell>
              <TableCell class="text-right font-mono text-sm text-muted-foreground">
                {{ fmtBytes(r.cached_bytes) }}
              </TableCell>
              <!-- State as the system draws it — pattern, word, hue — on the
                   page that is *about* cache state, which never showed one. A
                   degraded registry links to where its errors are, instead of
                   naming a problem and leaving the reader to find it; the
                   healthy word was a hardcoded English "OK". -->
              <TableCell>
                <RouterLink
                  v-if="degraded.some((d) => d.registry === r.registry)"
                  to="/admin/observability/health"
                  class="text-destructive hover:underline underline-offset-[3px]"
                >
                  <Resolution state="blocked" :label="t('adminDashboard.registryErrors')" />
                </RouterLink>
                <Resolution v-else state="cached" :label="t('adminDashboard.registryServing')" />
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </section>
    </template>
  </div>
</template>
