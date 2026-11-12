<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { usePreselectRegistry } from "@/composables/usePreselectRegistry";
import { useI18n } from "vue-i18n";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { RefreshCw } from "@lucide/vue";

import { listFindings, listRegistries, setGateExemption } from "@/client/sdk.gen";
import type { FindingListRow, FindingsPage } from "@/client/types.gen";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { SECURITY_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { Alert } from "@/components/ui/alert";
import { Announcer } from "@/components/ui/announcer";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Dialog } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Pagination } from "@/components/ui/pagination";
import { Select } from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

/**
 * Findings across the registries the caller reads them on (RFC 0044 §4.5,
 * phase 3). The server scopes the rows: `findings:read` per registry, then a
 * namespace re-check per row, so a page can hold fewer than `per_page` rows.
 */
const { t } = useI18n();
const route = useRoute();
const router = useRouter();

const PER_PAGE = 50;
const SEVERITIES = ["unknown", "low", "medium", "high", "critical"] as const;
const VERDICTS = ["denied", "quarantined", "warned", "allowed"] as const;
/** The verdict exemption gate (RFC 0018 §4.1): it lifts a hold to `warned`. */
const EXEMPT_GATE = "security_verdict";

const byReference = computed<boolean>({
  get() {
    const asked = Array.isArray(route.query.group) ? route.query.group[0] : route.query.group;
    return asked === "reference";
  },
  set(on) {
    void router.replace({
      path: route.path,
      query: { ...route.query, group: on ? "reference" : "finding" },
    });
  },
});

const registries = ref<string[]>([]);
const registriesLoaded = ref(false);
const registry = ref("");
// RFC 0046 §6.3 (RP-4): `?registry=` preselects the filter once the list is in.
usePreselectRegistry(
  () => registries.value,
  () => registriesLoaded.value,
  (name) => {
    registry.value = name;
  },
);
const severity = ref("");
const scanner = ref("");
const reference = ref("");
const verdict = ref("");
const page = ref(0);
const result = ref<FindingsPage | null>(null);
const loading = ref(false);
const loadError = ref<string | null>(null);
const refused = ref(false);
const announcement = ref("");

// Keys spelled out: the catalogue gate cannot see a key assembled at runtime.
function verdictWord(s: string): string {
  switch (s) {
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
function severityWord(s: string): string {
  switch (s) {
    case "critical":
      return t("adminFindings.severityCritical");
    case "high":
      return t("adminFindings.severityHigh");
    case "medium":
      return t("adminFindings.severityMedium");
    case "low":
      return t("adminFindings.severityLow");
    default:
      return t("adminFindings.severityUnknown");
  }
}

const registryOptions = computed(() => [
  { value: "", label: t("adminScans.allRegistries") },
  ...registries.value.map((r) => ({ value: r, label: r })),
]);
const severityOptions = computed(() => [
  { value: "", label: t("adminFindings.anySeverity") },
  ...SEVERITIES.slice(1).map((s) => ({
    value: s,
    label: t("adminFindings.atLeast", { severity: severityWord(s) }),
  })),
]);
const verdictOptions = computed(() => [
  { value: "", label: t("adminScans.anyState") },
  ...VERDICTS.map((s) => ({ value: s, label: verdictWord(s) })),
]);

async function load() {
  loading.value = true;
  try {
    const { data, error, response } = await listFindings({
      query: {
        registry: registry.value || undefined,
        severity: severity.value || undefined,
        scanner: scanner.value.trim() || undefined,
        reference: reference.value.trim() || undefined,
        verdict: verdict.value || undefined,
        group: byReference.value ? "reference" : undefined,
        page: page.value + 1,
        per_page: PER_PAGE,
      },
    });
    refused.value = response?.status === 403;
    if (error || !data) {
      const status = response ? `${response.status} ${response.statusText}`.trim() : "—";
      throw new Error(t("adminFindings.loadError", { status }));
    }
    result.value = data;
    loadError.value = null;
  } catch (e) {
    result.value = null;
    loadError.value = (e as Error).message;
  } finally {
    loading.value = false;
  }
}

watch([registry, severity, verdict, byReference], () => {
  page.value = 0;
  void load();
});
watch(page, () => void load());

// Typed filters wait for the typing to stop.
let typing: ReturnType<typeof setTimeout> | undefined;
watch([scanner, reference], () => {
  clearTimeout(typing);
  typing = setTimeout(() => {
    page.value = 0;
    void load();
  }, 300);
});

const coord = (r: FindingListRow) => `${r.registry}/${r.name}@${r.version}`;
const packageHref = (r: FindingListRow) =>
  `/packages/${encodeURIComponent(r.registry)}/${encodeURIComponent(r.name)}`;

function severityVariant(s: string) {
  if (s === "critical") return "destructive";
  if (s === "high") return "copper";
  return "outline";
}
/** VerdictPanel's mapping, so the two never disagree. */
function verdictVariant(s: string) {
  switch (s) {
    case "denied":
      return "destructive";
    case "quarantined":
      return "copper";
    case "warned":
      return "outline";
    default:
      return "secondary";
  }
}

/** Consecutive rows sharing a reference; the server orders by reference. Page-local. */
const groups = computed(() => {
  const out: { reference: string; rows: FindingListRow[] }[] = [];
  for (const row of result.value?.items ?? []) {
    const ref = row.reference ?? "";
    const last = out.at(-1);
    if (last && last.reference === ref) last.rows.push(row);
    else out.push({ reference: ref, rows: [row] });
  }
  return out;
});
const maxSeverity = (rows: FindingListRow[]) =>
  rows.reduce(
    (m, r) => (SEVERITIES.indexOf(r.severity) > SEVERITIES.indexOf(m) ? r.severity : m),
    rows[0]!.severity,
  );
const scanners = (rows: FindingListRow[]) => [...new Set(rows.map((r) => r.scanner))].join(", ");

const totalPages = computed(() =>
  result.value ? Math.max(1, Math.ceil(result.value.total / result.value.per_page)) : 1,
);

// ── [Exempt…] ────────────────────────────────────────────────────────────────

const exempting = ref<FindingListRow | null>(null);
const exemptReason = ref("");
const exemptUntil = ref("");
const exemptError = ref<string | null>(null);
const exemptBusy = ref(false);
const today = () => new Date().toISOString().slice(0, 10);

function openExempt(row: FindingListRow) {
  exempting.value = row;
  exemptReason.value = "";
  exemptUntil.value = "";
  exemptError.value = null;
}

async function submitExempt() {
  const row = exempting.value;
  if (!row) return;
  if (!exemptReason.value.trim()) {
    exemptError.value = t("adminFindings.reasonRequired");
    return;
  }
  if (!exemptUntil.value || exemptUntil.value <= today()) {
    exemptError.value = t("adminFindings.expiryFuture");
    return;
  }
  exemptBusy.value = true;
  exemptError.value = null;
  try {
    const { error } = await setGateExemption({
      path: {
        registry: row.registry,
        package: row.name,
        version: row.version,
        gate: EXEMPT_GATE,
      },
      body: { reason: exemptReason.value.trim(), exempt_until: `${exemptUntil.value}T23:59:59Z` },
    });
    if (error) {
      const msg = (error as { message?: string }).message;
      throw new Error(msg || t("adminFindings.exemptFailed", { coordinate: coord(row) }));
    }
    exempting.value = null;
    announcement.value = t("adminFindings.exempted", { coordinate: coord(row) });
    await load();
  } catch (e) {
    exemptError.value = (e as Error).message;
  } finally {
    exemptBusy.value = false;
  }
}

onMounted(async () => {
  void load();
  try {
    const { data } = await listRegistries();
    registries.value = (data ?? []).map((r) => r.name).sort((a, b) => a.localeCompare(b));
    registriesLoaded.value = true;
  } catch {
    // The filter falls back to "all"; the list itself still loads.
  }
});
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="SECURITY_TABS" />
    <Announcer :message="announcement" />
    <PageHeader
      variant="display"
      :title="t('adminFindings.title')"
      :description="t('adminFindings.description')"
    >
      <template #actions>
        <Button variant="outline" size="sm" :disabled="loading" @click="load">
          <RefreshCw class="h-4 w-4" aria-hidden="true" />
          {{ loading ? t("common.loading") : t("common.refresh") }}
        </Button>
      </template>
    </PageHeader>

    <p v-if="refused" class="text-sm text-muted-foreground" data-testid="refused">
      {{ t("adminFindings.refused") }}
    </p>

    <template v-else>
      <div class="flex flex-wrap items-end gap-4">
        <div class="space-y-1">
          <Label for="findings-registry">{{ t("adminScans.registry") }}</Label>
          <Select id="findings-registry" v-model="registry" :options="registryOptions" />
        </div>
        <div class="space-y-1">
          <Label for="findings-severity">{{ t("adminFindings.severityLabel") }}</Label>
          <Select id="findings-severity" v-model="severity" :options="severityOptions" />
        </div>
        <div class="space-y-1">
          <Label for="findings-scanner">{{ t("adminFindings.scanner") }}</Label>
          <Input id="findings-scanner" v-model="scanner" class="w-32" />
        </div>
        <div class="space-y-1">
          <Label for="findings-reference">{{ t("adminFindings.reference") }}</Label>
          <Input
            id="findings-reference"
            v-model="reference"
            maxlength="128"
            placeholder="GHSA-…"
            class="w-48"
          />
        </div>
        <div class="space-y-1">
          <Label for="findings-verdict">{{ t("adminScans.verdict") }}</Label>
          <Select id="findings-verdict" v-model="verdict" :options="verdictOptions" />
        </div>
        <div class="flex gap-1" role="group" :aria-label="t('adminFindings.groupLabel')">
          <Button
            size="sm"
            :variant="byReference ? 'outline' : 'default'"
            :aria-pressed="!byReference"
            data-testid="group-finding"
            @click="byReference = false"
          >
            {{ t("adminFindings.byFinding") }}
          </Button>
          <Button
            size="sm"
            :variant="byReference ? 'default' : 'outline'"
            :aria-pressed="byReference"
            data-testid="group-reference"
            @click="byReference = true"
          >
            {{ t("adminFindings.byReference") }}
          </Button>
        </div>
      </div>

      <Alert v-if="loadError" variant="destructive" class="space-y-2" data-testid="load-error">
        <p class="text-sm">{{ loadError }}</p>
        <Button variant="outline" size="sm" @click="load">{{ t("common.retry") }}</Button>
      </Alert>

      <AsyncState
        v-else
        :loading="loading && !result"
        :empty="!!result && result.items.length === 0"
        :empty-message="t('adminFindings.empty')"
      >
        <Card>
          <CardContent class="p-0">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{{ t("adminScans.coordinate") }}</TableHead>
                  <TableHead>{{ t("adminFindings.severityLabel") }}</TableHead>
                  <TableHead>{{ t("adminFindings.scanner") }}</TableHead>
                  <TableHead>{{ t("adminFindings.reference") }}</TableHead>
                  <TableHead>{{ t("adminFindings.summary") }}</TableHead>
                  <TableHead>{{ t("adminScans.verdict") }}</TableHead>
                  <TableHead>{{ t("common.actions") }}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                <template v-for="(g, gi) in groups" :key="gi">
                  <TableRow v-if="byReference" class="bg-muted/40" data-testid="group-header">
                    <TableCell colspan="7" class="text-sm">
                      <span class="font-mono font-medium">{{
                        g.reference || t("adminFindings.noReference")
                      }}</span>
                      ·
                      <Badge :variant="severityVariant(maxSeverity(g.rows))">{{
                        severityWord(maxSeverity(g.rows))
                      }}</Badge>
                      · {{ scanners(g.rows) }} ·
                      {{ t("adminFindings.versionsOnPage", { n: g.rows.length }) }}
                    </TableCell>
                  </TableRow>
                  <TableRow
                    v-for="row in g.rows"
                    :key="`${coord(row)}|${row.scanner}|${row.reference ?? ''}`"
                    data-testid="finding-row"
                  >
                    <TableCell class="font-mono text-xs">
                      <RouterLink :to="packageHref(row)" class="hover:underline">{{
                        coord(row)
                      }}</RouterLink>
                    </TableCell>
                    <TableCell>
                      <Badge :variant="severityVariant(row.severity)">{{
                        severityWord(row.severity)
                      }}</Badge>
                    </TableCell>
                    <TableCell>{{ row.scanner }}</TableCell>
                    <TableCell class="font-mono text-xs">{{ row.reference ?? "—" }}</TableCell>
                    <TableCell class="text-xs break-words">{{ row.summary }}</TableCell>
                    <TableCell>
                      <Badge :variant="verdictVariant(row.verdict)">{{
                        verdictWord(row.verdict)
                      }}</Badge>
                    </TableCell>
                    <TableCell>
                      <Button
                        v-if="row.can_exempt"
                        size="sm"
                        variant="outline"
                        data-testid="exempt"
                        @click="openExempt(row)"
                      >
                        {{ t("adminFindings.exempt") }}
                      </Button>
                    </TableCell>
                  </TableRow>
                </template>
              </TableBody>
            </Table>
          </CardContent>
        </Card>
        <Pagination
          v-if="result && result.total > result.per_page"
          v-model:page="page"
          :total-pages="totalPages"
        />
      </AsyncState>
    </template>

    <Dialog :open="!!exempting" @update:open="(o) => !o && (exempting = null)">
      <template #title>{{ t("adminFindings.exemptTitle") }}</template>
      <template #description>
        {{
          t("adminFindings.exemptDescription", {
            coordinate: exempting ? coord(exempting) : "",
          })
        }}
      </template>
      <form class="space-y-4" data-testid="exempt-form" @submit.prevent="submitExempt">
        <div class="space-y-1">
          <Label for="exempt-reason">{{ t("adminFindings.reason") }}</Label>
          <textarea
            id="exempt-reason"
            v-model="exemptReason"
            rows="3"
            required
            class="flex w-full rounded-sm border border-input bg-transparent px-3 py-2 text-sm resize-y"
          />
        </div>
        <div class="space-y-1">
          <Label for="exempt-until">{{ t("adminFindings.expiry") }}</Label>
          <Input id="exempt-until" v-model="exemptUntil" type="date" required />
        </div>
        <p v-if="exemptError" class="text-sm text-destructive" data-testid="exempt-error">
          {{ exemptError }}
        </p>
        <div class="flex justify-end gap-2">
          <Button type="button" variant="outline" @click="exempting = null">{{
            t("common.cancel")
          }}</Button>
          <Button type="submit" :disabled="exemptBusy">{{
            t("adminFindings.exemptConfirm")
          }}</Button>
        </div>
      </form>
    </Dialog>
  </div>
</template>
