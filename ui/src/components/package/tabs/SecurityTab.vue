<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";

import type { ScanStatusDto } from "@/client/types.gen";
import type { PollStop } from "@/composables/usePoll";
import { Button } from "@/components/ui/button";
import { formatDay } from "@/lib/format";
import ScanBadge from "@/components/package/ScanBadge.vue";
import VerdictPanel from "@/components/package/VerdictPanel.vue";

/**
 * The package page's Security tab (RFC 0044 §4.5): the selected version's
 * scan status, its verdict and findings, then the page's vulnerability and
 * SBOM blocks through the default slot.
 *
 * The status comes from the explore detail the page polls, never from the
 * verdict GET, whose `404` with no verdict row `batlehub-cli wait` reads
 * (decision 42).
 */
const props = defineProps<{
  registry: string;
  name: string;
  version: string | null;
  status?: ScanStatusDto | null;
  /** The page's status batch failed. */
  unavailable: boolean;
  /** `gates:exempt`; an admin also always holds `quarantine:read`, so for
      them an absent status can only mean the registry scans nothing. */
  isAdmin: boolean;
  /** Why the poll stopped, if it did. */
  pollStopped: PollStop | null;
}>();
defineEmits<{ refresh: [] }>();

const { t } = useI18n();

const transient = computed(() => ["queued", "scanning"].includes(props.status?.state ?? ""));

/** The line after the badge: the attempt, or since when it waits. */
const detailLine = computed(() => {
  const d = props.status?.detail;
  if (!d) return "";
  if (props.status?.state === "scanning" && d.attempts != null)
    return t("verdictPanel.attempt", { attempt: d.attempts });
  if (props.status?.state === "queued") {
    if (d.deferred_since)
      return t("verdictPanel.deferredSince", { when: formatDay(d.deferred_since) });
    if (d.queued_since) return t("verdictPanel.waitingSince", { when: formatDay(d.queued_since) });
  }
  return "";
});

const noScanning = computed(() => props.isAdmin && !props.status && !props.unavailable);
</script>

<template>
  <div class="space-y-4">
    <p v-if="noScanning" class="text-sm text-muted-foreground" data-testid="no-scanning">
      {{ t("verdictPanel.noScanning") }}
    </p>
    <div v-else-if="status || unavailable" class="space-y-1 text-sm" data-testid="scan-line">
      <p class="flex flex-wrap items-center gap-2">
        <ScanBadge :status="status" :unavailable="unavailable" />
        <span v-if="detailLine" class="text-muted-foreground">— {{ detailLine }}</span>
      </p>
      <!-- Announced once per change of state, not every 5 s. -->
      <p v-if="transient" class="text-xs text-muted-foreground" aria-live="polite">
        <template v-if="pollStopped">
          {{
            pollStopped === "failed" ? t("verdictPanel.pollFailed") : t("verdictPanel.pollStopped")
          }}
          <Button
            variant="outline"
            size="sm"
            class="ml-2"
            data-testid="poll-refresh"
            @click="$emit('refresh')"
          >
            {{ t("common.refresh") }}
          </Button>
        </template>
        <template v-else>{{ t("verdictPanel.scanning") }}</template>
      </p>
    </div>

    <!-- Keyed on the state, so a scan that finishes under the poll reloads
         the verdict it wrote. -->
    <VerdictPanel
      :key="status?.state ?? 'none'"
      :registry="registry"
      :name="name"
      :version="version"
      :can-rescan="isAdmin"
    />

    <slot />
  </div>
</template>
