<script setup lang="ts">
import { useI18n } from "vue-i18n";
import type { PackageEventDto } from "@/client/types.gen";
import { formatDate as fmtDate } from "@/lib/format";
import { Badge } from "@/components/ui/badge";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";

const { t } = useI18n();

defineProps<{ events: PackageEventDto[] }>();

/* Keys, not sentences. An action this table has never seen falls through to
   the raw verb the server sent, which is the honest answer for a value the
   console does not know a name for. */
const ACTION_LABEL_KEYS: Record<string, string> = {
  download: "common.download",
  view_metadata: "packageEventsTable.viewMetadata",
  block: "common.block",
  unblock: "common.unblock",
};
function fmtAction(a: string) {
  const key = ACTION_LABEL_KEYS[a];
  return key ? t(key) : a;
}
</script>

<template>
  <Card>
    <CardHeader>
      <CardTitle>{{ t("packageEventsTable.recentAccessEvents") }}</CardTitle>
    </CardHeader>
    <CardContent class="p-0">
      <Table :label="t('a11y.eventsTableLabel')">
        <TableHeader>
          <TableRow>
            <TableHead>{{ t("common.when") }}</TableHead>
            <TableHead>{{ t("common.user") }}</TableHead>
            <TableHead>{{ t("common.role") }}</TableHead>
            <TableHead>{{ t("common.version") }}</TableHead>
            <TableHead>{{ t("common.artifact") }}</TableHead>
            <TableHead>{{ t("common.action") }}</TableHead>
            <TableHead>{{ t("common.outcome") }}</TableHead>
            <TableHead>{{ t("common.reason") }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow v-for="ev in events" :key="ev.id">
            <TableCell class="text-xs tabular-nums whitespace-nowrap">{{
              fmtDate(ev.timestamp)
            }}</TableCell>
            <TableCell class="text-sm">
              <span v-if="ev.user_id">{{ ev.user_id }}</span>
              <span v-else class="text-muted-foreground italic">anonymous</span>
            </TableCell>
            <TableCell>
              <Badge variant="outline" class="text-xs capitalize">{{ ev.user_role }}</Badge>
            </TableCell>
            <TableCell class="font-mono text-xs">{{ ev.version }}</TableCell>
            <TableCell class="font-mono text-xs text-muted-foreground">{{
              ev.artifact ?? "—"
            }}</TableCell>
            <TableCell class="text-xs">{{ fmtAction(ev.action) }}</TableCell>
            <TableCell>
              <Badge
                :variant="ev.outcome === 'denied' ? 'destructive' : 'secondary'"
                class="text-xs"
              >
                {{ ev.outcome }}
              </Badge>
            </TableCell>
            <TableCell
              class="text-xs text-muted-foreground max-w-[200px] truncate"
              :title="ev.deny_reason ?? ''"
            >
              {{ ev.deny_reason ?? "—" }}
            </TableCell>
          </TableRow>
        </TableBody>
      </Table>
      <p v-if="events.length === 0" class="p-6 text-sm text-muted-foreground text-center">
        {{ t("packageEventsTable.noEventsRecordedYet") }}
      </p>
    </CardContent>
  </Card>
</template>
