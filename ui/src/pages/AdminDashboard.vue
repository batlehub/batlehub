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
import { ExternalLink } from "@lucide/vue";
import {
  adminStats,
  adminStatsHistory,
  getConfigWarnings,
  instanceSecurity,
  registryHealth,
  securityOverview,
} from "@/client/sdk.gen";
import type {
  ConfigWarningsResponse,
  InstanceSecurity,
  SecurityOverview,
  StatsResponse,
  StatsHistoryResponse,
  RegistryHealthDto,
} from "@/client/types.gen";
import SecurityStatusStrip from "@/components/admin/SecurityStatusStrip.vue";
import StateMark from "@/components/admin/StateMark.vue";
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
import { DateTime } from "@/components/ui/date-time";
const { t } = useI18n();

const { token, can, canAny } = useAuth();

/**
 * RFC 0050 §4.2 (the critique's P0): the dashboard says whether anything
 * security-wise is wrong, line by line for the verbs the viewer holds —
 * holds, flags, scanners from the security overview, and the security
 * warnings of the config from instance security. A line whose verb is not
 * held is absent (decision 11).
 */
const canSecurity = computed(() => canAny(["quarantine:read", "flags:read"]));
const {
  data: security,
  error: securityError,
  reload: reloadSecurity,
} = useApi<SecurityOverview>(
  () =>
    canSecurity.value
      ? (securityOverview() as Promise<{ data?: unknown; error?: unknown }>)
      : Promise.resolve({ data: null }),
  [token],
);
const canConfigLine = computed(() => can("config:read"));
/** Instance security needs `system:read` too; without it, the config page's own list. */
const viaInstance = computed(() => canConfigLine.value && can("system:read"));
const { data: instance, error: instanceError } = useApi<InstanceSecurity>(
  () =>
    viaInstance.value
      ? (instanceSecurity({ query: { window: "24h" } }) as Promise<{
          data?: unknown;
          error?: unknown;
        }>)
      : Promise.resolve({ data: null }),
  [token],
);
const { data: configWarnings, error: configWarningsError } = useApi<ConfigWarningsResponse>(
  () =>
    canConfigLine.value && !viaInstance.value
      ? (getConfigWarnings() as Promise<{ data?: unknown; error?: unknown }>)
      : Promise.resolve({ data: null }),
  [token],
);
const securityWarnings = computed(() =>
  viaInstance.value ? (instance.value?.warnings ?? null) : (configWarnings.value?.warnings ?? null),
);
/** The config line is owed to whoever may read it: a failed read is UNKNOWN, not absent. */
const configLineUnknown = computed(
  () =>
    canConfigLine.value && !!(viaInstance.value ? instanceError.value : configWarningsError.value),
);

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
/** `[stats] dashboard_url`, bound only when http(s): an `href` is a script vector (T11). */
const monitoring = computed(() => {
  const u = stats.value?.monitoring_url;
  return u && /^https?:\/\//i.test(u) ? u : undefined;
});
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
  if (t0?.hit_rate == null) return null;
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
    <PageHeader variant="display" :title="t('dashboard.title')">
      <template v-if="monitoring" #actions>
        <Button as-child size="sm" variant="outline">
          <a :href="monitoring" target="_blank" rel="noopener noreferrer" data-testid="monitoring">
            {{ t("adminRegistry.monitoring") }}
            <ExternalLink class="h-3.5 w-3.5" aria-hidden="true" />
          </a>
        </Button>
      </template>
    </PageHeader>

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
          <RouterLink to="/admin/system/health">{{ t("adminDashboard.openHealth") }}</RouterLink>
        </Button>
      </p>
    </div>

    <section
      v-if="canSecurity || securityWarnings"
      class="space-y-2"
      aria-labelledby="dashboard-security"
      data-testid="dashboard-security"
    >
      <h2 id="dashboard-security" class="font-mono text-xs uppercase tracking-wider">
        {{ t("nav.security") }}
      </h2>
      <p v-if="canSecurity && securityError" class="text-sm" data-testid="security-unknown">
        <StateMark state="unknown" /> {{ t("securityStatus.couldNotRead") }}
        <Button variant="outline" size="sm" class="ml-2" @click="reloadSecurity">{{
          t("common.retry")
        }}</Button>
      </p>
      <SecurityStatusStrip
        v-if="security || securityWarnings || configLineUnknown"
        :overview="security ?? { computed_at: '', decisions: { total: 0, items: [] } }"
        :domains="['holds', 'flags', 'scanners']"
        @retry="reloadSecurity"
      >
        <li
          v-if="securityWarnings || configLineUnknown"
          class="flex flex-col gap-1 py-2 lg:flex-row lg:items-baseline lg:gap-3"
          data-testid="status-config"
        >
          <StateMark
            :state="!securityWarnings ? 'unknown' : securityWarnings.length ? 'attention' : 'ok'"
          />
          <span class="w-40 shrink-0 font-mono text-sm">{{ t("instanceSecurity.config") }}</span>
          <span class="min-w-0 flex-1 text-sm">{{
            !securityWarnings
              ? t("securityStatus.couldNotRead")
              : viaInstance
                ? t("instanceSecurity.warnings", { n: securityWarnings.length })
                : t("adminDashboard.configWarnings", { n: securityWarnings.length })
          }}</span>
          <RouterLink
            v-if="viaInstance"
            to="/admin/instance-security"
            class="shrink-0 text-sm text-primary underline underline-offset-[3px]"
            >→ {{ t("adminNav.instanceSecurity") }}</RouterLink
          >
          <RouterLink
            v-else
            to="/admin/system/configuration"
            class="shrink-0 text-sm text-primary underline underline-offset-[3px]"
            >→ {{ t("adminNav.configReload") }}</RouterLink
          >
        </li>
      </SecurityStatusStrip>
    </section>

    <EmptyState
      v-if="isFresh"
      :title="t('adminDashboard.noRegistriesConfiguredYet')"
      :description="t('adminDashboard.nothingIsCachedOrServed')"
    >
      <template #action>
        <Button as-child size="sm">
          <RouterLink to="/admin/system/configuration">{{
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
        <!-- RFC 0050 §6.7: no "of 0 downloads" — a rate of nothing is not a rate. -->
        <i18n-t
          :keypath="
            totalRequests ? 'adminDashboard.cacheSummary' : 'adminDashboard.cacheSummaryNone'
          "
          tag="span"
        >
          <template #rate
            ><span class="font-mono text-foreground">{{ fmtPct(hitRate) }}</span></template
          >
          <template #total
            ><span class="font-mono text-foreground">{{
              formatCount(totalRequests)
            }}</span></template
          >
          <template #since
            ><span class="font-mono"><DateTime :iso="stats.since_startup" /></span
          ></template>
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
        <h2 class="font-mono text-xs uppercase tracking-[0.16em] text-muted-foreground">
          {{ t("adminDashboard.perRegistry") }}
        </h2>
        <Table :label="t('adminDashboard.perRegistry')">
          <!-- On a phone the three raw counts leave the table whole rather than
               pushing State off-screen: State is the answer, the counts are detail. -->
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("common.registry") }}</TableHead>
              <TableHead class="text-right">{{ t("adminDashboard.hitRate") }}</TableHead>
              <TableHead class="hidden text-right sm:table-cell">{{ t("common.hits") }}</TableHead>
              <TableHead class="hidden text-right sm:table-cell">{{
                t("common.misses")
              }}</TableHead>
              <TableHead class="hidden text-right sm:table-cell">{{
                t("common.cached")
              }}</TableHead>
              <TableHead>{{ t("common.state") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow v-for="r in stats.per_registry" :key="r.registry">
              <TableCell class="font-mono text-sm">
                <!-- The registry page asks system:read; this page only stats:read. -->
                <RouterLink
                  v-if="can('system:read')"
                  :to="`/admin/registry/${encodeURIComponent(r.registry)}`"
                  class="hover:underline underline-offset-[3px]"
                  >{{ r.registry }}</RouterLink
                >
                <template v-else>{{ r.registry }}</template>
              </TableCell>
              <TableCell class="text-right font-mono text-sm">
                {{ fmtPct(r.hit_rate ?? null) }}
              </TableCell>
              <TableCell
                class="hidden text-right font-mono text-sm text-muted-foreground sm:table-cell"
              >
                {{ formatCount(r.artifact_hits) }}
              </TableCell>
              <TableCell
                class="hidden text-right font-mono text-sm text-muted-foreground sm:table-cell"
              >
                {{ formatCount(r.artifact_misses) }}
              </TableCell>
              <TableCell
                class="hidden text-right font-mono text-sm text-muted-foreground sm:table-cell"
              >
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
                  to="/admin/system/health"
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
