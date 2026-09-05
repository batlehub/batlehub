<script setup lang="ts">
/**
 * RFC 0014 phase 8 — what the upstream audit knows.
 *
 * The rows the sweep holds: packages seen missing at their upstream and
 * packages confirmed gone, with the policy the instance applies to a
 * confirmation stated at the top, so a blocked-package question never starts
 * with "check the config file". *Recheck* probes one package now, through the
 * same ladder and state machine as the sweep.
 */
import { useI18n } from "vue-i18n";
import { ref, computed, onMounted, watch } from "vue";
import { useAuthFetch } from "@/composables/useAuthFetch";
import { extractMessage } from "@/composables/useApi";
import { API_BASE_URL } from "@/config";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { OPERATIONS_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Announcer } from "@/components/ui/announcer";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";

const { t } = useI18n();
const { authFetch } = useAuthFetch();

export interface UpstreamStatusSummary {
  registry: string;
  package_name: string;
  version?: string | null;
  state: "missing" | "disappeared";
  first_missed_at: string;
  last_checked_at: string;
  confirmed_at?: string | null;
  consecutive_misses: number;
  last_error?: string | null;
}

export interface UpstreamStatusPage {
  items: UpstreamStatusSummary[];
  total: number;
  page: number;
  per_page: number;
  policy: "audit" | "block";
  registries: string[];
  counts: {
    registry: string;
    missing: number;
    disappeared: number;
    /** RFC 0014 §13 O6: the registry's own `on_confirmed` row, else `policy`. */
    policy: "audit" | "block";
    overridden: boolean;
  }[];
}

interface RecheckResponse {
  probed: number;
  missing: number;
  inconclusive: number;
  transitions: string[];
  status?: UpstreamStatusSummary | null;
}

const data = ref<UpstreamStatusPage | null>(null);
const loading = ref(false);
/** `null` while nothing failed; `"off"` when the audit is not running here. */
const error = ref<string | null>(null);
const notRunning = ref(false);

const registryFilter = ref("");
const stateFilter = ref<"" | "missing" | "disappeared">("");
const page = ref(0);

const rechecking = ref<string | null>(null);
const recheckResult = ref<Record<string, string>>({});
const announcement = ref("");

const rowKey = (r: UpstreamStatusSummary) => `${r.registry}/${r.package_name}@${r.version ?? "*"}`;

/* Literal keys, so the catalogue test can see every one is referenced. */
const POLICY_KEYS: Record<string, string> = {
  audit: "adminUpstream.policy.audit",
  block: "adminUpstream.policy.block",
};
const POLICY_HELP_KEYS: Record<string, string> = {
  audit: "adminUpstream.policyHelp.audit",
  block: "adminUpstream.policyHelp.block",
};
const STATE_KEYS: Record<string, string> = {
  missing: "adminUpstream.state.missing",
  disappeared: "adminUpstream.state.disappeared",
};
const RECHECK_KEYS: Record<string, string> = {
  confirmed: "adminUpstream.recheck.confirmed",
  reappeared: "adminUpstream.recheck.reappeared",
};

async function load() {
  loading.value = true;
  error.value = null;
  try {
    const params = new URLSearchParams();
    if (registryFilter.value) params.set("registry", registryFilter.value);
    if (stateFilter.value) params.set("state", stateFilter.value);
    params.set("page", String(page.value));
    const res = await authFetch(`${API_BASE_URL}/api/v1/admin/upstream/disappeared?${params}`);
    if (res.status === 503) {
      notRunning.value = true;
      data.value = null;
      return;
    }
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    notRunning.value = false;
    data.value = (await res.json()) as UpstreamStatusPage;
  } catch (e) {
    error.value = extractMessage(e);
  } finally {
    loading.value = false;
  }
}

async function recheck(row: UpstreamStatusSummary) {
  const key = rowKey(row);
  rechecking.value = key;
  try {
    const res = await authFetch(`${API_BASE_URL}/api/v1/admin/upstream/recheck`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        registry: row.registry,
        package_name: row.package_name,
        version: row.version ?? undefined,
      }),
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const out = (await res.json()) as RecheckResponse;
    const outcome =
      out.transitions.length > 0
        ? t(RECHECK_KEYS[out.transitions[0]] ?? "adminUpstream.recheck.confirmed")
        : out.status
          ? t("adminUpstream.recheck.stillMissing", { misses: out.status.consecutive_misses })
          : t("adminUpstream.recheck.present");
    recheckResult.value = { ...recheckResult.value, [key]: outcome };
    announcement.value = `${row.package_name}: ${outcome}`;
    await load();
  } catch (e) {
    const msg = extractMessage(e);
    recheckResult.value = { ...recheckResult.value, [key]: msg };
    announcement.value = msg;
  } finally {
    rechecking.value = null;
  }
}

const pageCount = computed(() =>
  data.value ? Math.max(1, Math.ceil(data.value.total / Math.max(1, data.value.per_page))) : 1,
);

watch([registryFilter, stateFilter], () => {
  page.value = 0;
  void load();
});
watch(page, () => void load());
onMounted(load);

const fmt = (iso?: string | null) => (iso ? new Date(iso).toLocaleString() : "—");
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="OPERATIONS_TABS" />
    <PageHeader
      :title="t('adminUpstream.title')"
      :description="t('adminUpstream.description')"
      variant="display"
    >
      <template #actions>
        <Button variant="outline" size="sm" :disabled="loading" @click="load">
          {{ loading ? t("adminUpstream.refreshing") : t("common.refresh") }}
        </Button>
      </template>
    </PageHeader>
    <Announcer :message="announcement" />

    <!-- The audit is not running in this process: the honest empty state,
         not an empty table that reads as "nothing has disappeared". -->
    <Card v-if="notRunning" data-testid="upstream-not-running">
      <CardContent class="py-6 text-sm text-muted-foreground">
        {{ t("adminUpstream.notRunning") }}
      </CardContent>
    </Card>

    <AsyncState v-else :loading="loading && !data" :error="error" :empty="false">
      <template v-if="data">
        <!-- The policy, first. -->
        <Card data-testid="upstream-policy">
          <CardHeader class="pb-2">
            <CardTitle class="text-base">{{ t("adminUpstream.policyTitle") }}</CardTitle>
          </CardHeader>
          <CardContent class="flex flex-wrap items-center gap-3 text-sm">
            <Badge :variant="data.policy === 'block' ? 'destructive' : 'secondary'">
              {{ t(POLICY_KEYS[data.policy]) }}
            </Badge>
            <span class="text-muted-foreground">
              {{ t(POLICY_HELP_KEYS[data.policy]) }}
            </span>
            <span
              v-for="c in data.counts"
              :key="c.registry"
              class="inline-flex items-center gap-1 font-mono text-xs"
              data-testid="upstream-count"
            >
              {{ c.registry }}:
              {{ t("adminUpstream.counts", { missing: c.missing, disappeared: c.disappeared }) }}
              <!-- A registry whose own row differs from the estate's key says
                   so beside its counts (RFC 0014 §13 O6), so "block" above
                   is never read as "everywhere". -->
              <Badge
                v-if="c.overridden"
                :variant="c.policy === 'block' ? 'destructive' : 'secondary'"
                :title="t('adminUpstream.policyOverride')"
                data-testid="upstream-count-policy"
              >
                {{ t(POLICY_KEYS[c.policy]) }}
              </Badge>
            </span>
          </CardContent>
        </Card>

        <!-- Filters -->
        <div class="flex flex-wrap items-end gap-3">
          <label class="flex flex-col gap-1 text-xs">
            <span>{{ t("adminUpstream.filterRegistry") }}</span>
            <select
              v-model="registryFilter"
              class="h-9 rounded-md border bg-background px-2 text-sm"
              data-testid="upstream-registry-filter"
            >
              <option value="">{{ t("adminUpstream.allRegistries") }}</option>
              <option v-for="r in data.registries" :key="r" :value="r">{{ r }}</option>
            </select>
          </label>
          <label class="flex flex-col gap-1 text-xs">
            <span>{{ t("adminUpstream.filterState") }}</span>
            <select
              v-model="stateFilter"
              class="h-9 rounded-md border bg-background px-2 text-sm"
              data-testid="upstream-state-filter"
            >
              <option value="">{{ t("adminUpstream.allStates") }}</option>
              <option value="missing">{{ t("adminUpstream.state.missing") }}</option>
              <option value="disappeared">{{ t("adminUpstream.state.disappeared") }}</option>
            </select>
          </label>
        </div>

        <!-- The first sweep after enabling finds nothing, by design: every
             miss starts at one. Say so, rather than render a table that
             looks like a working audit with nothing to report. -->
        <Card v-if="data.items.length === 0" data-testid="upstream-empty">
          <CardContent class="py-6 text-sm text-muted-foreground">
            {{ t("adminUpstream.empty") }}
          </CardContent>
        </Card>

        <Card v-else>
          <CardContent class="overflow-x-auto p-0">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{{ t("adminUpstream.col.package") }}</TableHead>
                  <TableHead>{{ t("adminUpstream.col.state") }}</TableHead>
                  <TableHead>{{ t("adminUpstream.col.misses") }}</TableHead>
                  <TableHead>{{ t("adminUpstream.col.firstMissed") }}</TableHead>
                  <TableHead>{{ t("adminUpstream.col.lastChecked") }}</TableHead>
                  <TableHead>{{ t("adminUpstream.col.confirmed") }}</TableHead>
                  <TableHead />
                </TableRow>
              </TableHeader>
              <TableBody>
                <TableRow v-for="row in data.items" :key="rowKey(row)" data-testid="upstream-row">
                  <TableCell class="font-mono text-xs">
                    <span class="text-muted-foreground">{{ row.registry }}/</span
                    >{{ row.package_name }}<span v-if="row.version">@{{ row.version }}</span>
                    <span v-else class="text-muted-foreground">
                      {{ t("adminUpstream.wholePackage") }}</span
                    >
                    <p
                      v-if="row.last_error"
                      class="mt-1 max-w-[48ch] truncate text-muted-foreground"
                      :title="row.last_error"
                    >
                      {{ row.last_error }}
                    </p>
                  </TableCell>
                  <TableCell>
                    <Badge :variant="row.state === 'disappeared' ? 'destructive' : 'copper'">
                      {{ t(STATE_KEYS[row.state]) }}
                    </Badge>
                  </TableCell>
                  <TableCell class="tabular-nums">{{ row.consecutive_misses }}</TableCell>
                  <TableCell class="text-xs">{{ fmt(row.first_missed_at) }}</TableCell>
                  <TableCell class="text-xs">{{ fmt(row.last_checked_at) }}</TableCell>
                  <TableCell class="text-xs">{{ fmt(row.confirmed_at) }}</TableCell>
                  <TableCell class="text-right">
                    <Button
                      size="sm"
                      variant="outline"
                      :disabled="rechecking === rowKey(row)"
                      data-testid="upstream-recheck"
                      @click="recheck(row)"
                    >
                      {{
                        rechecking === rowKey(row)
                          ? t("adminUpstream.rechecking")
                          : t("adminUpstream.recheckButton")
                      }}
                    </Button>
                    <p v-if="recheckResult[rowKey(row)]" class="mt-1 text-xs text-muted-foreground">
                      {{ recheckResult[rowKey(row)] }}
                    </p>
                  </TableCell>
                </TableRow>
              </TableBody>
            </Table>
          </CardContent>
        </Card>

        <div v-if="pageCount > 1" class="flex items-center gap-2 text-sm">
          <Button size="sm" variant="outline" :disabled="page === 0" @click="page = page - 1">
            {{ t("common.previous") }}
          </Button>
          <span class="tabular-nums">{{
            t("adminUpstream.pageOf", { page: page + 1, count: pageCount })
          }}</span>
          <Button
            size="sm"
            variant="outline"
            :disabled="page + 1 >= pageCount"
            @click="page = page + 1"
          >
            {{ t("common.next") }}
          </Button>
        </div>
      </template>
    </AsyncState>
  </div>
</template>
