<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";
import type { SecurityOverview } from "@/client/types.gen";
import { Button } from "@/components/ui/button";
import StateMark, { type StatusState } from "./StateMark.vue";

/**
 * The status strip of RFC 0050 §4.5: one line per domain, state first, one
 * sentence saying what happens to a request, a link to the page that acts on
 * it. A domain absent from the body (verb not held) has no line — an absence,
 * never a locked row. Shared by `/security` and the admin dashboard.
 */
export type Domain = "holds" | "flags" | "scanners" | "coverage";

const props = defineProps<{
  overview: SecurityOverview;
  domains: Domain[];
}>();
const emit = defineEmits<{ retry: [] }>();
const { t } = useI18n();

interface Line {
  domain: Domain;
  state: StatusState;
  text: string;
  to: string;
}

function holdsText(o: NonNullable<SecurityOverview["holds"]>): string {
  if (!o.held) return t("securityStatus.holdsNone");
  // A SOC's hard blocks first: in a mass compromise, the figure to read.
  if (o.soc_verdict) return t("securityStatus.holdsSoc", { n: o.held, soc: o.soc_verdict });
  return o.malware
    ? t("securityStatus.holdsMalware", { n: o.held, malware: o.malware })
    : t("securityStatus.holds", { n: o.held, waiting: o.waiting });
}

function flagsText(o: NonNullable<SecurityOverview["flags"]>): string {
  if (!o.blocking) return t("securityStatus.flagsNone");
  return o.pulled_after_flag
    ? t("securityStatus.flagsPulled", { n: o.blocking, pulled: o.pulled_after_flag })
    : t("securityStatus.flags", { n: o.blocking });
}

function scannersText(o: NonNullable<SecurityOverview["scanners"]>): string {
  if (o.uncovered.length)
    return t("securityStatus.scannersUncovered", { names: o.uncovered.join(", ") });
  if (o.breaker_open) return t("securityStatus.scannersBreaker");
  if (o.never_ran) return t("securityStatus.scannersNeverRan", { n: o.never_ran });
  if (o.scanners_attention) return t("securityStatus.scannersFailing", { n: o.scanners_attention });
  if (o.deferred_over_day) return t("securityStatus.scannersDeferred", { n: o.deferred_over_day });
  return t("securityStatus.scanners", {
    n: o.live_workers,
    queued: o.queued,
    failed: o.failed,
  });
}

/** `upstream`: the registries serving upstream traffic, the only ones the line judges. */
function coverageText(o: NonNullable<SecurityOverview["coverage"]>): string {
  const upstream = o.upstream;
  // "Nothing protected" only when no `cve_gate` covers anything either.
  if (!o.protected && o.unprotected.length === upstream) return t("securityStatus.coverageNone");
  if (!o.unprotected.length) return t("securityStatus.coverageAll", { n: upstream });
  return t("securityStatus.coverage", { n: o.unprotected.length, total: upstream });
}

const lines = computed<Line[]>(() => {
  const o = props.overview;
  const out: Line[] = [];
  for (const domain of props.domains) {
    const block = o[domain];
    if (!block) continue;
    const state = block.state as StatusState;
    const text =
      state === "unknown"
        ? t("securityStatus.couldNotRead")
        : domain === "holds" && o.holds
          ? holdsText(o.holds)
          : domain === "flags" && o.flags
            ? flagsText(o.flags)
            : domain === "scanners" && o.scanners
              ? scannersText(o.scanners)
              : o.coverage
                ? coverageText(o.coverage)
                : "";
    const to = {
      holds: o.holds?.soc_verdict ? "/security/holds?reason=SOC_VERDICT" : "/security/holds",
      flags: "/security/flags",
      scanners: "/security/scanners",
      coverage: "/security/coverage",
    }[domain];
    out.push({ domain, state, text, to });
  }
  return out;
});

const DOMAIN_LABEL: Record<Domain, string> = {
  holds: "securityNav.holds",
  flags: "securityNav.flags",
  scanners: "securityNav.scanners",
  coverage: "securityNav.coverage",
};
</script>

<template>
  <ul class="divide-y divide-dashed divide-rule-soft border-y border-rule-soft">
    <li
      v-for="line in lines"
      :key="line.domain"
      class="flex flex-col gap-1 py-2 sm:flex-row sm:items-baseline sm:gap-3"
      :data-testid="`status-${line.domain}`"
    >
      <StateMark :state="line.state" />
      <span class="w-40 shrink-0 font-mono text-sm">{{ t(DOMAIN_LABEL[line.domain]) }}</span>
      <span class="flex-1 text-sm">
        {{ line.text }}
        <Button
          v-if="line.state === 'unknown'"
          variant="outline"
          size="sm"
          class="ml-2"
          @click="emit('retry')"
          >{{ t("common.retry") }}</Button
        >
      </span>
      <RouterLink
        :to="line.to"
        class="shrink-0 text-sm text-primary underline underline-offset-[3px]"
        >→ {{ t(DOMAIN_LABEL[line.domain]) }}</RouterLink
      >
    </li>
    <slot />
  </ul>
</template>
