<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { computed } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";

/** `label` is an i18n key, resolved here — see `config/adminSections.ts`. */

const { t } = useI18n();

const props = defineProps<{
  tabs: { to: string; label: string }[];
}>();

const route = useRoute();
const router = useRouter();

function isActive(to: string) {
  return route.path === to || route.path.startsWith(to + "/");
}

const current = computed(() => props.tabs.find((tab) => isActive(tab.to))?.to ?? "");

function go(to: string) {
  if (to && to !== current.value) void router.push(to);
}
</script>

<template>
  <!-- A 1px crimson edge under the current tab — the same selection edge as
       the sidebar — not a 2px stripe; and `aria-current`, because the active
       state was colour and weight only. -->
  <!-- Mobile: one native selector naming the current tab. Wrapped, seven
       tabs were three rows of links under the section menu; a select is
       one row, opens the platform's own picker, and keeps every tab one tap
       away. -->
  <div class="md:hidden mb-4">
    <label for="section-tab-select" class="sr-only">{{ t("a11y.inThisSection") }}</label>
    <select
      id="section-tab-select"
      class="h-10 w-full border border-border bg-background px-3 font-mono text-sm"
      :value="current"
      @change="go(($event.target as HTMLSelectElement).value)"
    >
      <option v-for="tab in tabs" :key="tab.to" :value="tab.to">{{ t(tab.label) }}</option>
    </select>
  </div>
  <nav
    class="hidden md:flex items-center gap-1 flex-wrap border-b border-rule-soft mb-4"
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
