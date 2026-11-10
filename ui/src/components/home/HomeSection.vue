<script setup lang="ts">
/**
 * One of the home page's personal sections: a meta-size heading, then loading,
 * error or the section's own content. The three widgets each wrote this chain
 * out by hand (heading, Skeleton, Alert) and drifted apart doing it.
 *
 * Loading keeps the Skeleton rather than AsyncState's panel: these sections
 * load beside each other, and a ruled placeholder shaped like a list holds the
 * page still where three 40px-padded panels would jump it twice.
 */
import { useI18n } from "vue-i18n";
import { AsyncState } from "@/components/ui/async-state";
import { Skeleton } from "@/components/ui/skeleton";

defineProps<{
  /** Id of the heading, which names the section for assistive tech. */
  headingId: string;
  title: string;
  loading: boolean;
  error: string | null;
}>();

const { t } = useI18n();
</script>

<template>
  <section :aria-labelledby="headingId">
    <h2 :id="headingId" class="font-mono text-xs uppercase tracking-[0.16em] text-muted-foreground">
      {{ title }}
    </h2>
    <div class="mt-3">
      <AsyncState :loading="loading" :error="error" :title="t('asyncState.couldNotLoad')">
        <template #loading><Skeleton :lines="2" /></template>
        <slot />
      </AsyncState>
    </div>
  </section>
</template>
