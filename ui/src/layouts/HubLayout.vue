<script setup lang="ts">
import { useI18n } from "vue-i18n";
/**
 * The shared shell for a tabbed hub (`/me`, `/tools`).
 *
 * Four account routes and two diagnostics routes were previously top-level
 * destinations reachable only from a dropdown. Grouping them means one heading,
 * one tab strip, and a URL per tab that still deep-links — the tabs are routed,
 * not local state, so a tab is bookmarkable and the back button works.
 *
 * Routed rather than eager-rendered matters for `/me/tokens` specifically: token
 * values must not render on a surface the user did not deliberately open.
 */
import { RouterLink, RouterView, useRoute } from "vue-router";
import type { NavLink } from "@/config/navigation";

const { t } = useI18n();

defineProps<{ title: string; tabs: NavLink[]; description?: string }>();

const route = useRoute();
const isActive = (to: string): boolean => route.path === to || route.path.startsWith(`${to}/`);
</script>

<template>
  <div class="space-y-6">
    <header class="space-y-1">
      <h1 class="font-display text-2xl font-bold tracking-[0.04em]">{{ title }}</h1>
      <p v-if="description" class="text-sm text-muted-foreground">{{ description }}</p>
    </header>

    <nav :aria-label="t('a11y.sectionsOf', { title })" class="border-b border-border">
      <ul class="-mb-px flex flex-wrap gap-1">
        <li v-for="tab in tabs" :key="tab.to">
          <RouterLink
            :to="tab.to"
            :aria-current="isActive(tab.to) ? 'page' : undefined"
            class="inline-block border-b px-3 py-2 font-mono text-sm"
            :class="
              isActive(tab.to)
                ? 'border-primary text-foreground'
                : 'border-transparent text-muted-foreground hover:text-foreground'
            "
          >
            {{ t(tab.label) }}
          </RouterLink>
        </li>
      </ul>
    </nav>

    <RouterView />
  </div>
</template>
