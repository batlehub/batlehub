<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { onMounted, ref } from "vue";

import { auditLog } from "@/client/sdk.gen";
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

type AuditRow = {
  timestamp?: string;
  user_id?: string | null;
  package_id?: string | null;
  action?: string;
  reason?: string | null;
};

const denials = ref<AuditRow[]>([]);
const denialsError = ref<string | null>(null);

async function loadDenials() {
  denialsError.value = null;
  try {
    // The audit log already records every denial that *happened*; this panel is
    // that list, filtered. Deliberately a different source from Shadow above:
    // one is what was refused, the other is what would have been.
    const res = await auditLog({ query: { result: "denied", limit: 25 } as never });
    if (res.error) {
      denialsError.value = extractMessage(res.error);
      return;
    }
    const data = res.data as unknown as { events?: AuditRow[] } | AuditRow[] | null;
    denials.value = Array.isArray(data) ? data : (data?.events ?? []);
  } catch (e: unknown) {
    denialsError.value = e instanceof Error ? e.message : String(e);
  }
}

onMounted(loadDenials);
</script>

<template>
  <section class="space-y-3" data-testid="panel-denials">
    <h2 class="font-mono text-xs uppercase tracking-[0.16em] text-muted-foreground">
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
          <TableRow v-for="(d, i) in denials" :key="i">
            <TableCell class="text-xs">{{ d.timestamp }}</TableCell>
            <TableCell class="font-mono text-xs">{{ d.user_id ?? "-" }}</TableCell>
            <TableCell class="font-mono text-xs">{{ d.package_id ?? "-" }}</TableCell>
            <TableCell class="text-xs">{{ d.reason ?? "-" }}</TableCell>
          </TableRow>
        </TableBody>
      </Table>
    </AsyncState>
  </section>
</template>
