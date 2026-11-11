<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { Circle, CircleCheck, CircleDashed, CircleX, LoaderCircle, Minus } from "@lucide/vue";

import type { ScanStatusDto } from "@/client/types.gen";
import { Badge } from "@/components/ui/badge";
import type { BadgeVariant } from "@/lib/badge-variants";

/**
 * Where one version stands in scanning (RFC 0044 §4.5): one component for the
 * header, the catalog, the admin list and the Versions tab.
 *
 * The text carries the state, never the colour alone. No `status` and no
 * `unavailable` draws nothing: a registry without `[security]` and a caller
 * without `quarantine:read` look the same on purpose (§4.2).
 */
const props = defineProps<{
  status?: ScanStatusDto | null;
  /** The page's batch failed: draw "—" with its tooltip. */
  unavailable?: boolean;
  /** A list row: the verdict is the stored one, so the tooltip says so. */
  inList?: boolean;
}>();

const { t } = useI18n();

/** VerdictPanel's `stateVariant`, so the header and the panel never disagree. */
const VERDICT_VARIANT: Record<string, BadgeVariant> = {
  denied: "destructive",
  quarantined: "copper",
  warned: "outline",
  allowed: "secondary",
};

/** Spelled out, so the catalogue test sees every key used. */
function verdictLabel(verdict: string): string {
  switch (verdict) {
    case "warned":
      return t("scanBadge.verdict.warned");
    case "quarantined":
      return t("scanBadge.verdict.quarantined");
    case "denied":
      return t("scanBadge.verdict.denied");
    default:
      return t("scanBadge.verdict.allowed");
  }
}

function failedHelp(d: ScanStatusDto["detail"]): string {
  if (d == null) return "";
  const attempts = d.attempts ?? "?";
  return d.last_error
    ? t("scanBadge.failedHelp", { attempts, error: d.last_error })
    : t("scanBadge.failedNoError", { attempts });
}

const view = computed(() => {
  const s = props.status;
  if (!s) {
    return props.unavailable
      ? {
          label: "—",
          help: t("scanBadge.unavailable"),
          variant: "outline" as BadgeVariant,
          icon: Minus,
        }
      : null;
  }
  const d = s.detail;
  switch (s.state) {
    case "not_scanned":
      return {
        label: t("scanBadge.notScanned"),
        help: t("scanBadge.notScannedHelp"),
        variant: "outline" as BadgeVariant,
        icon: Circle,
      };
    case "queued":
      return {
        label: t("scanBadge.queued"),
        help: t("scanBadge.queuedHelp"),
        variant: "outline" as BadgeVariant,
        icon: CircleDashed,
      };
    case "scanning":
      return {
        label: t("scanBadge.scanning"),
        help:
          d?.attempts != null
            ? t("scanBadge.scanningHelp", { attempt: d.attempts })
            : t("scanBadge.scanningHelpNoAttempt"),
        variant: "outline" as BadgeVariant,
        icon: LoaderCircle,
      };
    case "failed":
      return {
        label: t("scanBadge.failed"),
        help: failedHelp(d),
        variant: "destructive" as BadgeVariant,
        icon: CircleX,
      };
    default: {
      const verdict = s.verdict ?? "allowed";
      return {
        label: t("scanBadge.scanned", { verdict: verdictLabel(verdict) }),
        help: props.inList ? t("scanBadge.asLastRecorded") : "",
        variant: VERDICT_VARIANT[verdict] ?? "secondary",
        icon: CircleCheck,
      };
    }
  }
});
</script>

<template>
  <Badge
    v-if="view"
    :variant="view.variant"
    class="gap-1 whitespace-normal text-xs"
    :title="view.help || undefined"
    :aria-label="view.help ? `${view.label} — ${view.help}` : undefined"
    data-testid="scan-badge"
    :data-state="status?.state ?? 'unavailable'"
  >
    <component :is="view.icon" class="h-3 w-3 shrink-0" aria-hidden="true" />
    {{ view.label }}
  </Badge>
</template>
