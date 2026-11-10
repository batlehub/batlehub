<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { useRoute, useRouter } from "vue-router";
import { RefreshCw } from "@lucide/vue";

import { listRegistries, listScans, listVerdicts, rescanVerdict } from "@/client/sdk.gen";
import type { ScanJobRow, ScanQueuePage, Verdict } from "@/client/types.gen";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import ScanBadge from "@/components/package/ScanBadge.vue";
import { SECURITY_TABS } from "@/config/adminSections";
import { usePoll } from "@/composables/usePoll";
import { formatDate, formatRelative } from "@/lib/format";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { Alert } from "@/components/ui/alert";
import { Announcer } from "@/components/ui/announcer";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Pagination } from "@/components/ui/pagination";
import { Select } from "@/components/ui/select";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

/**
 * The scan queue, its failures and the verdicts by state (RFC 0042 §4.5,
 * phase 2). Rows come filtered by the server: a registry where the caller
 * lacks `quarantine:read` contributes none, and `last_error` needs
 * `findings:read` (T16).
 */
const { t } = useI18n();
const route = useRoute();
const router = useRouter();

const PER_PAGE = 50;
const TABS = ["queue", "failures", "verdicts"] as const;
type Tab = (typeof TABS)[number];

const activeTab = computed<Tab>({
  get() {
    const asked = Array.isArray(route.query.tab) ? route.query.tab[0] : route.query.tab;
    return TABS.includes(asked as Tab) ? (asked as Tab) : "queue";
  },
  set(tab) {
    // `replace`: tabs are not destinations, and must not stack in history.
    void router.replace({ path: route.path, query: { ...route.query, tab } });
  },
});

const registries = ref<string[]>([]);
const registry = ref("");
const queueState = ref("");
const queuePage = ref(0);
const failPage = ref(0);
const queue = ref<ScanQueuePage | null>(null);
const failures = ref<ScanQueuePage | null>(null);
const loading = ref(false);
const loadError = ref<string | null>(null);
const announcement = ref("");
const rescanning = ref<Record<string, boolean>>({});
const rescanError = ref<string | null>(null);

const registryOptions = computed(() => [
  { value: "", label: t("adminScans.allRegistries") },
  ...registries.value.map((r) => ({ value: r, label: r })),
]);
const stateOptions = computed(() => [
  { value: "", label: t("adminScans.stateOpen") },
  { value: "queued", label: t("scanBadge.queued") },
  { value: "scanning", label: t("scanBadge.scanning") },
]);

async function fetchPage(state: string | undefined, page: number): Promise<ScanQueuePage> {
  const { data, error, response } = await listScans({
    query: {
      registry: registry.value || undefined,
      state,
      page: page + 1,
      per_page: PER_PAGE,
    },
  });
  if (error || !data) {
    const status = response ? `${response.status} ${response.statusText}`.trim() : "—";
    throw Object.assign(new Error(t("adminScans.loadError", { status })), { response });
  }
  return data;
}

/** Both lists, so both tab counts are right whichever tab is open. Throws for the poll. */
async function refresh() {
  const [q, f] = await Promise.all([
    fetchPage(queueState.value || undefined, queuePage.value),
    fetchPage("failed", failPage.value),
  ]);
  queue.value = q;
  failures.value = f;
  loadError.value = null;
}

async function load() {
  loading.value = true;
  try {
    await refresh();
  } catch (e) {
    loadError.value = (e as Error).message;
  } finally {
    loading.value = false;
  }
}

// §4.2: poll only while the queue shows a queued or scanning row.
const transient = computed(() => (queue.value?.items.length ?? 0) > 0);
const { stopped, restart } = usePoll(refresh, transient);

function onRefresh() {
  restart();
  void load();
}

watch([registry, queueState], () => {
  queuePage.value = 0;
  failPage.value = 0;
  void load();
});
watch([queuePage, failPage], () => void load());

const coord = (r: { registry: string; name: string; version: string }) =>
  `${r.registry}/${r.name}@${r.version}`;

async function rescan(row: ScanJobRow) {
  const k = coord(row);
  rescanning.value[k] = true;
  rescanError.value = null;
  try {
    const { error } = await rescanVerdict({
      path: { registry: row.registry, name: row.name, version: row.version },
    });
    if (error) throw new Error(t("adminScans.rescanFailed", { coordinate: k }));
    announcement.value = t("adminScans.rescanQueued", { coordinate: k });
    // The row moves from Failures to Queue: re-read both.
    await load();
  } catch (e) {
    rescanError.value = (e as Error).message;
  } finally {
    rescanning.value[k] = false;
  }
}

const totalPages = (p: ScanQueuePage | null) =>
  p ? Math.max(1, Math.ceil(p.total / p.per_page)) : 1;

// ── Verdicts by state ────────────────────────────────────────────────────────

const verdictState = ref("");
const verdicts = ref<Verdict[]>([]);
const verdictsLoading = ref(false);
const verdictsError = ref<string | null>(null);

function verdictWord(state: string): string {
  switch (state) {
    case "warned":
      return t("scanBadge.verdict.warned");
    case "quarantined":
      return t("scanBadge.verdict.quarantined");
    case "denied":
      return t("scanBadge.verdict.denied");
    default:
      return t("scanBadge.verdict.allowed");
  }
}

const verdictStateOptions = computed(() => [
  { value: "", label: t("adminScans.anyState") },
  ...["quarantined", "denied", "warned", "allowed"].map((s) => ({
    value: s,
    label: verdictWord(s),
  })),
]);

async function loadVerdicts() {
  verdicts.value = [];
  verdictsError.value = null;
  if (!registry.value) return;
  verdictsLoading.value = true;
  try {
    const { data, error, response } = await listVerdicts({
      query: { registry: registry.value, state: verdictState.value || undefined },
    });
    if (error || !data) {
      throw new Error(
        t("adminScans.verdictsError", {
          status: response ? `${response.status} ${response.statusText}`.trim() : "—",
        }),
      );
    }
    verdicts.value = data.items;
  } catch (e) {
    verdictsError.value = (e as Error).message;
  } finally {
    verdictsLoading.value = false;
  }
}

watch(
  [registry, verdictState, () => activeTab.value === "verdicts"],
  ([, , open]) => {
    if (open) void loadVerdicts();
  },
  { immediate: true },
);

const verdictsEmpty = computed(() =>
  verdictState.value
    ? t("adminScans.noVerdictsInState", {
        registry: registry.value,
        state: verdictWord(verdictState.value),
      })
    : t("adminScans.noVerdicts", { registry: registry.value }),
);

onMounted(async () => {
  void load();
  try {
    const { data } = await listRegistries();
    registries.value = (data ?? []).map((r) => r.name).sort();
  } catch {
    // The filter falls back to "all"; the lists themselves still load.
  }
});
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="SECURITY_TABS" />
    <Announcer :message="announcement" />
    <PageHeader
      variant="display"
      :title="t('adminScans.title')"
      :description="t('adminScans.description')"
    >
      <template #actions>
        <Button variant="outline" size="sm" :disabled="loading" @click="onRefresh">
          <RefreshCw class="h-4 w-4" aria-hidden="true" />
          {{ loading ? t("common.loading") : t("common.refresh") }}
        </Button>
      </template>
    </PageHeader>

    <div class="flex flex-wrap items-end gap-4">
      <div class="space-y-1">
        <Label for="scans-registry">{{ t("adminScans.registry") }}</Label>
        <Select id="scans-registry" v-model="registry" :options="registryOptions" />
      </div>
      <p v-if="transient && !stopped" class="text-xs text-muted-foreground" data-testid="polling">
        {{ t("adminScans.polling") }}
      </p>
      <p v-else-if="stopped" class="text-xs text-muted-foreground" data-testid="poll-stopped">
        {{ stopped === "failed" ? t("verdictPanel.pollFailed") : t("verdictPanel.pollStopped") }}
      </p>
    </div>

    <Alert v-if="loadError" variant="destructive" class="space-y-2" data-testid="load-error">
      <p class="text-sm">{{ loadError }}</p>
      <Button variant="outline" size="sm" @click="onRefresh">{{ t("common.retry") }}</Button>
    </Alert>

    <Tabs v-model="activeTab">
      <TabsList :aria-label="t('adminScans.tabsLabel')" class="flex-wrap h-auto">
        <TabsTrigger value="queue">{{
          t("adminScans.queueTab", { n: queue?.total ?? 0 })
        }}</TabsTrigger>
        <TabsTrigger value="failures">{{
          t("adminScans.failuresTab", { n: failures?.total ?? 0 })
        }}</TabsTrigger>
        <TabsTrigger value="verdicts">{{ t("adminScans.verdictsTab") }}</TabsTrigger>
      </TabsList>

      <TabsContent value="queue" class="space-y-4">
        <div class="space-y-1">
          <Label for="scans-state">{{ t("adminScans.state") }}</Label>
          <Select id="scans-state" v-model="queueState" :options="stateOptions" />
        </div>
        <AsyncState
          :loading="loading && !queue"
          :empty="!!queue && queue.items.length === 0"
          :empty-message="t('adminScans.queueEmpty')"
        >
          <Card>
            <CardContent class="p-0">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{{ t("adminScans.coordinate") }}</TableHead>
                    <TableHead>{{ t("adminScans.state") }}</TableHead>
                    <TableHead>{{ t("adminScans.trigger") }}</TableHead>
                    <TableHead>{{ t("adminScans.since") }}</TableHead>
                    <TableHead>{{ t("adminScans.worker") }}</TableHead>
                    <TableHead>{{ t("adminScans.attempts") }}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  <TableRow v-for="row in queue?.items ?? []" :key="coord(row)">
                    <TableCell class="font-mono text-xs">{{ coord(row) }}</TableCell>
                    <TableCell>
                      <ScanBadge :status="{ state: row.state }" />
                      <div v-if="row.deferred_since" class="text-xs text-muted-foreground">
                        {{
                          t("adminScans.deferredSince", { when: formatDate(row.deferred_since) })
                        }}
                      </div>
                    </TableCell>
                    <TableCell>{{ row.trigger }}</TableCell>
                    <TableCell :title="formatDate(row.since)">{{
                      formatRelative(row.since)
                    }}</TableCell>
                    <TableCell>{{ row.leased_by ?? "—" }}</TableCell>
                    <TableCell>{{ row.attempts }}</TableCell>
                  </TableRow>
                </TableBody>
              </Table>
            </CardContent>
          </Card>
          <Pagination
            v-if="queue && queue.total > queue.per_page"
            v-model:page="queuePage"
            :total-pages="totalPages(queue)"
          />
        </AsyncState>
      </TabsContent>

      <TabsContent value="failures" class="space-y-4">
        <p v-if="rescanError" class="text-sm text-destructive" data-testid="rescan-error">
          {{ rescanError }}
        </p>
        <AsyncState
          :loading="loading && !failures"
          :empty="!!failures && failures.items.length === 0"
          :empty-message="t('adminScans.failuresEmpty')"
        >
          <Card>
            <CardContent class="p-0">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{{ t("adminScans.coordinate") }}</TableHead>
                    <TableHead>{{ t("adminScans.attempts") }}</TableHead>
                    <TableHead>{{ t("adminScans.lastError") }}</TableHead>
                    <TableHead>{{ t("adminScans.since") }}</TableHead>
                    <TableHead>{{ t("common.actions") }}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  <TableRow v-for="row in failures?.items ?? []" :key="coord(row)">
                    <TableCell class="font-mono text-xs">{{ coord(row) }}</TableCell>
                    <TableCell>{{ row.attempts }}</TableCell>
                    <TableCell class="text-xs break-words">
                      <span v-if="row.last_error">{{ row.last_error }}</span>
                      <span v-else-if="row.error_withheld" class="text-muted-foreground">{{
                        t("adminScans.errorWithheld")
                      }}</span>
                      <span v-else class="text-muted-foreground">{{
                        t("adminScans.noErrorRecorded")
                      }}</span>
                    </TableCell>
                    <TableCell :title="formatDate(row.since)">{{
                      formatRelative(row.since)
                    }}</TableCell>
                    <TableCell>
                      <Button
                        v-if="row.can_rescan"
                        size="sm"
                        variant="outline"
                        :disabled="rescanning[coord(row)]"
                        @click="rescan(row)"
                      >
                        {{ t("adminScans.rescan") }}
                      </Button>
                    </TableCell>
                  </TableRow>
                </TableBody>
              </Table>
            </CardContent>
          </Card>
          <Pagination
            v-if="failures && failures.total > failures.per_page"
            v-model:page="failPage"
            :total-pages="totalPages(failures)"
          />
        </AsyncState>
      </TabsContent>

      <TabsContent value="verdicts" class="space-y-4">
        <p v-if="!registry" class="text-sm text-muted-foreground" data-testid="pick-registry">
          {{ t("adminScans.pickRegistry") }}
        </p>
        <template v-else>
          <div class="space-y-1">
            <Label for="verdicts-state">{{ t("adminScans.state") }}</Label>
            <Select id="verdicts-state" v-model="verdictState" :options="verdictStateOptions" />
          </div>
          <AsyncState
            :loading="verdictsLoading"
            :error="verdictsError"
            :empty="verdicts.length === 0"
            :empty-message="verdictsEmpty"
            :on-retry="loadVerdicts"
          >
            <Card>
              <CardContent class="p-0">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>{{ t("adminScans.coordinate") }}</TableHead>
                      <TableHead>{{ t("adminScans.verdict") }}</TableHead>
                      <TableHead>{{ t("adminScans.reasons") }}</TableHead>
                      <TableHead>{{ t("adminScans.evaluated") }}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    <TableRow v-for="v in verdicts" :key="coord(v.package)">
                      <TableCell class="font-mono text-xs">{{ coord(v.package) }}</TableCell>
                      <TableCell
                        ><Badge variant="outline">{{ verdictWord(v.state) }}</Badge></TableCell
                      >
                      <TableCell class="text-xs">{{ v.reason_codes.join(", ") || "—" }}</TableCell>
                      <TableCell>{{ formatDate(v.evaluated_at) }}</TableCell>
                    </TableRow>
                  </TableBody>
                </Table>
              </CardContent>
            </Card>
          </AsyncState>
        </template>
      </TabsContent>
    </Tabs>
  </div>
</template>
