<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { onMounted, ref } from "vue";

import { auditLog } from "@/client/sdk.gen";
import type { AccessEvent } from "@/client/types.gen";
import { extractMessage } from "@/composables/useApi";
import { AsyncState } from "@/components/ui/async-state";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";

/** The denials the audit log recorded, most recent first. */
const { t } = useI18n();

const denials = ref<AccessEvent[]>([]);

/** `registry/name@version`, or a dash for an event about no package. */
function coordinate(e: AccessEvent): string {
  const p = e.package_id;
  return p ? `${p.registry}/${p.name}@${p.version}` : "-";
}

/** The refusal's reason; a denied event always carries one. */
function reason(e: AccessEvent): string {
  return e.result.outcome === "allowed" ? "-" : e.result.reason;
}
const denialsError = ref<string | null>(null);

async function loadDenials() {
  denialsError.value = null;
  try {
    // The audit log already records every denial that *happened*; this panel is
    // that list, filtered. Deliberately a different source from Shadow above:
    // one is what was refused, the other is what would have been.
    const res = await auditLog({ query: { denied_only: true, per_page: 25 } });
    if (res.error) {
      denialsError.value = extractMessage(res.error);
      return;
    }
    denials.value = res.data?.items ?? [];
  } catch (e: unknown) {
    denialsError.value = e instanceof Error ? e.message : String(e);
  }
}

onMounted(loadDenials);
</script>

<template>
  <section class="space-y-3" data-testid="panel-denials">
    <h2 class="font-sans text-sm font-semibold text-foreground">
      {{ t("adminAuthorization.denialsTitle") }}
    </h2>
    <AsyncState
      :loading="false"
      :error="denialsError"
      :empty="!denials.length"
      :empty-message="t('adminAuthorization.denialsNone')"
      :on-retry="loadDenials"
    >
      <Table :label="t('adminAuthorization.denialsTitle')">
        <TableHeader>
          <TableRow>
            <TableHead>{{ t("adminAuthorization.when") }}</TableHead>
            <TableHead>{{ t("adminAuthorization.subject") }}</TableHead>
            <TableHead>{{ t("adminAuthorization.coordinate") }}</TableHead>
            <TableHead>{{ t("adminAuthorization.reason") }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow v-for="d in denials" :key="d.id">
            <TableCell class="text-xs">{{ d.timestamp }}</TableCell>
            <TableCell class="font-mono text-xs">{{ d.user_id ?? "-" }}</TableCell>
            <TableCell class="font-mono text-xs">{{ coordinate(d) }}</TableCell>
            <TableCell class="text-xs">{{ reason(d) }}</TableCell>
          </TableRow>
        </TableBody>
      </Table>
    </AsyncState>
  </section>
</template>
