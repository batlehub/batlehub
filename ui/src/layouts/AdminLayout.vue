<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { RouterView, RouterLink, useRoute } from "vue-router";
import { ChevronDown, ShieldCheck } from "@lucide/vue";
import { ADMIN_SECTIONS } from "@/config/adminSections";
import { useAuth } from "@/composables/useAuth";

const { t } = useI18n();
const route = useRoute();

function isActive(to: string) {
  return route.path === to || route.path.startsWith(to + "/");
}

const { can } = useAuth();
/**
 * Each section with only the pages the viewer may open, and no section left
 * empty (RFC 0042 §4.3): an absent row, never a greyed-out one (RFC 0046 M1′).
 * `can(verb)` at the instance tier, the call the router guard makes.
 */
const sections = computed(() =>
  ADMIN_SECTIONS.map((section) => ({
    ...section,
    pages: section.pages.filter((page) => can(page.verb)),
  })).filter((section) => section.pages.length > 0),
);

/** A section that is one page at its own path is a link, not a heading (the dashboard). */
function isSinglePage(section: (typeof ADMIN_SECTIONS)[number]) {
  return section.pages.length === 1 && section.pages[0]?.to === section.to;
}

/** The section the route is in, named on the mobile trigger. */
const currentSection = computed(() => sections.value.find((section) => isActive(section.to)));

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
      <!--
        RFC 0046 M1: every page the viewer holds is one click away, under a
        heading naming its section. The headings are text, not links, and not
        focusable, so Tab moves page to page; each group is labelled by its
        heading for a screen reader. French labels wrap, never truncate: a cut
        label hides the word the operator scans for.
      -->
      <nav class="flex flex-col" :aria-label="t('a11y.sectionsOf', { title: t('common.admin') })">
        <template v-for="section in sections" :key="section.to">
          <RouterLink
            v-if="isSinglePage(section)"
            :to="section.to"
            :aria-current="isActive(section.to) ? 'page' : undefined"
            :class="[
              'flex items-center gap-2 border-l px-3 py-2 font-mono text-sm',
              isActive(section.to)
                ? 'border-primary text-foreground'
                : 'border-transparent text-muted-foreground hover:text-foreground',
            ]"
          >
            <component :is="section.icon" class="h-4 w-4 shrink-0" aria-hidden="true" />
            {{ t(section.label) }}
          </RouterLink>
          <div
            v-else
            role="group"
            :aria-labelledby="`admin-section-${section.to.slice(7)}`"
            class="mt-3 flex flex-col"
          >
            <span
              :id="`admin-section-${section.to.slice(7)}`"
              class="flex items-center gap-2 px-3 py-1 font-mono text-xs uppercase tracking-wider text-muted-foreground"
            >
              <component :is="section.icon" class="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
              {{ t(section.label) }}
            </span>
            <RouterLink
              v-for="page in section.pages"
              :key="page.to"
              :to="page.to"
              :aria-current="isActive(page.to) ? 'page' : undefined"
              :class="[
                'border-l py-1 pl-10 pr-3 font-mono text-sm',
                isActive(page.to)
                  ? 'border-primary text-foreground'
                  : 'border-transparent text-muted-foreground hover:text-foreground',
              ]"
            >
              {{ t(page.label) }}
            </RouterLink>
          </div>
        </template>
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

      RFC 0046 M1″: the expansion above is desktop-only. This menu stays a list
      of sections; on a phone the pages are one tap away in `SectionTabs`.
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
          v-for="section in sections"
          :key="section.to"
          :to="section.to"
          :aria-current="isActive(section.to) ? 'page' : undefined"
          :class="[
            'flex items-center gap-2 border-l px-3 py-2 font-mono text-sm',
            isActive(section.to)
              ? 'border-primary text-foreground'
              : 'border-transparent text-muted-foreground hover:text-foreground',
          ]"
        >
          <component :is="section.icon" class="h-4 w-4 shrink-0" aria-hidden="true" />
          {{ t(section.label) }}
        </RouterLink>
      </nav>
    </div>

    <!-- Content -->
    <div class="flex-1 min-w-0 pt-2">
      <RouterView />
    </div>
  </div>
</template>
