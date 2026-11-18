<script setup lang="ts">
import { computed } from "vue";
import { formatDate, formatRelative } from "@/lib/format";

/**
 * One date format across the console (RFC 0050 §6.7): `2026-11-16 17:44`,
 * the relative time in a `title`, the instant in `datetime`.
 */
const props = defineProps<{ iso: string | null | undefined }>();
const valid = computed(() => !!props.iso && !Number.isNaN(new Date(props.iso).getTime()));
</script>

<template>
  <time v-if="valid" :datetime="props.iso!" :title="formatRelative(props.iso)">{{
    formatDate(props.iso)
  }}</time>
  <template v-else>{{ formatDate(props.iso) }}</template>
</template>
