<script setup lang="ts">
import { ref, onMounted } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";

import { verdictSummary } from "@/client/sdk.gen";
import type { RegistryVerdictSummary } from "@/client/types.gen";
import { Resolution } from "@/components/ui/resolution";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { AsyncState } from "@/components/ui/async-state";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";
import { formatDay } from "@/lib/format";
import { packagePath } from "@/config/navigation";
import { HOLD_LABEL_KEYS, HOLD_RESOLUTION } from "@/config/verdict";
import CodeLabel from "@/components/admin/CodeLabel.vue";
import { REASON_LABEL_KEYS } from "@/config/codeLabels";

/**
 * What the supply-chain layer holds, across every registry with a
 * `[security]` profile (RFC 0042 §5.2): counts per state, then the most
 * recent holds with their reason codes. Never the findings — those stay
 * behind `findings:read`: with `canOpenFindings`, each registry row links to
 * its findings on `/security/findings?registry=` (RFC 0050 §6.6: the inline
 * drawer is gone, one home per table).
 */
const props = defineProps<{ canOpenFindings?: boolean }>();

const { t } = useI18n();

const registries = ref<RegistryVerdictSummary[]>([]);
const loading = ref(false);
const error = ref<string | null>(null);

async function load() {
  loading.value = true;
  error.value = null;
  try {
    const { data, error: err } = await verdictSummary();
    if (err || !data) {
      error.value = t("holdsPanel.loadFailed");
      registries.value = [];
      return;
    }
    registries.value = data.registries;
  } catch {
    error.value = t("holdsPanel.loadFailed");
  } finally {
    loading.value = false;
  }
}

onMounted(load);
</script>

<template>
  <Card data-testid="holds-panel">
    <CardHeader>
      <CardTitle>{{ t("holdsPanel.title") }}</CardTitle>
    </CardHeader>
    <CardContent class="space-y-4">
      <p class="text-sm text-muted-foreground max-w-[70ch]">{{ t("holdsPanel.intro") }}</p>
      <AsyncState
        :loading="loading"
        :error="error"
        :empty="!registries.length"
        :empty-message="t('holdsPanel.empty')"
        :on-retry="load"
      >
        <Table stacked :label="t('holdsPanel.title')" data-testid="holds-counts">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("common.registry") }}</TableHead>
              <TableHead>{{ t("holdsPanel.denied") }}</TableHead>
              <TableHead>{{ t("holdsPanel.quarantined") }}</TableHead>
              <TableHead>{{ t("holdsPanel.warned") }}</TableHead>
              <TableHead>{{ t("holdsPanel.allowed") }}</TableHead>
              <TableHead v-if="props.canOpenFindings">
                <span class="sr-only">{{ t("findingsPanel.title") }}</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow v-for="r in registries" :key="r.registry">
              <TableCell class="font-mono text-sm">{{ r.registry }}</TableCell>
              <TableCell class="font-mono text-sm" :label="t('holdsPanel.denied')">{{
                r.counts.denied
              }}</TableCell>
              <TableCell class="font-mono text-sm" :label="t('holdsPanel.quarantined')">{{
                r.counts.quarantined
              }}</TableCell>
              <TableCell class="font-mono text-sm" :label="t('holdsPanel.warned')">{{
                r.counts.warned
              }}</TableCell>
              <TableCell class="font-mono text-sm" :label="t('holdsPanel.allowed')">{{
                r.counts.allowed
              }}</TableCell>
              <TableCell v-if="props.canOpenFindings" class="text-right">
                <RouterLink
                  :to="{
                    path: '/security/findings',
                    query: { registry: r.registry, verdict: 'denied' },
                  }"
                  class="text-sm text-primary underline underline-offset-[3px]"
                  :aria-label="t('holdsPanel.findingsOf', { registry: r.registry })"
                  :data-testid="`open-findings-${r.registry}`"
                  >{{ t("securityNav.findings") }}</RouterLink
                >
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>

        <Table stacked :label="t('holdsPanel.recent')" data-testid="holds-recent">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("common.package") }}</TableHead>
              <TableHead>{{ t("holdsPanel.state") }}</TableHead>
              <TableHead>{{ t("holdsPanel.reasons") }}</TableHead>
              <TableHead>{{ t("holdsPanel.evaluated") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <template v-for="r in registries" :key="r.registry">
              <TableRow v-for="h in r.recent" :key="`${r.registry}/${h.name}@${h.version}`">
                <TableCell class="font-mono text-sm">
                  <RouterLink :to="packagePath(r.registry, h.name)" class="underline">
                    {{ r.registry }}/{{ h.name }}@{{ h.version }}
                  </RouterLink>
                </TableCell>
                <TableCell :label="t('holdsPanel.state')">
                  <Resolution
                    v-if="HOLD_RESOLUTION[h.state]"
                    :state="HOLD_RESOLUTION[h.state]!"
                    :label="t(HOLD_LABEL_KEYS[h.state]!)"
                  />
                  <span v-else class="font-mono text-xs uppercase">{{ h.state }}</span>
                </TableCell>
                <TableCell :label="t('holdsPanel.reasons')" class="text-xs"
                  ><span class="flex flex-col gap-1">
                    <CodeLabel
                      v-for="code in h.reason_codes"
                      :key="code"
                      :code="code"
                      :labels="REASON_LABEL_KEYS"
                    />
                  </span>
                </TableCell>
                <TableCell :label="t('holdsPanel.evaluated')" class="text-xs">{{
                  formatDay(h.evaluated_at)
                }}</TableCell>
              </TableRow>
            </template>
          </TableBody>
        </Table>
      </AsyncState>
    </CardContent>
  </Card>
</template>
