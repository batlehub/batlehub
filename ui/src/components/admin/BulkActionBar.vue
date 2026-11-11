<script setup lang="ts">
/**
 * The bar a table's selection raises: how many rows, the verbs, Clear, and
 * an optional far-end slot for the one filled action (set apart so it is never
 * the button beside the one you meant). It sticks under the masthead.
 */
import { useI18n } from "vue-i18n";
import { Button } from "@/components/ui/button";

defineProps<{ label: string }>();
const emit = defineEmits<{ clear: [] }>();
const { t } = useI18n();
</script>

<template>
  <div
    class="sticky top-[var(--masthead-h,4rem)] z-30 flex flex-wrap items-center gap-3 border border-rule-strong bg-background px-4 py-2"
  >
    <span class="text-sm font-medium">{{ label }}</span>
    <slot />
    <Button size="sm" variant="ghost" @click="emit('clear')">{{ t("common.clearAction") }}</Button>
    <slot name="end" />
  </div>
</template>
