<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";
import { securityScanners } from "@/client/sdk.gen";
import type { ScannerHealth } from "@/client/types.gen";
import { useApi } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { AsyncState } from "@/components/ui/async-state";
import { DateTime } from "@/components/ui/date-time";
import { formatRelative } from "@/lib/format";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import StateMark from "./StateMark.vue";
import { docsUrl } from "@/config";

/**
 * Whether anything is scanning (RFC 0050 §4.5): the live workers and what
 * they cover, the queue that remains, and each scanner's last success and
 * last error class. A scanner that never ran reads as such, not as OK.
 */
const { t, locale } = useI18n();
const { token } = useAuth();
const { data, loading, error, reload } = useApi<ScannerHealth>(
  () => securityScanners() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);
const live = computed(() => (data.value?.workers ?? []).filter((w) => w.live));
const lastSeen = computed(() =>
  (data.value?.workers ?? [])
    .map((w) => w.last_seen)
    .sort()
    .at(-1),
);
const scannerState = (s: string) => (s === "ok" ? "ok" : "attention") as "ok" | "attention";
</script>

<template>
  <AsyncState :loading="loading" :error="error" :on-retry="reload">
    <section v-if="data" class="space-y-3" data-testid="scanner-health">
      <div class="flex flex-col gap-1 text-sm sm:flex-row sm:gap-3" data-testid="workers">
        <span class="w-24 shrink-0 font-mono text-xs uppercase tracking-wider">{{
          t("securityScanners.workers")
        }}</span>
        <span v-if="live.length" class="flex-1">
          <span v-for="w in live" :key="w.worker_id" class="block">
            <StateMark state="ok" />
            <span class="font-mono">{{ w.worker_id }}</span>
            · {{ t("securityScanners.lastSeen", { when: formatRelative(w.last_seen) }) }} ·
            {{
              w.registries.length
                ? t("securityScanners.covers", { names: w.registries.join(", ") })
                : t("securityScanners.coversAll")
            }}
          </span>
        </span>
        <span v-else class="flex-1">
          <StateMark state="attention" />
          {{
            lastSeen
              ? t("securityScanners.noneRunningSince", { when: formatRelative(lastSeen) })
              : t("securityScanners.noneRunning")
          }}
          <a
            :href="docsUrl('guide/security-gates#worker', locale)"
            class="ml-1 text-primary underline underline-offset-[3px]"
            >{{ t("securityScanners.startWorker") }}</a
          >
        </span>
      </div>
      <div class="flex flex-col gap-1 text-sm sm:flex-row sm:gap-3" data-testid="queue">
        <span class="w-24 shrink-0 font-mono text-xs uppercase tracking-wider">{{
          t("securityScanners.queue")
        }}</span>
        <span class="flex-1 tabular-nums">
          {{
            t("securityScanners.queueLine", {
              queued: data.queue.queued,
              deferred: data.queue.deferred,
              failed: data.queue.failed,
            })
          }}
          <RouterLink
            v-if="data.queue.failed"
            :to="{ path: '/security/scanners', query: { tab: 'failures' } }"
            class="ml-1 text-primary underline underline-offset-[3px]"
            >→ {{ t("securityScanners.failedScans") }}</RouterLink
          >
        </span>
      </div>
      <p v-if="!data.scanners.length" class="text-sm" data-testid="no-scanner">
        <StateMark state="attention" /> {{ t("securityScanners.noScanner") }}
        <a
          :href="docsUrl('guide/security-gates#scanners', locale)"
          class="ml-1 text-primary underline underline-offset-[3px]"
          >{{ t("securityScanners.configureScanner") }}</a
        >
      </p>
      <Table v-else stacked :label="t('securityScanners.scanners')" data-testid="scanner-rows">
        <TableHeader>
          <TableRow>
            <TableHead>{{ t("securityScanners.scanner") }}</TableHead>
            <TableHead>{{ t("securityScanners.lastSuccess") }}</TableHead>
            <TableHead>{{ t("securityScanners.lastError") }}</TableHead>
            <TableHead>{{ t("securityScanners.state") }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow v-for="s in data.scanners" :key="s.name">
            <TableCell class="font-mono text-sm">{{ s.name }}</TableCell>
            <TableCell class="text-xs" :label="t('securityScanners.lastSuccess')">
              <DateTime v-if="s.last_ok_at" :iso="s.last_ok_at" />
              <template v-else>{{ t("common.never") }}</template>
            </TableCell>
            <TableCell class="text-xs" :label="t('securityScanners.lastError')">
              <template v-if="s.last_error_at">
                <DateTime :iso="s.last_error_at" />
                <code v-if="s.last_error_class" class="ml-1 font-mono">{{
                  s.last_error_class
                }}</code>
              </template>
              <template v-else>—</template>
            </TableCell>
            <TableCell class="text-xs" :label="t('securityScanners.state')">
              <StateMark :state="scannerState(s.state)" />
              <span v-if="s.state === 'never_ran'">{{ t("securityScanners.neverRan") }}</span>
              <span v-else-if="s.unavailable_until" class="text-muted-foreground">{{
                t("securityScanners.breakerOpen")
              }}</span>
            </TableCell>
          </TableRow>
        </TableBody>
      </Table>
    </section>
  </AsyncState>
</template>
