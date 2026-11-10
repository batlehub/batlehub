<script setup lang="ts">
/**
 * The primary bar, cut to the design proof's segment grammar
 * (`ui/design-proof/index.html`, `.masthead nav`).
 *
 * The cells share one bounding box and are divided by the rule, rather than
 * floating as separate rounded pills: the box is what says "these three are one
 * choice". The chosen cell reverses to a solid ink block — the same reversal
 * the proof's `.segmented` control uses for a pressed option, so a selection
 * reads identically wherever it appears. That reversal is the component's one
 * contrast event and it costs no scanning room; the previous tinted fill was
 * carrying the state on `bg-accent`, which The Undependable Fill Rule says is
 * not a state channel at all (it resolves to `--ground-raised`, 1.06:1).
 *
 * Admin keeps its copper ink while unchosen — that signal is the app's, not the
 * proof's, and it survives the re-cut because it rides on ink, not on fill.
 */
import { useI18n } from "vue-i18n";
import { RouterLink, useRoute } from "vue-router";

const { t } = useI18n();

defineProps<{
  links: { to: string; label: string }[];
  variant: "desktop" | "mobile";
}>();

const emit = defineEmits<{ navigate: [] }>();

const route = useRoute();

function isActive(to: string) {
  return route.path === to || route.path.startsWith(to + "/");
}

/* The proof's Meta step, uppercased and tracked out. Shared so the desktop
   segment and the mobile stack cannot drift into two different typographies. */
const CELL = "text-xs uppercase tracking-[0.1em] transition-colors";

function cellInk(_to: string, active: boolean) {
  if (active) return "bg-foreground text-background font-bold";
  // Admin was copper, which means *waiting or held*; its place in the bar
  // already marks it out.
  return "text-muted-foreground hover:text-foreground";
}
</script>

<template>
  <!-- `hidden lg:flex`, not `md`: this segment run is what makes the masthead
       too wide to fit between 768 and 900 — the labels are translated, and
       `PAQUETS · CONFIGURATION · ADMINISTRATION` is 362px against the English
       227px. See the breakpoint note in `AppHeader.vue`. -->
  <nav
    v-if="variant === 'desktop'"
    :aria-label="t('nav.aria')"
    class="hidden lg:flex border border-border"
  >
    <RouterLink
      v-for="link in links"
      :key="link.to"
      :to="link.to"
      :aria-current="isActive(link.to) ? 'page' : undefined"
      :class="[
        CELL,
        'px-3 py-2 border-r border-border last:border-r-0',
        cellInk(link.to, isActive(link.to)),
      ]"
    >
      {{ t(link.label) }}
    </RouterLink>
  </nav>
  <!-- Mobile rows are bare: AppHeader stacks them into one ruled box with the
       account entries, so the panel is a single segment run rather than a
       bordered nav sitting beside four unbordered links. -->
  <template v-else>
    <RouterLink
      v-for="link in links"
      :key="link.to"
      :to="link.to"
      :aria-current="isActive(link.to) ? 'page' : undefined"
      :class="[CELL, 'block px-3 py-2', cellInk(link.to, isActive(link.to))]"
      @click="emit('navigate')"
    >
      {{ t(link.label) }}
    </RouterLink>
  </template>
</template>
