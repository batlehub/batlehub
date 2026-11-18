<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import type { CoverageRow } from "@/client/types.gen";

/**
 * How a registry is protected, as one phrase with a glyph (RFC 0050 §4.5):
 * `● refuses high and above`, `◐ warns on critical and above, still refuses known malware`,
 * `○ not protected`. Glyph and words, never colour alone.
 */
const props = defineProps<{ row: CoverageRow }>();
const { t } = useI18n();

const phrase = computed(() => {
  const r = props.row;
  if (!r.protected && !r.cve_gate) return { glyph: "○", text: t("securityCoverage.notProtected") };
  // `security_mode = "warn"` serves with a warning at any severity; only the
  // always-denied codes (known malware, block list, SOC verdict) are refused.
  if (r.security_mode === "warn")
    return {
      glyph: "◐",
      text: r.deny_at
        ? t("securityCoverage.warnsRefuses", { deny: r.deny_at })
        : t("securityCoverage.warnsOnly"),
    };
  if (r.deny_at) return { glyph: "●", text: t("securityCoverage.refusesAt", { deny: r.deny_at }) };
  return { glyph: "●", text: t("securityCoverage.protected") };
});
</script>

<template>
  <span class="inline-flex items-baseline gap-2">
    <span aria-hidden="true" class="font-mono">{{ phrase.glyph }}</span>
    {{ phrase.text }}
  </span>
</template>
