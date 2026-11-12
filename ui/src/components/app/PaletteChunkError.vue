<script setup lang="ts">
/**
 * What the palette's lazy chunk shows when it cannot load (RFC 0046 §10.2): a
 * redeploy rotated the hashes under a long-lived tab. A stated notice, not a
 * shortcut that silently does nothing.
 */
import { useI18n } from "vue-i18n";

const { t } = useI18n();
</script>

<template>
  <p role="status" class="border-b border-rule-soft px-4 py-2 text-sm md:px-6">
    {{ t("palette.chunkFailed") }}
  </p>
</template>
