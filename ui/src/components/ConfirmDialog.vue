<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { Dialog } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";

const { t } = useI18n();

const props = withDefaults(
  defineProps<{
    open: boolean;
    title?: string;
    description?: string;
    confirmLabel?: string;
    cancelLabel?: string;
    loadingLabel?: string;
    destructive?: boolean;
    loading?: boolean;
    error?: string | null;
  }>(),
  {
    title: undefined,
    description: undefined,
    confirmLabel: undefined,
    cancelLabel: undefined,
    loadingLabel: undefined,
    destructive: false,
    loading: false,
    error: null,
  },
);

/* Resolved here rather than in `withDefaults`, which evaluates once at module
   scope: the English would be frozen in for the life of the tab, including
   across a locale switch. Every call site that passes its own verb still wins. */
const confirmText = computed(() => props.confirmLabel ?? t("common.confirm"));
const cancelText = computed(() => props.cancelLabel ?? t("common.cancel"));

const emit = defineEmits<{
  "update:open": [boolean];
  confirm: [];
}>();

function cancel() {
  emit("update:open", false);
}
</script>

<template>
  <Dialog :open="open" @update:open="emit('update:open', $event)">
    <template #title><slot name="title">{{ title }}</slot></template>
    <template v-if="description || $slots.description" #description>
      <slot name="description">{{ description }}</slot>
    </template>
    <div class="space-y-4">
      <p v-if="error" class="text-sm text-destructive">{{ error }}</p>
      <div class="flex justify-end gap-2">
        <Button variant="outline" size="sm" :disabled="loading" @click="cancel">
          {{ cancelText }}
        </Button>
        <Button
          :variant="destructive ? 'destructive' : 'default'"
          size="sm"
          :disabled="loading"
          @click="emit('confirm')"
        >
          {{ loading ? (loadingLabel ?? confirmText) : confirmText }}
        </Button>
      </div>
    </div>
  </Dialog>
</template>
