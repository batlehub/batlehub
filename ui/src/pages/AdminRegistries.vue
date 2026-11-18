<script setup lang="ts">
/**
 * Every registry, one line each, leading to its page (RFC 0044 §6.4). Read
 * from `/admin/stats`, which already filters per registry by `stats:read`.
 */
import { useI18n } from "vue-i18n";
import { computed } from "vue";
import { RouterLink, useRouter } from "vue-router";

import { adminStats, registryHealth } from "@/client/sdk.gen";
import type { RegistryHealthDto, StatsResponse } from "@/client/types.gen";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { REGISTRIES_TABS } from "@/config/adminSections";
import { useApi } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { Card, CardContent } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { formatBytes, formatDate, formatRelative } from "@/lib/format";
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

/**
 * What each registry *is*, beside how it is doing: type, mode, size and last
 * activity, from the health probe the dashboard already reads. A list of names
 * and hit rates left an operator opening each registry to learn which one was
 * the npm proxy and which the private gems. Joined by name; a registry the
 * probe does not return keeps its stats row with dashes, never disappears.
 */
const { data: health } = useApi<RegistryHealthDto[]>(
  () => registryHealth() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);
const healthByName = computed(
  () => new Map((health.value ?? []).map((h) => [h.registry, h] as const)),
);

/** The whole row opens the registry, for a pointer; the name stays the link. */
const router = useRouter();
function openRow(name: string, event: MouseEvent) {
  if (!can("system:read")) return;
  // A click that landed on the link itself is already navigating.
  if ((event.target as HTMLElement).closest("a")) return;
  void router.push(`/admin/registry/${encodeURIComponent(name)}`);
}
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="REGISTRIES_TABS" />
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
                <TableHead>{{ t("adminRegistries.type") }}</TableHead>
                <TableHead class="hidden md:table-cell">{{ t("adminRegistries.mode") }}</TableHead>
                <TableHead class="text-right">{{ t("adminDashboard.hitRate") }}</TableHead>
                <TableHead class="hidden text-right lg:table-cell">{{
                  t("common.cached")
                }}</TableHead>
                <TableHead class="hidden lg:table-cell">{{
                  t("adminRegistries.lastPull")
                }}</TableHead>
                <TableHead>{{ t("adminRegistries.upstream") }}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              <TableRow
                v-for="r in data?.per_registry ?? []"
                :key="r.registry"
                :class="can('system:read') ? 'cursor-pointer' : ''"
                data-testid="registry-row"
                @click="openRow(r.registry, $event)"
              >
                <TableCell class="font-mono text-sm">
                  <!-- The registry page asks system:read; this list only stats:read. -->
                  <RouterLink
                    v-if="can('system:read')"
                    :to="`/admin/registry/${encodeURIComponent(r.registry)}`"
                    class="hover:underline underline-offset-[3px]"
                    >{{ r.registry }}</RouterLink
                  >
                  <template v-else>{{ r.registry }}</template>
                </TableCell>
                <TableCell>
                  <Badge v-if="healthByName.get(r.registry)" variant="secondary">{{
                    healthByName.get(r.registry)?.registry_type
                  }}</Badge>
                  <span v-else class="text-muted-foreground">—</span>
                </TableCell>
                <TableCell class="hidden md:table-cell">
                  <Badge v-if="healthByName.get(r.registry)" variant="outline">{{
                    healthByName.get(r.registry)?.mode
                  }}</Badge>
                  <span v-else class="text-muted-foreground">—</span>
                </TableCell>
                <TableCell class="text-right font-mono text-sm">{{ pct(r.hit_rate) }}</TableCell>
                <TableCell
                  class="hidden text-right font-mono text-sm text-muted-foreground lg:table-cell"
                  >{{ formatBytes(r.cached_bytes) }}</TableCell
                >
                <TableCell class="hidden text-sm text-muted-foreground lg:table-cell">
                  <time
                    v-if="healthByName.get(r.registry)?.last_pull_at"
                    :datetime="healthByName.get(r.registry)?.last_pull_at ?? undefined"
                    :title="formatDate(healthByName.get(r.registry)?.last_pull_at)"
                    >{{ formatRelative(healthByName.get(r.registry)?.last_pull_at) }}</time
                  >
                  <template v-else>{{ t("common.never") }}</template>
                </TableCell>
                <TableCell
                  class="text-sm"
                  :class="r.upstream_degraded ? 'text-destructive' : 'text-ok'"
                  >{{
                    r.upstream_degraded ? t("adminRegistry.degraded") : t("adminRegistry.ok")
                  }}</TableCell
                >
              </TableRow>
            </TableBody>
          </Table>
        </CardContent>
      </Card>
    </AsyncState>
  </div>
</template>
