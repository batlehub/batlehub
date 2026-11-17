<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink, useRoute, useRouter } from "vue-router";

import { listVerdicts, securityCoverage } from "@/client/sdk.gen";
import type { Verdict } from "@/client/types.gen";
import { Resolution } from "@/components/ui/resolution";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { AsyncState } from "@/components/ui/async-state";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";
import DateTime from "@/components/ui/date-time/DateTime.vue";
import CodeLabel from "@/components/admin/CodeLabel.vue";
import { packagePath } from "@/config/navigation";
import { HOLD_LABEL_KEYS, HOLD_RESOLUTION } from "@/config/verdict";
import { REASON_LABEL_KEYS } from "@/config/codeLabels";

/**
 * Every held version, page by page, with the exact total: what a SOC walks in
 * a mass compromise (`?reason=SOC_VERDICT` lists all of a SOC's hard blocks,
 * however many). The filters live in the URL, so a link can open on them.
 */
const { t } = useI18n();
const route = useRoute();
const router = useRouter();

const PER_PAGE = 50;
const one = (v: unknown) => (typeof v === "string" ? v : "");

const registry = computed<string>({
  get: () => one(route.query.registry),
  set: (r) =>
    void router.replace({ query: { ...route.query, registry: r || undefined, page: undefined } }),
});
const reason = computed<string>({
  get: () => one(route.query.reason),
  set: (r) =>
    void router.replace({ query: { ...route.query, reason: r || undefined, page: undefined } }),
});
const page = computed<number>(() => Math.max(1, Number.parseInt(one(route.query.page), 10) || 1));
const goTo = (p: number) =>
  void router.replace({ query: { ...route.query, page: p > 1 ? String(p) : undefined } });

const registries = ref<string[]>([]);
// The picker is a convenience: without it, "every registry" still works.
async function loadRegistries() {
  try {
    const { data } = await securityCoverage();
    registries.value = (data?.registries ?? []).filter((r) => r.protected).map((r) => r.registry);
  } catch {
    // Left empty: the picker then offers "all registries" alone.
  }
}
void loadRegistries();
const registryOptions = computed(() => [
  { value: "", label: t("heldVersions.allRegistries") },
  ...registries.value.map((r) => ({ value: r, label: r })),
]);
const reasonOptions = computed(() => [
  { value: "", label: t("heldVersions.anyReason") },
  ...Object.entries(REASON_LABEL_KEYS).map(([code, key]) => ({ value: code, label: t(key) })),
]);

const items = ref<Verdict[]>([]);
const total = ref(0);
const loading = ref(false);
const error = ref<string | null>(null);
const pages = computed(() => Math.max(1, Math.ceil(total.value / PER_PAGE)));

// A later choice wins: an answer to an earlier one is dropped.
let latest = 0;
async function load() {
  const mine = ++latest;
  loading.value = true;
  error.value = null;
  try {
    const { data, error: err } = await listVerdicts({
      query: {
        state: "held",
        registry: registry.value || undefined,
        reason: reason.value || undefined,
        page: page.value,
        per_page: PER_PAGE,
      },
    });
    if (mine !== latest) return;
    if (err || !data) throw new Error(t("heldVersions.loadFailed"));
    items.value = data.items;
    total.value = data.total;
  } catch (e) {
    if (mine === latest) error.value = (e as Error).message;
  } finally {
    if (mine === latest) loading.value = false;
  }
}
watch([registry, reason, page], load, { immediate: true });
</script>

<template>
  <Card>
    <CardHeader>
      <CardTitle :count="total">{{ t("heldVersions.title") }}</CardTitle>
    </CardHeader>
    <CardContent class="space-y-4">
      <div class="flex flex-wrap items-end gap-4">
        <div class="space-y-1">
          <Label for="held-registry">{{ t("common.registry") }}</Label>
          <Select id="held-registry" v-model="registry" :options="registryOptions" />
        </div>
        <div class="space-y-1">
          <Label for="held-reason">{{ t("heldVersions.reason") }}</Label>
          <Select id="held-reason" v-model="reason" :options="reasonOptions" />
        </div>
      </div>
      <AsyncState
        :loading="loading && !items.length"
        :error="error"
        :empty="!items.length"
        :empty-message="t('heldVersions.empty')"
        :on-retry="load"
      >
        <Table stacked :label="t('heldVersions.title')" data-testid="held-rows">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("common.package") }}</TableHead>
              <TableHead>{{ t("holdsPanel.state") }}</TableHead>
              <TableHead>{{ t("holdsPanel.reasons") }}</TableHead>
              <TableHead>{{ t("holdsPanel.evaluated") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow
              v-for="v in items"
              :key="`${v.package.registry}/${v.package.name}@${v.package.version}`"
            >
              <TableCell class="font-mono text-sm">
                <RouterLink
                  :to="packagePath(v.package.registry, v.package.name)"
                  class="underline underline-offset-[3px]"
                  >{{ v.package.registry }}/{{ v.package.name }}@{{ v.package.version }}</RouterLink
                >
              </TableCell>
              <TableCell :label="t('holdsPanel.state')">
                <Resolution
                  v-if="HOLD_RESOLUTION[v.state]"
                  :state="HOLD_RESOLUTION[v.state]!"
                  :label="t(HOLD_LABEL_KEYS[v.state]!)"
                />
                <span v-else class="font-mono text-xs uppercase">{{ v.state }}</span>
              </TableCell>
              <TableCell :label="t('holdsPanel.reasons')" class="text-xs"
                ><span class="flex flex-col gap-1">
                  <CodeLabel
                    v-for="code in v.reason_codes"
                    :key="code"
                    :code="code"
                    :labels="REASON_LABEL_KEYS"
                  /> </span
              ></TableCell>
              <TableCell :label="t('holdsPanel.evaluated')" class="text-xs"
                ><DateTime :iso="v.evaluated_at"
              /></TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </AsyncState>
      <div v-if="total > PER_PAGE" class="flex items-center gap-2" data-testid="held-pager">
        <Button
          size="sm"
          variant="outline"
          :disabled="loading || page <= 1"
          @click="goTo(page - 1)"
        >
          {{ t("flagsPanel.previous") }}
        </Button>
        <span class="text-sm tabular-nums">{{ t("heldVersions.pageOf", { page, pages }) }}</span>
        <Button
          size="sm"
          variant="outline"
          :disabled="loading || page >= pages"
          @click="goTo(page + 1)"
        >
          {{ t("flagsPanel.next") }}
        </Button>
      </div>
    </CardContent>
  </Card>
</template>
