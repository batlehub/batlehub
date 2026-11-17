<script setup lang="ts">
import { computed, ref, watch, type Component } from "vue";
import { useI18n } from "vue-i18n";
import { RouterView, RouterLink, useRoute } from "vue-router";
import { ChevronDown, ChevronRight, Search } from "@lucide/vue";
import { usePalette } from "@/composables/usePalette";
import { useAuth } from "@/composables/useAuth";
import { canOpen, type AdminSection } from "@/config/adminSections";

/**
 * The sidebar shell of RFC 0050 §5.3, shared by the admin and the security
 * spaces: verb filtering, a one-page section as a link, collapsible groups
 * whose state is kept per browser, and the phone menu. The two layouts that
 * use it only hand it their table.
 */
const props = defineProps<{
  sections: AdminSection[];
  /** i18n key of the space's name, above the sidebar and on the phone trigger. */
  title: string;
  icon: Component;
  /** `admin` or `security`: the DOM id prefix and the `batlehub.nav.<key>` store. */
  navKey: string;
}>();

const { t } = useI18n();
const route = useRoute();
const { can } = useAuth();
const { openPalette, chord } = usePalette();

function isActive(to: string) {
  return route.path === to || route.path.startsWith(to + "/");
}

/**
 * Each section with only the pages the viewer may open, and no section left
 * empty (RFC 0042 §4.3): an absent row, never a greyed-out one (RFC 0046 M1′).
 * `can(verb)` at the instance tier, the call the router guard makes. Built
 * before the stored open state is applied, so a stored name of a hidden
 * group opens nothing (RFC 0050 T10).
 */
const sections = computed(() =>
  props.sections
    .map((section) => ({
      ...section,
      pages: section.pages.filter((page) => canOpen(can, page.verb)),
    }))
    .filter((section) => section.pages.length > 0),
);

/** A section that is one page at its own path is a link, not a group (the dashboard). */
function isSinglePage(section: AdminSection) {
  return section.pages.length === 1 && section.pages[0]?.to === section.to;
}

/** A section owns its own prefix and, for Registries, one registry's page. */
function sectionActive(section: AdminSection) {
  return isActive(section.to) || (!!section.also && route.path.startsWith(section.also));
}

/**
 * The page the route is on: the longest matching path, because "All
 * registries" (`/admin/registries`) prefixes its sibling pages and the
 * security overview (`/security`) prefixes every page of its space; one
 * registry's page belongs to that list.
 */
const activePage = computed(() => {
  const all = props.sections.flatMap((section) => section.pages);
  const hit = all.filter((page) => isActive(page.to)).sort((a, b) => b.to.length - a.to.length)[0];
  if (hit) return hit;
  return props.sections.find((section) => sectionActive(section))?.pages[0];
});

/** The section holding the current page (a security group shares no prefix). */
const currentSection = computed(() =>
  sections.value.find((section) => section.pages.some((page) => page.to === activePage.value?.to)),
);

/*
 * RFC 0050 §4.2: the group holding the current page is always open; the
 * others are open only when the reader opened them, remembered per browser.
 * An unreadable store (private window) reads as "none opened".
 */
const storeKey = `batlehub.nav.${props.navKey}`;
function readOpened(): string[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(storeKey) ?? "[]");
    return Array.isArray(parsed) ? parsed.filter((v): v is string => typeof v === "string") : [];
  } catch {
    return [];
  }
}
const opened = ref(new Set(readOpened()));

function isOpen(section: AdminSection) {
  return section.to === currentSection.value?.to || opened.value.has(section.to);
}

function toggle(section: AdminSection) {
  const next = new Set(opened.value);
  if (!next.delete(section.to)) next.add(section.to);
  opened.value = next;
  try {
    localStorage.setItem(storeKey, JSON.stringify([...next]));
  } catch {
    /* session-only is an acceptable degradation */
  }
}

const GROUP_HEADER =
  "flex w-full items-center gap-2 px-3 py-2 text-left font-mono text-xs uppercase tracking-wider text-muted-foreground";

const groupId = (section: AdminSection) =>
  `${props.navKey}-group-${section.to.split("/").filter(Boolean).join("-")}`;

/** Phone: one trigger naming where you are opens the whole tree; it closes on navigation. */
const menuOpen = ref(false);
watch(
  () => route.path,
  () => {
    menuOpen.value = false;
  },
);
</script>

<template>
  <!--
    Column on mobile, row from `md` up: a flex row at every width once put a
    full-width phone menu beside the content and every admin page scrolled
    sideways on a phone (RFC 0004 §10).
  -->
  <div class="flex flex-col md:flex-row md:gap-6 min-h-[calc(100vh-3.5rem-1px)]">
    <aside
      class="mb-3 flex w-full flex-col md:mb-0 md:w-52 md:shrink-0 md:border-r md:border-rule-soft md:pr-4 md:pt-2"
    >
      <div
        class="hidden md:flex items-center gap-2 px-3 py-2 mb-2 font-mono text-xs uppercase tracking-wider text-muted-foreground"
      >
        <component :is="icon" class="h-3.5 w-3.5" aria-hidden="true" />
        {{ t(title) }}
      </div>
      <!-- RFC 0046 §4.2: the palette's button, at the top of the sidebar. -->
      <button
        type="button"
        class="mb-2 hidden md:flex items-center gap-2 border border-border px-3 py-2 font-mono text-xs text-muted-foreground transition-colors hover:border-muted-foreground hover:text-foreground"
        @click="openPalette"
      >
        <Search class="h-3.5 w-3.5" aria-hidden="true" />
        {{ t("palette.button") }}
        <kbd class="ml-auto">{{ t(chord) }}</kbd>
      </button>

      <!--
        Phone: one trigger that names where you are. Not a second ☰: the
        header's own burger opens the site menu. The tree it opens is the same
        <nav> as the desktop sidebar, so every page is two taps away and the
        removed tab strips took no path with them (RFC 0050 §4.2).
      -->
      <button
        type="button"
        class="md:hidden flex w-full items-center justify-between gap-2 border border-border px-3 py-2 font-mono text-sm"
        :aria-expanded="menuOpen"
        :aria-controls="`${navKey}-nav`"
        @click="menuOpen = !menuOpen"
      >
        <span class="flex min-w-0 items-center gap-2">
          <component
            :is="icon"
            class="h-3.5 w-3.5 shrink-0 text-muted-foreground"
            aria-hidden="true"
          />
          <span class="text-muted-foreground">{{ t(title) }}</span>
          <template v-if="currentSection && !isSinglePage(currentSection)">
            <span class="text-muted-foreground" aria-hidden="true">›</span>
            <span class="text-muted-foreground">{{ t(currentSection.label) }}</span>
          </template>
          <span class="text-muted-foreground" aria-hidden="true">›</span>
          <span class="truncate">{{ activePage ? t(activePage.label) : "" }}</span>
        </span>
        <ChevronDown
          class="h-4 w-4 shrink-0 text-muted-foreground"
          :class="menuOpen ? 'rotate-180' : ''"
          aria-hidden="true"
        />
      </button>

      <!--
        The selected page is ink on a 1px crimson edge — DESIGN's selection
        edge — and `aria-current` is set by hand, because a section is never
        *exactly* the route. A group header is a <button aria-expanded>; the
        current group's is plain text, since it cannot be closed. French
        labels wrap, never truncate.
      -->
      <nav
        :id="`${navKey}-nav`"
        :class="[
          menuOpen ? 'flex' : 'hidden',
          'flex-col border border-t-0 border-border md:flex md:border-0',
        ]"
        :aria-label="t('a11y.sectionsOf', { title: t(title) })"
      >
        <template v-for="section in sections" :key="section.to">
          <RouterLink
            v-if="isSinglePage(section)"
            :to="section.to"
            :aria-current="activePage?.to === section.to ? 'page' : undefined"
            :class="[
              'flex items-center gap-2 border-l px-3 py-2 font-mono text-sm',
              activePage?.to === section.to
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
            :aria-labelledby="`${groupId(section)}-label`"
            class="flex flex-col md:mt-1"
          >
            <span
              v-if="section.to === currentSection?.to"
              :id="`${groupId(section)}-label`"
              :class="GROUP_HEADER"
            >
              <component :is="section.icon" class="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
              <span class="flex-1">{{ t(section.label) }}</span>
              <ChevronDown class="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
            </span>
            <button
              v-else
              :id="`${groupId(section)}-label`"
              type="button"
              :aria-expanded="isOpen(section)"
              :aria-controls="`${groupId(section)}-pages`"
              :class="[GROUP_HEADER, 'hover:text-foreground']"
              @click="toggle(section)"
            >
              <component :is="section.icon" class="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
              <span class="flex-1">{{ t(section.label) }}</span>
              <component
                :is="isOpen(section) ? ChevronDown : ChevronRight"
                class="h-3.5 w-3.5 shrink-0"
                aria-hidden="true"
              />
            </button>
            <div v-show="isOpen(section)" :id="`${groupId(section)}-pages`" class="flex flex-col">
              <RouterLink
                v-for="page in section.pages"
                :key="page.to"
                :to="page.to"
                :aria-current="activePage?.to === page.to ? 'page' : undefined"
                :class="[
                  'border-l py-1 pl-10 pr-3 font-mono text-sm',
                  activePage?.to === page.to
                    ? 'border-primary text-foreground'
                    : 'border-transparent text-muted-foreground hover:text-foreground',
                ]"
              >
                {{ t(page.label) }}
              </RouterLink>
            </div>
          </div>
        </template>
      </nav>
    </aside>

    <div class="flex-1 min-w-0 pt-2">
      <RouterView />
    </div>
  </div>
</template>
