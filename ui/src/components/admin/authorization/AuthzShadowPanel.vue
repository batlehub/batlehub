<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { onMounted, ref } from "vue";

import { authzShadow } from "@/client/sdk.gen";
import type { ShadowResponse } from "@/client/types.gen";
import { extractMessage } from "@/composables/useApi";
import { Alert } from "@/components/ui/alert";
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
 * RFC 0015 §4.7: the nodes in shadow-mode, serving what the model refuses.
 * Loads without being asked (see `AdminAuthorization.vue`).
 */
const { t } = useI18n();

const shadowReport = ref<ShadowResponse | null>(null);
const shadowError = ref<string | null>(null);

async function loadShadow() {
  shadowError.value = null;
  try {
    const res = await authzShadow({ query: {} as never });
    if (res.error) {
      shadowError.value = extractMessage(res.error);
      return;
    }
    shadowReport.value = res.data ?? null;
  } catch (e: unknown) {
    shadowError.value = e instanceof Error ? e.message : String(e);
  }
}

onMounted(loadShadow);
</script>

<template>
  <section class="space-y-3" data-testid="panel-shadow">
    <h2 class="font-mono text-xs uppercase tracking-[0.16em] text-muted-foreground">
      {{ t("adminAuthorization.shadowTitle") }}
    </h2>
    <!-- "No shadow" and "a quiet shadow" share the empty panel and not its
       words: they mean opposite things. -->
    <AsyncState
      :loading="false"
      :error="shadowError"
      :empty="!shadowReport?.by_node?.length"
      :empty-message="
        shadowReport?.no_shadow_configured
          ? t('adminAuthorization.shadowNoneConfigured')
          : t('adminAuthorization.shadowQuiet')
      "
      :on-retry="loadShadow"
    >
      <div class="space-y-2">
        <Alert variant="destructive">{{ t("adminAuthorization.shadowWarning") }}</Alert>
        <Table :label="t('adminAuthorization.shadowTitle')">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("adminAuthorization.node") }}</TableHead>
              <TableHead>{{ t("adminAuthorization.until") }}</TableHead>
              <TableHead class="text-right">{{ t("adminAuthorization.served") }}</TableHead>
              <TableHead>{{ t("adminAuthorization.missingVerbs") }}</TableHead>
              <TableHead>{{ t("adminAuthorization.subjects") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow v-for="row in shadowReport?.by_node" :key="row.node">
              <TableCell class="font-mono text-xs">{{ row.node }}</TableCell>
              <TableCell>{{ row.shadow_until }}</TableCell>
              <TableCell class="text-right tabular-nums">{{ row.count }}</TableCell>
              <TableCell class="font-mono text-xs">{{ row.actions.join(", ") }}</TableCell>
              <TableCell class="font-mono text-xs">{{ row.subjects.join(", ") }}</TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </div>
    </AsyncState>
  </section>
</template>
