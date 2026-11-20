<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { useRoute, useRouter } from "vue-router";
import { RefreshCw } from "@lucide/vue";

import { listRegistries, listScans, rescanVerdict } from "@/client/sdk.gen";
import type { ScanJobRow, ScanQueuePage } from "@/client/types.gen";
import ScanBadge from "@/components/package/ScanBadge.vue";
import { usePoll } from "@/composables/usePoll";
import { formatDate, formatRelative } from "@/lib/format";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { Alert } from "@/components/ui/alert";
import { Announcer } from "@/components/ui/announcer";
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
import ScannerHealthBlock from "@/components/admin/ScannerHealthBlock.vue";

/**
 * The scan queue and its failures (RFC 0044 §4.5,
 * phase 2). Rows come filtered by the server: a registry where the caller
 * lacks `quarantine:read` contributes none, and `last_error` needs
 * `findings:read` (T16).
 */
const { t } = useI18n();
const route = useRoute();
const router = useRouter();

const PER_PAGE = 50;
const TABS = ["queue", "failures"] as const;
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
// `?registry=` preselects the filter: the registry page links here filtered.
const registry = ref(typeof route.query.registry === "string" ? route.query.registry : "");
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

onMounted(async () => {
  void load();
  try {
    const { data } = await listRegistries();
    registries.value = (data ?? []).map((r) => r.name).sort((a, b) => a.localeCompare(b));
  } catch {
    // The filter falls back to "all"; the lists themselves still load.
  }
});
</script>

<template>
  <div class="space-y-6">
    <Announcer :message="announcement" />
    <PageHeader
      variant="display"
      :title="t('securityNav.scanners')"
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

    <!-- RFC 0050 §4.5: whether anything is scanning, before the queue. -->
    <ScannerHealthBlock />

    <Tabs v-model="activeTab">
      <TabsList :aria-label="t('adminScans.tabsLabel')" class="flex-wrap h-auto">
        <TabsTrigger value="queue">{{
          t("adminScans.queueTab", { n: queue?.total ?? 0 })
        }}</TabsTrigger>
        <TabsTrigger value="failures">{{
          t("adminScans.failuresTab", { n: failures?.total ?? 0 })
        }}</TabsTrigger>
      </TabsList>

      <TabsContent value="queue" class="space-y-4">
        <!-- RFC 0050 §6.7: no filter over an empty, unfiltered queue. -->
        <div v-if="queueState || (queue?.total ?? 0) > 0" class="space-y-1">
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
    </Tabs>
  </div>
</template>
