<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { RouterView, RouterLink, useRoute } from "vue-router";
import { ShieldCheck } from "@lucide/vue";
import { ADMIN_SIDEBAR } from "@/config/adminSections";

const { t } = useI18n();
const route = useRoute();

function isActive(to: string) {
  return route.path === to || route.path.startsWith(to + "/");
}
</script>

<template>
  <!--
    Column on mobile, row from `md` up.

    It was a row at every width, and the mobile tab strip below is `w-full` —
    so under `md` the strip and the content sat *side by side* in the same flex
    row and the document came out 496–705px wide on a 390px viewport. Every one
    of the fifteen admin pages scrolled sideways, and none of the design gates
    could see it: the rendered detector runs 390x844 but only over the
    unauthenticated routes, and the authenticated gate covers `/admin/*` at
    1440x900 only. `design-authed.mjs` now measures both widths (RFC 0004 §10).
  -->
  <div class="flex flex-col md:flex-row md:gap-6 min-h-[calc(100vh-3.5rem-1px)]">
    <!--
      The selected section is ink on a 1px crimson edge — DESIGN's selection
      edge, one of crimson's four jobs — and not a filled row with a 2px
      stripe, which is the thick side stripe the system refuses. `aria-current`
      is set by hand because a section link is never *exactly* active: the
      route is always one of its tabs, so RouterLink's own exact match never
      fired and a screen reader heard seven links and no position.
    -->
    <aside class="hidden md:flex flex-col w-52 shrink-0 border-r border-rule-soft pr-4 pt-2">
      <div
        class="flex items-center gap-2 px-3 py-2 mb-2 font-mono text-xs uppercase tracking-wider text-muted-foreground"
      >
        <ShieldCheck class="h-3.5 w-3.5" aria-hidden="true" />
        {{ t("common.admin") }}
      </div>
      <nav class="flex flex-col" :aria-label="t('a11y.sectionsOf', { title: t('common.admin') })">
        <RouterLink
          v-for="link in ADMIN_SIDEBAR"
          :key="link.to"
          :to="link.to"
          :aria-current="isActive(link.to) ? 'page' : undefined"
          :class="[
            'flex items-center gap-2.5 border-l px-3 py-2 font-mono text-sm',
            isActive(link.to)
              ? 'border-primary text-foreground'
              : 'border-transparent text-muted-foreground hover:text-foreground',
          ]"
        >
          <component :is="link.icon" class="h-4 w-4 shrink-0" aria-hidden="true" />
          {{ t(link.label) }}
        </RouterLink>
      </nav>
    </aside>

    <!--
      Mobile: the sections wrap rather than scroll. A horizontal strip whose
      only sign of more sections was the browser's own scrollbar hid four of
      seven behind a gesture nobody is told to make.
    -->
    <nav
      class="md:hidden mb-4 flex w-full flex-wrap items-center gap-x-1 gap-y-1 border-b border-rule-soft pb-2"
      :aria-label="t('a11y.sectionsOf', { title: t('common.admin') })"
    >
      <RouterLink
        v-for="link in ADMIN_SIDEBAR"
        :key="link.to"
        :to="link.to"
        :aria-current="isActive(link.to) ? 'page' : undefined"
        :class="[
          'flex items-center gap-1.5 border-b px-2 py-1.5 font-mono text-sm whitespace-nowrap',
          isActive(link.to)
            ? 'border-primary text-foreground'
            : 'border-transparent text-muted-foreground hover:text-foreground',
        ]"
      >
        <component :is="link.icon" class="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
        {{ t(link.label) }}
      </RouterLink>
    </nav>

    <!-- Content -->
    <div class="flex-1 min-w-0 pt-2">
      <RouterView />
    </div>
  </div>
</template>
