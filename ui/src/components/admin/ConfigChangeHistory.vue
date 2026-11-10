<script setup lang="ts">
import { ref } from "vue";
import { useI18n } from "vue-i18n";

import type { ConfigChangeRow } from "@/client/types.gen";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { AsyncState } from "@/components/ui/async-state";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";
import { ChevronRight } from "@lucide/vue";
import { formatDate } from "@/lib/format";

/** The recorded config changes, each row disclosing its diff. */
defineProps<{
  changeHistory: ConfigChangeRow[];
  loading: boolean;
}>();

const { t } = useI18n();

const expandedRow = ref<string | null>(null);
</script>

<template>
  <Card>
    <CardHeader>
      <CardTitle>{{ t("adminConfigReload.changeHistory") }}</CardTitle>
    </CardHeader>
    <CardContent>
      <AsyncState
        :loading="loading"
        :empty="changeHistory.length === 0"
        :empty-message="t('adminConfigReload.noChangesRecordedYet')"
      >
        <Table :label="t('adminConfigReload.changeHistory')">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("common.date") }}</TableHead>
              <TableHead>{{ t("adminConfigReload.byColumn") }}</TableHead>
              <TableHead>{{ t("common.status") }}</TableHead>
              <TableHead>{{ t("common.summary") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <template v-for="row in changeHistory" :key="row.id">
              <!-- The disclosure is a button in the first cell, not a `@click`
                   on the `<tr>`. A clickable row with `cursor-pointer`, no
                   `tabindex`, no `role` and no key handler is unreachable by
                   keyboard and announces nothing, which is the same finding
                   `PackageDetailPage.vue` records against its version list.
                   A button brings the focus ring, Enter and Space with it, and
                   `aria-expanded` is what tells a reader whether the diff below
                   is open — the chevron alone says it only to someone who can
                   see it. -->
              <TableRow>
                <TableCell>
                  <button
                    type="button"
                    class="inline-flex items-center gap-2 text-left hover:underline underline-offset-[3px]"
                    :aria-expanded="expandedRow === row.id"
                    :aria-controls="expandedRow === row.id ? `config-change-${row.id}` : undefined"
                    @click="expandedRow = expandedRow === row.id ? null : row.id"
                  >
                    <ChevronRight
                      class="h-3.5 w-3.5 shrink-0 text-muted-foreground"
                      :class="expandedRow === row.id ? 'rotate-90' : ''"
                      aria-hidden="true"
                    />
                    {{ formatDate(row.triggered_at) }}
                    <span class="sr-only">{{
                      expandedRow === row.id
                        ? t("adminConfigReload.hideDiff")
                        : t("adminConfigReload.showDiff")
                    }}</span>
                  </button>
                </TableCell>
                <TableCell>{{ row.triggered_by }}</TableCell>
                <TableCell>
                  <!-- `applied` or `rejected` (reload/applier.rs); only a rejection
                       is a refusal, so only it takes crimson. -->
                  <Badge :variant="row.status === 'rejected' ? 'destructive' : 'known'">
                    {{
                      row.status === "applied"
                        ? t("adminConfigReload.statusApplied")
                        : row.status === "rejected"
                          ? t("adminConfigReload.statusRejected")
                          : row.status
                    }}
                  </Badge>
                </TableCell>
                <TableCell>{{ row.summary }}</TableCell>
              </TableRow>
              <TableRow v-if="expandedRow === row.id" :id="`config-change-${row.id}`">
                <TableCell colspan="4">
                  <pre class="bg-muted text-xs p-2 overflow-x-auto">{{
                    JSON.stringify(row.diff, null, 2)
                  }}</pre>
                </TableCell>
              </TableRow>
            </template>
          </TableBody>
        </Table>
      </AsyncState>
    </CardContent>
  </Card>
</template>
