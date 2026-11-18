<script setup lang="ts">
import { useI18n } from "vue-i18n";

/**
 * A server code drawn as its translated label with the raw code beside it in
 * mono (RFC 0050 §4.2): `SOC verdict  SOC_VERDICT`. A code the table does not
 * know is drawn raw, alone — never as a missing key.
 */
const props = defineProps<{ code: string; labels: Record<string, string> }>();
const { t } = useI18n();
</script>

<template>
  <span class="inline-flex flex-wrap items-baseline gap-x-2">
    <template v-if="props.labels[props.code]">
      <span>{{ t(props.labels[props.code]!) }}</span>
      <code class="font-mono text-xs text-muted-foreground">{{ props.code }}</code>
    </template>
    <code v-else class="font-mono text-xs">{{ props.code }}</code>
  </span>
</template>
