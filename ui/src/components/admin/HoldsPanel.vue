<script setup lang="ts">
import { ref, onMounted } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";

import { verdictSummary } from "@/client/sdk.gen";
import type { RegistryVerdictSummary } from "@/client/types.gen";
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

/**
 * What the supply-chain layer holds, across every registry with a
 * `[security]` profile (RFC 0042 §5.2): counts per state. The held versions
 * themselves are listed once, by `HeldVersions` below it. Never the findings — those stay
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
      </AsyncState>
    </CardContent>
  </Card>
</template>
