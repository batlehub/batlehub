<script setup lang="ts">
/**
 * One registry's configuration and health (RFC 0044 §4.5, phase 5).
 *
 * The server decides what each caller sees: `config` is absent without
 * `config:read`, the upstream URLs without `upstreams:read`, the queue counts
 * without `quarantine:read`. This page only draws what arrives. The live values
 * are this replica's since it started; the Monitoring link is the estate-wide
 * view, and its history.
 */
import { computed, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ExternalLink, RefreshCw } from "@lucide/vue";

import { adminStats, adminStatsHistory, getRegistryView } from "@/client/sdk.gen";
import type {
  RegistryStatsDto,
  RegistryView,
  StatsHistoryPointDto,
  Suggestion,
} from "@/client/types.gen";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import StatsTrendChart from "@/components/admin/StatsTrendChart.vue";
import { ADMIN_PAGE_VERBS, REGISTRIES_TABS } from "@/config/adminSections";
import { useAuth } from "@/composables/useAuth";
import { formatDate } from "@/lib/format";
import { PageHeader } from "@/components/ui/page-header";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";

const { t } = useI18n();
const route = useRoute();
const router = useRouter();

const name = computed(() => String(route.params.name ?? ""));

const { can } = useAuth();
/** RFC 0046 M4: the pages that act on one registry, in the order an incident needs them. */
const ACTIONS = [
  { to: "/admin/registries/upstream", label: "adminRegistry.actions.upstream" },
  { to: "/admin/registries/warming", label: "adminRegistry.actions.warm" },
  { to: "/admin/system/health", label: "adminRegistry.actions.refreshExplorer" },
  { to: "/admin/registries/imports", label: "adminRegistry.actions.imports" },
  { to: "/admin/system/configuration", label: "adminRegistry.editConfig" },
];
const actions = computed(() =>
  ACTIONS.filter((action) => {
    const verb = ADMIN_PAGE_VERBS[action.to];
    return verb !== undefined && can(verb);
  }),
);

// ── the view ─────────────────────────────────────────────────────────────────

const view = ref<RegistryView | null>(null);
const state = ref<"loading" | "ok" | "missing" | "denied" | "error">("loading");
const loadError = ref("");

async function load() {
  state.value = "loading";
  try {
    const { data, error, response } = await getRegistryView({ path: { name: name.value } });
    if (data && !error) {
      view.value = data;
      state.value = "ok";
      return;
    }
    const status = response?.status;
    if (status === 404) state.value = "missing";
    else if (status === 403) state.value = "denied";
    else {
      loadError.value = t("adminRegistry.loadError", {
        status: response ? `${response.status} ${response.statusText}`.trim() : "—",
      });
      state.value = "error";
    }
  } catch (e) {
    loadError.value = (e as Error).message;
    state.value = "error";
  }
}

/** A config value rendered as an `href` is a script vector unless it is http(s) (T11). */
const safeHref = (u?: string | null) => (u && /^https?:\/\//i.test(u) ? u : undefined);
const monitoring = computed(() => safeHref(view.value?.monitoring_url));

// ── the stats this page borrows (stats:read) ────────────────────────────────

const statsRow = ref<RegistryStatsDto | null>(null);
/** `403` from `/admin/stats`: the Activity tab is not rendered. */
const statsDenied = ref(false);

async function loadStats() {
  try {
    const { data, response } = await adminStats();
    if (response?.status === 403) {
      statsDenied.value = true;
      return;
    }
    statsRow.value = data?.per_registry.find((r) => r.registry === name.value) ?? null;
  } catch {
    // The cache cell reads "—"; the rest of the page does not depend on it.
  }
}

const WINDOWS = ["1", "7", "30"] as const;
const historyWindow = ref<(typeof WINDOWS)[number]>("1");
const points = ref<StatsHistoryPointDto[]>([]);
const historyLoading = ref(false);
const historyError = ref("");

async function loadHistory() {
  historyLoading.value = true;
  historyError.value = "";
  try {
    const { data, error, response } = await adminStatsHistory({
      query: { window: `${historyWindow.value}d` },
    });
    if (error || !data) {
      if (response?.status === 403) statsDenied.value = true;
      throw new Error(
        t("adminRegistry.historyError", {
          status: response ? `${response.status} ${response.statusText}`.trim() : "—",
        }),
      );
    }
    points.value = data.per_registry.find((r) => r.registry === name.value)?.points ?? [];
  } catch (e) {
    historyError.value = (e as Error).message;
  } finally {
    historyLoading.value = false;
  }
}

const requests = computed(() => points.value.map((p) => p.hits + p.misses));
const hitRates = computed(() =>
  points.value.map((p) => (p.hits + p.misses > 0 ? p.hits / (p.hits + p.misses) : null)),
);
const pct = (n: number | null | undefined) => (n == null ? "—" : `${Math.round(n * 100)} %`);

// ── tabs ─────────────────────────────────────────────────────────────────────

const tabs = computed(() => {
  const out = ["overview"];
  if (!statsDenied.value) out.push("activity");
  out.push("scan");
  if (view.value?.config) out.push("config");
  return out;
});

const activeTab = computed<string>({
  get() {
    const asked = Array.isArray(route.query.tab) ? route.query.tab[0] : route.query.tab;
    return typeof asked === "string" && tabs.value.includes(asked) ? asked : "overview";
  },
  set(tab) {
    // `replace`: tabs are not destinations, and must not stack in history.
    void router.replace({ path: route.path, query: { ...route.query, tab } });
  },
});
const tabLink = (tab: string) => ({ path: route.path, query: { ...route.query, tab } });

// ── wording ──────────────────────────────────────────────────────────────────

function every(secs: number): string {
  if (secs % 86_400 === 0) return t("verdictPanel.nextRescanEveryDays", { n: secs / 86_400 });
  if (secs % 3600 === 0) return t("verdictPanel.nextRescanEveryHours", { n: secs / 3600 });
  if (secs % 60 === 0) return t("verdictPanel.nextRescanEveryMinutes", { n: secs / 60 });
  return t("verdictPanel.nextRescanEverySeconds", { n: secs });
}

const rescanInterval = computed(() => view.value?.config?.security?.rescan_interval_secs ?? null);
const has = (code: Suggestion["code"]) =>
  view.value?.suggestions.some((s) => s.code === code) ?? false;
const scansLink = computed(() => ({
  path: "/admin/security/scans",
  query: { registry: name.value },
}));

function suggestionText(s: Suggestion): string {
  switch (s.code) {
    case "degraded_without_stale":
      return t("adminRegistry.suggestDegraded");
    case "security_without_rescan":
      return t("adminRegistry.suggestNoRescan");
    default:
      return t("adminRegistry.suggestFailures", { n: s.count ?? 0 });
  }
}
function suggestionLink(s: Suggestion) {
  if (s.code === "scan_failures") {
    return { path: "/admin/security/scans", query: { tab: "failures", registry: name.value } };
  }
  return tabLink(view.value?.config ? "config" : "overview");
}

const queueText = computed(() => {
  const s = view.value?.scan;
  if (!s?.enabled) return t("adminRegistry.off");
  if (s.queued == null) return "—";
  return t("adminRegistry.queueCounts", {
    queued: s.queued,
    scanning: s.scanning ?? 0,
    failed: s.failed ?? 0,
  });
});

const sbomText = computed(() => {
  const s = view.value?.scan;
  if (!s?.sbom_recheck_enabled) return t("verdictPanel.nextRescanSbomDisabled");
  if (!s.sbom_recheck) return t("adminRegistry.sbomNotYet");
  return t("adminRegistry.sbomLast", {
    when: formatDate(s.sbom_recheck.last_scan_at),
    artifacts: s.sbom_recheck.artifacts_scanned,
    findings: s.sbom_recheck.findings,
  });
});

const authText = computed(() => {
  const a = view.value?.config?.upstream_auth;
  return a?.state === "configured"
    ? t("adminRegistry.authConfigured", { kind: a.kind })
    : t("adminRegistry.authNone");
});

function reloadAll() {
  void load();
  void loadStats();
}

onMounted(reloadAll);
watch(name, reloadAll);
watch(
  [historyWindow, () => activeTab.value === "activity"],
  ([, open]) => {
    if (open) void loadHistory();
  },
  { immediate: true },
);
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="REGISTRIES_TABS" />
    <RouterLink
      to="/admin/registries"
      class="text-sm text-muted-foreground hover:underline underline-offset-[3px]"
      >← {{ t("adminRegistry.back") }}</RouterLink
    >

    <Skeleton v-if="state === 'loading'" :lines="4" />

    <div v-else-if="state === 'missing'" class="space-y-2" data-testid="missing">
      <p class="font-mono text-sm">{{ t("adminRegistry.missing", { name }) }}</p>
      <p class="text-sm text-muted-foreground">{{ t("adminRegistry.missingHint") }}</p>
      <Button as-child size="sm" variant="outline">
        <RouterLink to="/admin/registries">{{ t("adminRegistry.backToList") }}</RouterLink>
      </Button>
    </div>

    <p v-else-if="state === 'denied'" class="text-sm" data-testid="denied">
      {{ t("adminRegistry.denied") }}
    </p>

    <Alert
      v-else-if="state === 'error'"
      variant="destructive"
      class="space-y-2"
      data-testid="error"
    >
      <p class="text-sm">{{ loadError }}</p>
      <Button variant="outline" size="sm" @click="reloadAll">{{ t("common.retry") }}</Button>
    </Alert>

    <template v-else-if="view">
      <PageHeader
        variant="display"
        :title="view.name"
        :description="`${view.registry_type} · ${view.mode}`"
      >
        <template #actions>
          <Button v-if="monitoring" as-child size="sm" variant="outline">
            <a
              :href="monitoring"
              target="_blank"
              rel="noopener noreferrer"
              data-testid="monitoring"
            >
              {{ t("adminRegistry.monitoring") }}
              <ExternalLink class="h-3.5 w-3.5" aria-hidden="true" />
            </a>
          </Button>
          <Button variant="ghost" size="sm" @click="reloadAll">
            <RefreshCw class="h-4 w-4" aria-hidden="true" />
            {{ t("common.refresh") }}
          </Button>
        </template>
      </PageHeader>

      <!-- The strip: the five signals someone opens the page for, from any tab. -->
      <nav
        :aria-label="t('adminRegistry.stripLabel')"
        class="grid grid-cols-2 gap-px border border-border bg-border sm:grid-cols-5"
        data-testid="strip"
      >
        <RouterLink :to="tabLink('activity')" class="bg-background p-3 hover:bg-muted">
          <span class="block text-xs uppercase text-muted-foreground">{{
            t("adminRegistry.upstream")
          }}</span>
          <span
            class="block font-mono text-sm"
            :class="view.live.upstream_degraded ? 'text-destructive' : ''"
            >{{
              view.live.upstream_degraded ? t("adminRegistry.degraded") : t("adminRegistry.ok")
            }}</span
          >
          <span class="block text-xs text-muted-foreground">{{
            t("adminRegistry.thisReplica", { since: formatDate(view.live.since) })
          }}</span>
        </RouterLink>
        <RouterLink :to="tabLink('activity')" class="bg-background p-3 hover:bg-muted">
          <span class="block text-xs uppercase text-muted-foreground">{{
            t("adminRegistry.cache")
          }}</span>
          <span class="block font-mono text-sm" data-testid="strip-cache">{{
            statsRow?.hit_rate != null
              ? t("adminRegistry.hit", { pct: pct(statsRow.hit_rate) })
              : "—"
          }}</span>
        </RouterLink>
        <RouterLink :to="tabLink('scan')" class="bg-background p-3 hover:bg-muted">
          <span class="block text-xs uppercase text-muted-foreground">{{
            t("adminRegistry.scan")
          }}</span>
          <span class="block font-mono text-sm" data-testid="strip-scan">{{
            !view.scan.enabled
              ? t("adminRegistry.off")
              : rescanInterval
                ? t("adminRegistry.rescanEvery", { every: every(rescanInterval) })
                : t("adminRegistry.rescanNever")
          }}</span>
        </RouterLink>
        <RouterLink :to="tabLink('scan')" class="bg-background p-3 hover:bg-muted">
          <span class="block text-xs uppercase text-muted-foreground">{{
            t("adminRegistry.queue")
          }}</span>
          <span class="block font-mono text-sm" data-testid="strip-queue">{{ queueText }}</span>
        </RouterLink>
        <RouterLink :to="tabLink('overview')" class="bg-background p-3 hover:bg-muted">
          <span class="block text-xs uppercase text-muted-foreground">{{
            t("adminRegistry.advice")
          }}</span>
          <span
            class="block font-mono text-sm"
            :class="view.suggestions.length ? 'text-copper' : ''"
            data-testid="strip-advice"
            >{{ view.suggestions.length ? `⚠ ${view.suggestions.length}` : "0" }}</span
          >
        </RouterLink>
      </nav>

      <!--
        RFC 0046 M4: the controls for this registry are links to the pages that
        own them, the registry preselected — one implementation per control. An
        action is drawn only when the guard would open its page (`can(verb)` at
        the instance tier); with none left the strip is absent, not empty.
      -->
      <nav
        v-if="actions.length > 0"
        :aria-label="t('adminRegistry.actionsLabel')"
        class="flex flex-wrap items-baseline gap-x-4 text-sm"
        data-testid="actions"
      >
        <span class="font-mono text-xs uppercase tracking-wider text-muted-foreground">{{
          t("adminRegistry.actionsLabel")
        }}</span>
        <RouterLink
          v-for="action in actions"
          :key="action.to"
          :to="{ path: action.to, query: { registry: view.name } }"
          class="inline-block py-1 underline underline-offset-[3px]"
          >{{ t(action.label) }}</RouterLink
        >
      </nav>

      <Tabs v-model="activeTab">
        <TabsList :aria-label="t('adminRegistry.tabsLabel')" class="flex-wrap h-auto">
          <TabsTrigger value="overview">{{ t("adminRegistry.overview") }}</TabsTrigger>
          <TabsTrigger v-if="tabs.includes('activity')" value="activity">{{
            t("adminRegistry.activity")
          }}</TabsTrigger>
          <TabsTrigger value="scan">{{ t("adminRegistry.scan") }}</TabsTrigger>
          <TabsTrigger v-if="tabs.includes('config')" value="config">{{
            t("adminRegistry.configuration")
          }}</TabsTrigger>
        </TabsList>

        <TabsContent value="overview" class="space-y-4">
          <section class="space-y-2">
            <h2 class="font-mono text-xs uppercase tracking-wider text-muted-foreground">
              {{ t("adminRegistry.suggestions") }}
            </h2>
            <p
              v-if="!view.suggestions.length"
              class="text-sm text-muted-foreground"
              data-testid="no-suggestions"
            >
              {{ t("adminRegistry.noSuggestions") }}
            </p>
            <ul v-else class="space-y-2" data-testid="suggestions">
              <li v-for="s in view.suggestions" :key="s.code" class="text-sm">
                ⚠ {{ suggestionText(s) }}
                <RouterLink
                  :to="suggestionLink(s)"
                  class="text-muted-foreground underline underline-offset-[3px] hover:text-foreground"
                  >→ {{ t("adminRegistry.see") }}</RouterLink
                >
              </li>
            </ul>
          </section>
          <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
            <dt class="text-muted-foreground">{{ t("adminRegistry.activity") }}</dt>
            <dd>
              {{
                statsRow?.hit_rate != null
                  ? t("adminRegistry.hit", { pct: pct(statsRow.hit_rate) })
                  : "—"
              }}
            </dd>
            <dt class="text-muted-foreground">{{ t("adminRegistry.scan") }}</dt>
            <dd>
              {{ view.scan.enabled ? queueText : t("adminRegistry.noScan") }}
            </dd>
            <dt class="text-muted-foreground">{{ t("adminRegistry.configuration") }}</dt>
            <dd>
              {{ view.registry_type }} · {{ view.mode }}
              <template v-if="view.config">
                · {{ t("adminRegistry.upstreamCount", { n: view.config.upstream_count }) }}
              </template>
            </dd>
          </dl>
        </TabsContent>

        <TabsContent v-if="tabs.includes('activity')" value="activity" class="space-y-4">
          <section class="space-y-1 text-sm" data-testid="live">
            <p class="text-muted-foreground">
              {{ t("adminRegistry.liveSince", { since: formatDate(view.live.since) }) }}
            </p>
            <p>
              {{ t("adminRegistry.liveErrors", { pct: pct(view.live.upstream_error_rate) }) }}
              ·
              {{ t("adminRegistry.liveLatency", { ms: view.live.upstream_latency_ms }) }}
              ·
              <span :class="view.live.upstream_degraded ? 'text-destructive' : ''">{{
                view.live.upstream_degraded
                  ? t("adminRegistry.degradedLong")
                  : t("adminRegistry.ok")
              }}</span>
            </p>
          </section>
          <div class="flex flex-wrap gap-2" role="group" :aria-label="t('adminRegistry.window')">
            <Button
              v-for="w in WINDOWS"
              :key="w"
              size="sm"
              :variant="historyWindow === w ? 'default' : 'outline'"
              :aria-pressed="historyWindow === w"
              @click="historyWindow = w"
            >
              {{
                w === "1"
                  ? t("adminRegistry.window24h")
                  : w === "7"
                    ? t("adminRegistry.window7d")
                    : t("adminRegistry.window30d")
              }}
            </Button>
          </div>
          <Skeleton v-if="historyLoading" :lines="2" />
          <Alert
            v-else-if="historyError"
            variant="destructive"
            class="space-y-2"
            data-testid="history-error"
          >
            <p class="text-sm">{{ historyError }}</p>
            <Button variant="outline" size="sm" @click="loadHistory">{{
              t("common.retry")
            }}</Button>
          </Alert>
          <p
            v-else-if="!points.length"
            class="text-sm text-muted-foreground"
            data-testid="no-traffic"
          >
            {{ t("adminRegistry.noTraffic") }}
          </p>
          <Card v-else>
            <CardContent class="space-y-3 p-4">
              <StatsTrendChart
                :label="t('adminRegistry.hitRate')"
                :values="hitRates"
                :max="1"
                :format="(v) => pct(v)"
              />
              <StatsTrendChart :label="t('adminRegistry.requests')" :values="requests" />
            </CardContent>
          </Card>
          <p class="text-xs text-muted-foreground" data-testid="footnote">
            {{
              monitoring ? t("adminRegistry.noErrorHistory") : t("adminRegistry.noErrorHistorySet")
            }}
          </p>
        </TabsContent>

        <TabsContent value="scan" class="space-y-4">
          <p v-if="!view.scan.enabled" class="text-sm" data-testid="no-scan">
            {{ t("adminRegistry.noScan") }}
          </p>
          <dl v-else class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
            <dt class="text-muted-foreground">{{ t("adminRegistry.verdictRescan") }}</dt>
            <dd data-testid="scan-rescan">
              <template v-if="rescanInterval">{{ every(rescanInterval) }}</template>
              <template v-else>{{ t("adminRegistry.rescanNeverLong") }} ⚠</template>
            </dd>
            <dt class="text-muted-foreground">{{ t("adminRegistry.sbom") }}</dt>
            <dd data-testid="scan-sbom">
              {{ sbomText }}
              <span v-if="view.scan.sbom_recheck?.next_at" class="block text-muted-foreground">{{
                t("adminRegistry.sbomNext", { when: formatDate(view.scan.sbom_recheck.next_at) })
              }}</span>
            </dd>
            <dt class="text-muted-foreground">{{ t("adminRegistry.queue") }}</dt>
            <dd>
              {{ queueText }}
              <RouterLink
                :to="scansLink"
                class="ml-2 text-muted-foreground underline underline-offset-[3px] hover:text-foreground"
                >→ {{ t("adminRegistry.scansFiltered") }}</RouterLink
              >
            </dd>
          </dl>
        </TabsContent>

        <TabsContent v-if="view.config" value="config" class="space-y-4">
          <dl class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm" data-testid="config">
            <dt class="text-muted-foreground">{{ t("common.type") }}</dt>
            <dd class="font-mono">{{ view.registry_type }}</dd>
            <dt class="text-muted-foreground">{{ t("adminRegistry.mode") }}</dt>
            <dd class="font-mono">{{ view.mode }}</dd>
            <dt class="text-muted-foreground">{{ t("adminRegistry.upstreams") }}</dt>
            <dd class="font-mono break-all" data-testid="config-upstreams">
              <template v-if="view.config.upstreams">
                <span v-for="u in view.config.upstreams" :key="u" class="block">{{ u }}</span>
                <span v-for="u in view.config.other_urls ?? []" :key="u" class="block">{{
                  u
                }}</span>
              </template>
              <template v-else>{{
                t("adminRegistry.upstreamsHidden", { n: view.config.upstream_count })
              }}</template>
            </dd>
            <dt class="text-muted-foreground">{{ t("adminRegistry.upstreamAuth") }}</dt>
            <dd data-testid="config-auth">{{ authText }}</dd>
            <dt class="text-muted-foreground">{{ t("adminRegistry.cache") }}</dt>
            <dd class="font-mono">
              <span class="block">metadata_ttl_secs {{ view.config.cache.metadata_ttl_secs }}</span>
              <span class="block" data-testid="config-stale"
                >serve_stale {{ view.config.cache.serve_stale }}
                <template v-if="has('degraded_without_stale')">⚠</template></span
              >
              <span class="block"
                >artifact_ttl_secs {{ view.config.cache.artifact_ttl_secs ?? "—" }}</span
              >
            </dd>
            <dt class="text-muted-foreground">{{ t("adminRegistry.rules") }}</dt>
            <dd class="font-mono">{{ view.config.rules.join(" · ") || "—" }}</dd>
            <template v-if="view.config.security">
              <dt class="text-muted-foreground">{{ t("adminRegistry.security") }}</dt>
              <dd class="font-mono" data-testid="config-security">
                <!-- Config keys, verbatim: they are what the operator searches the TOML for. -->
                <span class="block">mode {{ view.config.security.mode }}</span>
                <span class="block">max_severity {{ view.config.security.max_severity }}</span>
                <span class="block">scanners {{ view.config.security.scanners.join(", ") }}</span>
                <span class="block"
                  >rescan.interval_secs {{ view.config.security.rescan_interval_secs ?? "—" }}
                  <template v-if="has('security_without_rescan')">⚠</template></span
                >
              </dd>
            </template>
            <dt class="text-muted-foreground">{{ t("adminRegistry.grants") }}</dt>
            <dd>
              {{
                t("adminRegistry.grantsCount", {
                  subjects: view.config.grant_subjects,
                  namespaces: view.config.namespaces,
                })
              }}
            </dd>
          </dl>
        </TabsContent>
      </Tabs>
    </template>
  </div>
</template>
