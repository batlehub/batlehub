<script setup lang="ts">
/**
 * "What has been posted to us, and was it signed?" (RFC 0004 Phase 5, *split*).
 *
 * This was the third tab of `/admin/notifications`, and it is the half that
 * earned its own route: it reads nothing the other two produce. Its only source
 * is `listInboundEvents()`, and `webhook_name` is not a channel name —
 * `[[notifications.inbound]]` and `[[notifications.channels]]` are separate
 * config blocks, so the two nouns never meet.
 *
 * Its siblings do share state, which is why they stayed together: the
 * subscription form's channel `datalist` is populated from the channel list, so
 * separating them would duplicate a fetch and buy nothing (RFC 0004 R8).
 */
import { useI18n } from "vue-i18n";
import { computed } from "vue";
import { listInboundEvents } from "@/client/sdk.gen";
import type { InboundEventsResponse } from "@/client/types.gen";
import { useApi } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { formatDate as fmtTs } from "@/lib/format";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { NOTIFICATIONS_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { EmptyState } from "@/components/ui/empty-state";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";

const { t } = useI18n();
const { token } = useAuth();

const {
  data: inboundResp,
  loading,
  error,
  reload,
} = useApi<InboundEventsResponse>(
  () => listInboundEvents() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);

const events = computed(() => inboundResp.value?.events ?? []);
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="NOTIFICATIONS_TABS" />
    <PageHeader
      variant="display"
      :title="t('adminNav.inboundEvents')"
      :description="t('adminNotifications.inboundDescription')"
    >
      <template #actions>
        <Button variant="outline" size="sm" :disabled="loading" @click="reload">
          {{ loading ? t("common.refreshing") : t("common.refresh") }}
        </Button>
      </template>
    </PageHeader>

    <AsyncState :loading="loading && !inboundResp" :error="error">
      <!-- Says where events come from, as the channels panel does for its own
           half: an empty list here is far more often "nothing is configured"
           than "nothing arrived". -->
      <EmptyState
        v-if="events.length === 0"
        :title="t('adminNotifications.noInboundEventsReceived')"
        :description="t('adminNotifications.inboundConfiguredIn')"
      />
      <Card v-else>
        <CardContent class="p-0">
          <Table :label="t('adminNav.inboundEvents')">
            <TableHeader>
              <TableRow>
                <TableHead>{{ t("common.webhook") }}</TableHead>
                <TableHead>{{ t("adminNotifications.receivedAt") }}</TableHead>
                <TableHead>{{ t("adminNotifications.sourceIp") }}</TableHead>
                <TableHead>{{ t("common.signature") }}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              <TableRow v-for="ev in events" :key="ev.id">
                <TableCell class="font-mono text-sm">{{ ev.webhook_name }}</TableCell>
                <TableCell class="text-xs">{{ fmtTs(ev.received_at) }}</TableCell>
                <TableCell class="font-mono text-xs">{{ ev.source_ip ?? "—" }}</TableCell>
                <TableCell>
                  <Badge v-if="ev.signature_valid === true" variant="known" class="text-xs">{{
                    t("common.valid")
                  }}</Badge>
                  <Badge
                    v-else-if="ev.signature_valid === false"
                    variant="destructive"
                    class="text-xs"
                    >{{ t("common.invalid") }}</Badge
                  >
                  <span v-else class="text-xs text-muted-foreground">{{
                    t("adminNotifications.unsigned")
                  }}</span>
                </TableCell>
              </TableRow>
            </TableBody>
          </Table>
        </CardContent>
      </Card>
    </AsyncState>
  </div>
</template>
