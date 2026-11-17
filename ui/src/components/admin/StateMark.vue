<script setup lang="ts">
import { useI18n } from "vue-i18n";

/**
 * A status line's state as glyph and word (RFC 0050 §4.2): `● OK`,
 * `▲ ATTENTION`, `? UNKNOWN`. Never colour alone; and `unknown` is never drawn
 * as OK — a source that did not answer does not reassure.
 */
export type StatusState = "ok" | "attention" | "unknown";

const props = defineProps<{ state: StatusState }>();
const { t } = useI18n();

const GLYPH: Record<StatusState, string> = { ok: "●", attention: "▲", unknown: "?" };
const WORD: Record<StatusState, string> = {
  ok: "securityStatus.ok",
  attention: "securityStatus.attention",
  unknown: "securityStatus.unknown",
};
const TONE: Record<StatusState, string> = {
  ok: "text-foreground",
  attention: "text-copper",
  unknown: "text-muted-foreground",
};
</script>

<template>
  <span
    class="inline-flex w-28 shrink-0 items-center gap-1 font-mono text-xs uppercase tracking-wider"
    :class="TONE[props.state]"
    :data-state="props.state"
  >
    <span aria-hidden="true">{{ GLYPH[props.state] }}</span>
    {{ t(WORD[props.state]) }}
  </span>
</template>
