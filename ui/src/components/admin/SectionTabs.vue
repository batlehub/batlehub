<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useAuth } from "@/composables/useAuth";
import { ADMIN_PAGE_VERBS } from "@/config/adminSections";

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

const { can } = useAuth();
/** Only the tabs whose verb the viewer holds (RFC 0042 §4.3). */
const tabs = computed(() =>
  props.tabs.filter((tab) => {
    const verb = ADMIN_PAGE_VERBS[tab.to];
    return verb === undefined || can(verb);
  }),
);

/*
 * The longest match: "All registries" (`/admin/registries`) is a prefix of its
 * sibling tabs (RFC 0046 M3), and one registry's page belongs to the list.
 */
const current = computed(
  () =>
    tabs.value.filter((tab) => isActive(tab.to)).sort((a, b) => b.to.length - a.to.length)[0]?.to ??
    (route.path.startsWith("/admin/registry/") ? "/admin/registries" : ""),
);

function go(to: string) {
  if (to && to !== current.value) void router.push(to);
}

/* WCAG 3.2.2: on a closed select, desktop browsers fire `change` on every
   arrow key, so navigating on `change` turned each keystroke into a page load.
   A keyboard choice waits for Enter; a pointer or the platform picker still
   navigates at once. A choice left unconfirmed snaps back on blur. */
let keyed = false;
function onKeydown(event: KeyboardEvent) {
  const select = event.target as HTMLSelectElement;
  keyed = event.key !== "Enter";
  if (event.key === "Enter") go(select.value);
}
function onPointerdown() {
  keyed = false;
}
function onChange(event: Event) {
  if (!keyed) go((event.target as HTMLSelectElement).value);
}
function onBlur(event: FocusEvent) {
  keyed = false;
  (event.target as HTMLSelectElement).value = current.value;
}
</script>

<template>
  <!-- Mobile only. From `md` the admin sidebar lists every page of every
       section, so a desktop tab row under it repeated the same links a
       second time, one level down (DESIGN.md, Navigation). Below `md` the
       sidebar collapses into the section menu and this native selector is
       how the pages of the current section stay one tap away. -->
  <div class="md:hidden mb-4">
    <label for="section-tab-select" class="sr-only">{{ t("a11y.inThisSection") }}</label>
    <select
      id="section-tab-select"
      class="h-10 w-full border border-border bg-background px-3 font-sans text-sm"
      :value="current"
      @pointerdown="onPointerdown"
      @keydown="onKeydown"
      @change="onChange"
      @blur="onBlur"
    >
      <option v-for="tab in tabs" :key="tab.to" :value="tab.to">{{ t(tab.label) }}</option>
    </select>
  </div>
</template>
