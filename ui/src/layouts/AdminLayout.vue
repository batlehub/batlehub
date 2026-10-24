<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { RouterView, RouterLink, useRoute } from "vue-router";
import { ChevronDown, ShieldCheck } from "@lucide/vue";
import { ADMIN_SIDEBAR } from "@/config/adminSections";

const { t } = useI18n();
const route = useRoute();

function isActive(to: string) {
  return route.path === to || route.path.startsWith(to + "/");
}

/** The section the route is in, named on the mobile trigger. */
const currentSection = computed(() => ADMIN_SIDEBAR.find((link) => isActive(link.to)));

/** Mobile: the sections sit behind one trigger that says where you are. */
const sectionsOpen = ref(false);
watch(
  () => route.path,
  () => {
    sectionsOpen.value = false;
  },
);
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
      Mobile: one trigger that names where you are — "Admin · Operations" —
      and opens the seven sections. Wrapped, they were four rows of links
      above three rows of tabs, ~300px before the page began on a phone;
      scrolled sideways before that, four of seven hid behind a gesture
      nobody is told to make. Not a second ☰: the header's own burger opens
      the site menu, and two identical icons opening different things is a
      guess the reader should not have to make. It closes on navigation.
    -->
    <div class="md:hidden mb-3 w-full">
      <button
        type="button"
        class="flex w-full items-center justify-between gap-2 border border-border px-3 py-2 font-mono text-sm"
        :aria-expanded="sectionsOpen"
        aria-controls="admin-sections-mobile"
        @click="sectionsOpen = !sectionsOpen"
      >
        <span class="flex min-w-0 items-center gap-2">
          <ShieldCheck class="h-3.5 w-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
          <span class="text-muted-foreground">{{ t("common.admin") }} ·</span>
          <span class="truncate">{{ currentSection ? t(currentSection.label) : "" }}</span>
        </span>
        <ChevronDown
          class="h-4 w-4 shrink-0 text-muted-foreground"
          :class="sectionsOpen ? 'rotate-180' : ''"
          aria-hidden="true"
        />
      </button>
      <nav
        v-show="sectionsOpen"
        id="admin-sections-mobile"
        class="flex flex-col border border-t-0 border-border"
        :aria-label="t('a11y.sectionsOf', { title: t('common.admin') })"
      >
        <RouterLink
          v-for="link in ADMIN_SIDEBAR"
          :key="link.to"
          :to="link.to"
          :aria-current="isActive(link.to) ? 'page' : undefined"
          :class="[
            'flex items-center gap-2.5 border-l px-3 py-2.5 font-mono text-sm',
            isActive(link.to)
              ? 'border-primary text-foreground'
              : 'border-transparent text-muted-foreground hover:text-foreground',
          ]"
        >
          <component :is="link.icon" class="h-4 w-4 shrink-0" aria-hidden="true" />
          {{ t(link.label) }}
        </RouterLink>
      </nav>
    </div>

    <!-- Content -->
    <div class="flex-1 min-w-0 pt-2">
      <RouterView />
    </div>
  </div>
</template>
