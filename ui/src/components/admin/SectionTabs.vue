<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { RouterLink, useRoute } from "vue-router";

/** `label` is an i18n key, resolved here — see `config/adminSections.ts`. */
defineProps<{
  tabs: { to: string; label: string }[];
}>();

const { t } = useI18n();

const route = useRoute();

function isActive(to: string) {
  return route.path === to || route.path.startsWith(to + "/");
}
</script>

<template>
  <!-- A 1px crimson edge under the current tab — the same selection edge as
       the sidebar — not a 2px stripe; and `aria-current`, because the active
       state was colour and weight only. -->
  <nav
    class="flex items-center gap-1 flex-wrap border-b border-rule-soft mb-4"
    :aria-label="t('a11y.inThisSection')"
  >
    <RouterLink
      v-for="tab in tabs"
      :key="tab.to"
      :to="tab.to"
      :aria-current="isActive(tab.to) ? 'page' : undefined"
      :class="[
        'px-3 py-1.5 font-mono text-sm whitespace-nowrap border-b -mb-px',
        isActive(tab.to)
          ? 'border-primary text-foreground'
          : 'border-transparent text-muted-foreground hover:text-foreground',
      ]"
    >
      {{ t(tab.label) }}
    </RouterLink>
  </nav>
</template>
