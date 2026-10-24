<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";

const { t } = useI18n();

/**
 * Loading, then error, then empty, then content — in that order, and only one
 * of them. The order is the point: a failed load that also rendered its empty
 * state ("No packages cached yet") and a "0 packages" caption told the reader
 * three things, two of them false.
 *
 * An error says *what* failed (`title`) above the transport's own message, and
 * offers a retry whenever the page passes `onRetry`: a bare "Failed to fetch"
 * with no way forward was the console's answer to every backend hiccup.
 */
const props = withDefaults(
  defineProps<{
    loading: boolean;
    error?: string | null;
    empty?: boolean;
    emptyMessage?: string;
    /** What could not be loaded, in the reader's words — "Packages could not be loaded." */
    title?: string;
    onRetry?: () => void;
  }>(),
  { error: null, empty: false, emptyMessage: undefined, title: undefined, onRetry: undefined },
);

/* The defaults are resolved here rather than in `withDefaults`, which evaluates
   once at module scope and would freeze the English in place — including for a
   viewer who switches locale without a reload. */
const emptyText = computed(() => props.emptyMessage ?? t("asyncState.noResults"));
const titleText = computed(() => props.title ?? t("asyncState.couldNotLoad"));
</script>

<template>
  <template v-if="loading">
    <slot name="loading">
      <p class="text-sm text-muted-foreground py-4" role="status">{{ t("asyncState.loading") }}</p>
    </slot>
  </template>
  <template v-else-if="error">
    <slot name="error" :error="error">
      <Alert variant="destructive" class="space-y-2">
        <p class="font-medium">{{ titleText }}</p>
        <p class="text-xs break-words">{{ error }}</p>
        <Button v-if="onRetry" variant="outline" size="sm" @click="onRetry()">
          {{ t("common.retry") }}
        </Button>
      </Alert>
    </slot>
  </template>
  <template v-else-if="empty">
    <slot name="empty">
      <p class="text-sm text-muted-foreground text-center py-4">{{ emptyText }}</p>
    </slot>
  </template>
  <template v-else>
    <slot />
  </template>
</template>
