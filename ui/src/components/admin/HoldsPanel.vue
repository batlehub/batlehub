<script setup lang="ts">
import { ref, onMounted } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";

import { verdictSummary } from "@/client/sdk.gen";
import type { RegistryVerdictSummary } from "@/client/types.gen";
import { Button } from "@/components/ui/button";
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

/**
 * What the supply-chain layer holds, across every registry with a
 * `[security]` profile (RFC 0042 §5.2): counts per state, then the most
 * recent holds with their reason codes. Never the findings — those stay
 * behind `findings:read`: with `canOpenFindings`, each registry row offers
 * its listing, which the dashboard draws as its own section.
 */
const props = defineProps<{
  canOpenFindings?: boolean;
  /** The registry whose findings are open, so its row reads as pressed. */
  findingsFor?: string | null;
}>();
const emit = defineEmits<{ openFindings: [registry: string] }>();

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
      <p class="text-sm text-muted-foreground">{{ t("holdsPanel.intro") }}</p>
      <AsyncState
        :loading="loading"
        :error="error"
        :empty="!registries.length"
        :empty-message="t('holdsPanel.empty')"
        :on-retry="load"
      >
        <Table :label="t('holdsPanel.title')" data-testid="holds-counts">
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
              <TableCell class="font-mono text-sm">{{ r.counts.denied }}</TableCell>
              <TableCell class="font-mono text-sm">{{ r.counts.quarantined }}</TableCell>
              <TableCell class="font-mono text-sm">{{ r.counts.warned }}</TableCell>
              <TableCell class="font-mono text-sm">{{ r.counts.allowed }}</TableCell>
              <TableCell v-if="props.canOpenFindings" class="text-right">
                <Button
                  variant="outline"
                  size="sm"
                  :aria-pressed="props.findingsFor === r.registry"
                  :aria-label="t('findingsPanel.openFor', { registry: r.registry })"
                  :data-testid="`open-findings-${r.registry}`"
                  @click="emit('openFindings', r.registry)"
                >
                  {{ t("findingsPanel.open") }}
                </Button>
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>

        <Table :label="t('holdsPanel.recent')" data-testid="holds-recent">
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
                <TableCell>
                  <Resolution
                    v-if="HOLD_RESOLUTION[h.state]"
                    :state="HOLD_RESOLUTION[h.state]!"
                    :label="t(HOLD_LABEL_KEYS[h.state]!)"
                  />
                  <span v-else class="font-mono text-xs uppercase">{{ h.state }}</span>
                </TableCell>
                <TableCell class="font-mono text-xs">{{ h.reason_codes.join(", ") }}</TableCell>
                <TableCell class="text-xs">{{ formatDay(h.evaluated_at) }}</TableCell>
              </TableRow>
            </template>
          </TableBody>
        </Table>
      </AsyncState>
    </CardContent>
  </Card>
</template>
