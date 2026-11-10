<script setup lang="ts">
/**
 * Every registry, one line each, leading to its page (RFC 0044 §6.4). Read
 * from `/admin/stats`, which already filters per registry by `stats:read`.
 */
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";

import { adminStats } from "@/client/sdk.gen";
import type { StatsResponse } from "@/client/types.gen";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { OBSERVABILITY_TABS } from "@/config/adminSections";
import { useApi } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { Card, CardContent } from "@/components/ui/card";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

const { t } = useI18n();
const { token, can } = useAuth();
const { data, loading, error, reload } = useApi<StatsResponse>(
  () => adminStats() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);
const pct = (n: number | null | undefined) => (n == null ? "—" : `${(n * 100).toFixed(1)}%`);
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="OBSERVABILITY_TABS" />
    <PageHeader
      variant="display"
      :title="t('adminRegistries.title')"
      :description="t('adminRegistries.description')"
    />
    <AsyncState
      :loading="loading"
      :error="error"
      :title="t('adminRegistries.loadError')"
      :on-retry="reload"
      :empty="!!data && data.per_registry.length === 0"
      :empty-message="t('adminRegistries.empty')"
    >
      <Card>
        <CardContent class="p-0">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{{ t("common.registry") }}</TableHead>
                <TableHead class="text-right">{{ t("adminDashboard.hitRate") }}</TableHead>
                <TableHead>{{ t("adminRegistries.upstream") }}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              <TableRow v-for="r in data?.per_registry ?? []" :key="r.registry">
                <TableCell class="font-mono text-sm">
                  <!-- The registry page asks system:read; this list only stats:read. -->
                  <RouterLink
                    v-if="can('system:read')"
                    :to="`/admin/observability/registries/${encodeURIComponent(r.registry)}`"
                    class="hover:underline underline-offset-[3px]"
                    >{{ r.registry }}</RouterLink
                  >
                  <template v-else>{{ r.registry }}</template>
                </TableCell>
                <TableCell class="text-right font-mono text-sm">{{ pct(r.hit_rate) }}</TableCell>
                <TableCell :class="r.upstream_degraded ? 'text-destructive' : ''">{{
                  r.upstream_degraded ? t("adminRegistry.degraded") : t("adminRegistry.ok")
                }}</TableCell>
              </TableRow>
            </TableBody>
          </Table>
        </CardContent>
      </Card>
    </AsyncState>
  </div>
</template>
