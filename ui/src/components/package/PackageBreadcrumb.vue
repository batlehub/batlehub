<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink, useRouter } from "vue-router";
import { ArrowLeft } from "@lucide/vue";
import type { ExploreVersionDto, FirewallDto } from "@/client/types.gen";
import { rowState, STATE_LABEL_KEYS } from "@/composables/usePackageVersions";
import { Resolution } from "@/components/ui/resolution";
import ScanBadge from "@/components/package/ScanBadge.vue";

/** The sticky trail at the top of the package page: catalog, registry, package,
    and the selected version with its state. */
const props = defineProps<{
  registry: string;
  name: string;
  selectedRow: ExploreVersionDto | null;
  /** RFC 0044: the detail's scan-status batch failed. */
  scanUnavailable?: boolean;
}>();

const { t } = useI18n();
const router = useRouter();

const stateLabel = (row: { state?: string; firewall: FirewallDto }) =>
  t(STATE_LABEL_KEYS[rowState(row)]);

/**
 * Back to the catalog **as the reader left it**, not to a fresh one.
 *
 * This pushed `/packages?registry=…`, which looked like it preserved something
 * and did not: the catalog never read `route.query`, so the registry was dropped
 * on arrival along with the search, the scope, the sort and the page. Opening a
 * package from the fifth page of a search and pressing Back gave an empty search
 * box and page one.
 *
 * `router.back()` when the previous entry is the catalog, because that restores
 * the URL *and* the scroll offset (`scrollBehavior` in `router/index.ts` returns
 * the saved position on a pop) — and the catalog now carries its whole state in
 * the query, so the URL is the search. Rebuilding a location by hand would
 * reconstruct the query and still lose the offset.
 *
 * The fallback is for arriving without that history: a pasted link, a refresh, a
 * new tab. `history.state.back` is Vue Router's own record of the previous
 * entry, so this asks the question directly rather than guessing from
 * `document.referrer`. A detail page also starts with `/packages`, hence the
 * exact-path test — going "back" from one package to another would be a
 * surprise, not a return.
 */
function isCatalogEntry(entry: unknown): boolean {
  if (typeof entry !== "string") return false;
  return entry.split("?")[0].split("#")[0] === "/packages";
}

function goBack() {
  if (isCatalogEntry(window.history.state?.back)) {
    router.back();
    return;
  }
  void router.push({ path: "/packages", query: { registry: props.registry } });
}

/**
 * The first crumb is a link that also knows how to *return*.
 *
 * A plain click runs `goBack()` — which pops the history entry when the catalog
 * is the one behind us, and that is what restores its search, scope, sort, page
 * *and* scroll offset. A modified click (new tab, new window, download) is left
 * to the browser, and so is the href, which is why this is an `<a>` and not the
 * `<button>` it used to be: a control that navigates and cannot be middle-clicked
 * is a link wearing the wrong element.
 */
/** The href the first crumb carries, so it is a real link before it is a handler. */
const catalogHref = computed(() => router.resolve({ path: "/packages" }).href);

function onCrumbToCatalog(event: MouseEvent) {
  if (event.defaultPrevented) return;
  if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.button !== 0)
    return;
  event.preventDefault();
  goBack();
}
</script>

<template>
  <!-- ── The trail ────────────────────────────────────────────────────────
       A back button became a breadcrumb, and the breadcrumb is sticky.

       It answers three separate complaints with one element. (1) It is real
       navigation: `Packages` was a `<button>` calling `goBack()`, so it had no
       href, could not be middle-clicked and told a reader nothing about where
       they were. (2) It carries **the selected version and its state**, and
       that is the point of the stickiness: the index sits below a README of
       arbitrary length, so clicking a version used to change a region the
       reader could no longer see — now the change is always on screen, in the
       one strip that never scrolls away. (3) It restores the location channel
       the console had nowhere else on this route.

       Sticky with a hairline and the page's own ground — no shadow, because
       nothing in this world casts one at rest, and a strip that needed a
       shadow to separate itself would be a strip that is not a rule.

       The first crumb is a plain `<a>` with a resolved href and its own click
       handler, deliberately not a `RouterLink`: a fallthrough `@click` on one
       is merged *after* the component's own `navigate`, so it would push
       before this handler could decline — the href is what makes it a link
       (middle-click, copy, focus), and `goBack()` is what restores the
       catalog's search, scope, sort, page and scroll offset when the reader
       came from there. A modified click stays the browser's.

       `top-[var(--masthead-h)] z-30` (set by `AppHeader`) is the offset the console's other two sticky bars already
       use (`AdminPackages.vue`, `PackageVersionsTable.vue`). `top-0 z-10`
       pinned this at viewport y=0 — the same line `AppHeader`'s `sticky top-0
       z-40` opaque bar occupies — so the crumb, the version and the Resolution
       chip vanished under the masthead the moment the page scrolled, which is
       the opposite of what the note above claims for it. -->
  <nav
    :aria-label="t('packageDetailPage.breadcrumb')"
    class="sticky top-[var(--masthead-h,4rem)] z-30 -mx-4 border-b border-rule-soft bg-background px-4 py-3 md:-mx-6 md:px-6"
  >
    <ol class="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm">
      <li>
        <a
          :href="catalogHref"
          class="inline-flex items-center gap-2 py-1 text-primary underline underline-offset-[3px]"
          @click="onCrumbToCatalog"
        >
          <ArrowLeft class="h-4 w-4" aria-hidden="true" />
          {{ t("packageDetailPage.backToCatalog") }}
        </a>
      </li>
      <li aria-hidden="true" class="text-muted-foreground">/</li>
      <li>
        <RouterLink
          :to="{ path: '/packages', query: { registry } }"
          class="inline-block py-1 text-primary underline underline-offset-[3px]"
          >{{ registry }}</RouterLink
        >
      </li>
      <li aria-hidden="true" class="text-muted-foreground">/</li>
      <li class="min-w-0 [overflow-wrap:anywhere] font-mono text-foreground" aria-current="page">
        {{ name }}
      </li>
      <!-- The version, and its state, pinned to the top of the viewport. This
           is the selection feedback; without it a click twelve rows down the
           index changes only what is off screen above. -->
      <template v-if="selectedRow">
        <li aria-hidden="true" class="text-muted-foreground">/</li>
        <li class="font-mono text-foreground">{{ selectedRow.version }}</li>
        <li>
          <Resolution :state="rowState(selectedRow)" :label="stateLabel(selectedRow)" />
        </li>
        <li>
          <ScanBadge :status="selectedRow.scan_status" :unavailable="scanUnavailable === true" />
        </li>
      </template>
    </ol>
  </nav>
</template>
