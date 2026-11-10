<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useAuth } from "@/composables/useAuth";
import { useCatalogQuery, type SortKey } from "@/composables/useCatalogQuery";
import { formatBytes, formatCount, formatRelative } from "@/lib/format";
import CatalogSpecimen from "@/components/catalog/CatalogSpecimen.vue";
import { Facet } from "@/components/ui/facet";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { Pagination } from "@/components/ui/pagination";
import { RefreshCw } from "@lucide/vue";
import {
  STATE_LABEL_KEYS,
  detailPath,
  matchLabel,
  noteKey,
  rowId,
  rowSnippet,
  rowState,
  type ExploreRow,
  type UpstreamRow,
} from "@/lib/catalogRow";
import { Resolution } from "@/components/ui/resolution";
import { Button } from "@/components/ui/button";
import { SearchInput } from "@/components/ui/search-input";
import { RuleNote } from "@/components/ui/rule-note";
import { Announcer } from "@/components/ui/announcer";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { AsyncState } from "@/components/ui/async-state";
import {
  Table,
  TableHeader,
  TableHead,
  TableBody,
  TableRow,
  TableCell,
} from "@/components/ui/table";

const { t } = useI18n();

const {
  selectedRegistry,
  search,
  searchIn,
  readmeSearchEnabled,
  searchTruncated,
  sort,
  page,
  perPage,
  allRegistries,
  registryStats,
  total,
  loading,
  loadingUpstream,
  error,
  facetOptions,
  totalPackages,
  countsKnown,
  tableRows,
  totalPages,
  searchedProse,
  exploreCache,
  fetchPackages,
  fetchUpstream,
  onClearSearch,
  onSearchInput,
  onSearchInChange,
  selectRegistry,
  onSortChange,
  goToPage,
  fetching,
  fetchResult,
  onFetchRow,
} = useCatalogQuery();

/**
 * The table's caption, which the proof spends on the two things a reader needs
 * before scanning a column of rows: how many there are, and what order they are
 * in. The ordering half is not decoration — a table sorted by last fetch and a
 * table sorted by downloads look identical, and the proof's own caption is what
 * tells them apart.
 *
 * Named from the same catalogue entries as the sort control, so the caption
 * cannot describe an order the control does not offer.
 */
const SORT_LABEL_KEYS: Record<SortKey, string> = {
  fetched: "packageCatalog.lastFetch",
  downloads: "packageCatalog.mostDownloaded",
  name: "packageCatalog.nameAZ",
  recent: "packageCatalog.recentlyAccessed",
};

const tableCaption = computed(() =>
  t("packageCatalog.tableCaption", {
    count: formatCount(total.value),
    sort: t(SORT_LABEL_KEYS[sort.value]),
  }),
);

const { isAuthenticated } = useAuth();

/**
 * Whether this row offers a fetch: upstream-only, and the server said yes.
 *
 * `fetch?.` although the generated type says the field is always there. It is
 * always there in a *fresh* response; the upstream half of this page is served
 * from a ten-minute in-memory cache, so a reader whose tab outlived a deploy
 * has rows from the server that had no such field. Optional chaining draws no
 * button for them, which is the right answer, where a hard read throws inside
 * the render and takes the whole listing down — including its cached rows,
 * which had nothing to do with it.
 */
function canFetch(row: ExploreRow): boolean {
  return row.kind === "upstream" && row.fetch?.offered === true && isAuthenticated.value;
}
</script>

<template>
  <div>
    <!-- The specimen. Not `PageHeader`: this is the one surface with a
         checked-in statement of its own appearance, and that statement gives
         the Display step to the subject rather than to the page's nav label
         (RFC 0004-bis §2.7, §14.9). Every other page keeps PageHeader.

         Full width, above the sheet — the proof's own arrangement, where
         `.specimen` is a direct child of `<main>` and `.sheet` starts beneath
         it. It used to sit *inside* the right-hand column, which meant the bay
         ran alongside it and the display type began 232px in from the page
         edge. A specimen is the page announcing its subject; starting it
         indented, in the gutter of a list it is not part of, is the one
         placement that undercuts that.

         `overflow-hidden` contains the plate; `relative` gives it a frame.
         The h1 sits above it on z-index — the plate is `aria-hidden` and
         purely decorative.

         The bottom rule is `--rule-soft`, the separator token, rather than
         `--border` (`--rule-strong`): it now divides the specimen from the
         sheet across the whole window, which is the job the proof gives
         `.specimen{border-bottom:1px solid var(--rule-soft)}`. At full width a
         contrast-carrying weight reads as a second masthead. -->
    <CatalogSpecimen
      :selected-registry="selectedRegistry"
      :all-registries="allRegistries"
      :registry-stats="registryStats"
      :counts-known="countsKnown"
      :total-packages="totalPackages"
    />

    <!-- The sheet: the proof's `grid-template-columns: 232px 1fr` with
         `align-items:start`, so the bay is its own column beside the catalog
         and neither stretches to match the other's height. Below `md` it
         collapses to one column, as the proof collapses it at 900px. -->
    <div class="grid min-h-[60vh] items-start md:grid-cols-[14rem_1fr]">
      <!-- The registry bay, the shared facet primitive rather than 40 lines of
           inline buttons. Selection rides on ink and a lit edge, not a fill.
           The separator is `--rule-soft` — it divides two regions, it does not
           carry contrast, and the proof rules it accordingly. -->
      <!-- Visible at every width. It used to be `hidden md:block`, which left a
           phone with no way to change registry at all — the filter existed and
           its only control did not. Below `md` it is a rule *under* the strip
           rather than beside it, matching the orientation the cells take. -->
      <!-- `min-w-0` for the same reason the catalog column below needs it, and
           it bites harder here: a grid item's default `min-width:auto` refuses
           to shrink below its content's min-content width, and this item's
           content is a nowrap flex row of every registry. Without it that row's
           full width propagates out through the grid and the *body* scrolls
           sideways — the one thing the Own-Container Overflow Rule forbids.
           With it, the row scrolls inside itself, which is the point. -->
      <aside
        class="min-w-0 border-b border-rule-soft py-3 md:border-b-0 md:border-r md:py-6 md:pr-4"
      >
        <Facet
          :model-value="selectedRegistry"
          :options="facetOptions"
          :label="t('packageCatalog.registries')"
          :all-label="
            countsKnown
              ? t('packageCatalog.allRegistries', { count: totalPackages })
              : t('packageCatalog.allRegistriesUnknown')
          "
          @update:model-value="selectRegistry"
        />
      </aside>

      <!-- `min-w-0` is load-bearing on a grid child: without it the track
           refuses to shrink below its content's min-content width, and the
           table's own scroll container never gets the chance to do its job. -->
      <div class="min-w-0 space-y-4 py-6 md:pl-6">
        <!-- Search + sort bar -->
        <div class="flex gap-2 flex-wrap">
          <SearchInput
            :placeholder="t('packageCatalog.searchPackages')"
            :aria-label="t('packageCatalog.searchPackages2')"
            :value="search"
            @input="onSearchInput(($event.target as HTMLInputElement).value)"
          />
          <!-- Where the search looks. Beside the box it modifies rather than in
             a settings panel, because it changes what the *next keystroke*
             means. Hidden entirely when the instance searches names only: a
             control whose other options do nothing is worse than no control
             (RFC 0007-bis §4.3). -->
          <!-- The label is visually hidden but `for`-bound: the `Select`
               trigger is a button, and an `aria-label` on the component lands
               on radix's hidden native select instead of on it. -->
          <template v-if="readmeSearchEnabled">
            <Label for="catalog-search-in" class="sr-only">{{
              t("packageCatalog.searchIn")
            }}</Label>
            <Select
              id="catalog-search-in"
              class="w-auto gap-2"
              :model-value="searchIn"
              :options="[
                { value: 'name', label: t('packageCatalog.searchInName') },
                { value: 'readme', label: t('packageCatalog.searchInReadme') },
                { value: 'both', label: t('packageCatalog.searchInBoth') },
              ]"
              @update:model-value="onSearchInChange"
            />
          </template>
          <!-- The page's one action, in the toolbar beside search — where
             `design-proof/index.html` puts its own (RFC 0004-bis §14.9). It
             used to sit in `PageHeader`, which the specimen replaced. -->
          <Button
            variant="outline"
            size="sm"
            @click="
              () => {
                exploreCache.invalidate(selectedRegistry ?? undefined);
                void fetchPackages();
                if (search.trim().length >= 2) void fetchUpstream();
              }
            "
          >
            <RefreshCw class="h-4 w-4 mr-1" />
            {{ t("common.refresh") }}
          </Button>
          <Label for="catalog-sort" class="sr-only">{{ t("packageCatalog.sortPackages") }}</Label>
          <Select
            id="catalog-sort"
            class="w-auto gap-2"
            :model-value="sort"
            :options="[
              { value: 'fetched', label: t('packageCatalog.lastFetch') },
              { value: 'downloads', label: t('packageCatalog.mostDownloaded') },
              { value: 'name', label: t('packageCatalog.nameAZ') },
              { value: 'recent', label: t('packageCatalog.recentlyAccessed') },
            ]"
            @update:model-value="onSortChange"
          />
        </div>

        <!-- Error: what failed, the transport's reason, and a way forward. It
             replaces the caption and the empty state below rather than joining
             them — "Failed to load", "0 packages" and "No packages cached yet"
             on one screen were three claims, two of them false. -->
        <AsyncState
          v-if="error"
          :loading="false"
          :error="error"
          :title="t('packageCatalog.loadFailed')"
          :on-retry="fetchPackages"
        />

        <!-- The catalog.

           No `Card`. The proof puts the table straight onto the sheet, and the
           card was adding a second boundary around content the ruled header row
           already bounds — two frames around one table, on a system whose
           Flat-At-Rest Rule allows no elevation to justify either.

           Column order is the proof's, and the order is the argument: State
           leads, because "does this instance hold it" is the question the whole
           world is organised around, and a reader scanning for what is wrong
           should not have to cross five columns to find out. It used to sit
           last, behind Registry, Versions, Downloads and Source. -->
        <!-- The floor under the five-column layout, and the reason the wrapper's
             `overflow-auto` is not decoration.

             `Table` sets `w-full`, whose min-width is 0, so the table could
             never be wider than its track: instead of overflowing into the
             scroll container the Own-Container Overflow Rule gives it, it
             crushed the one column with no declared width down to 74px. A
             scroll container with nothing that can exceed it is a region that
             never scrolls — measured `scrollWidth === clientWidth` at every
             width from 390 to 1440.

             44rem is 32.5rem of fixed columns plus 11.5rem for the name, so the
             narrowest legible five-column table. It applies only from `lg`,
             where the columns exist; below that the three remaining columns are
             content-sized and fit, and a min-width would make a phone scroll
             sideways for no gain. At `lg` the track is 728px against this
             704px, so the guard is dormant at every standard width and engages
             when the track is squeezed (a narrower window at that breakpoint, a
             zoom step, a longer version string). -->
        <Table :label="t('packageCatalog.tableLabel')" class="lg:min-w-[44rem]">
          <!-- Body, not Meta. DESIGN.md gives uppercase-and-tracked to labels —
               column heads, state words, nav — and prose to captions, and this
               is a sentence: "7 packages · sorted by last fetch" is 33
               characters of it. Set in caps it was the one run of body text on
               the page that the all-caps rule catches, because caps strips the
               ascenders and descenders a reader recognises words by. The
               tracking goes with it: letterspacing is a small-caps device. -->
          <caption v-if="!error" class="caption-top pb-3 text-left text-xs text-muted-foreground">
            {{
              tableCaption
            }}
          </caption>
          <TableHeader>
            <!-- The one solid rule in the table, under the header: the proof
               separates the head from the body with `--rule-strong` and every
               row below with a dashed `--rule-soft`. One weight for "this is
               the boundary", a lighter dashed one for "these are the divisions
               inside it". -->
            <!-- Cells are padded on the right and flush left, per the proof's
               `padding: --s3 --s3 --s3 0`: the column gap belongs *between*
               columns, and a left-flush first column is what lines the table up
               with the caption and the specimen rule above it.

               The fixed widths only apply from `lg`, and they are released
               below it exactly as the proof releases them
               (`.c-state,.c-ver{width:auto}` under 900px) — 9.5rem of State
               inside a 358px box leaves the package name nowhere to go, and the
               columns collide.

               `lg`, not `md`, because that is the width at which the fixed
               columns actually fit *this* layout, measured rather than
               transcribed from the proof's 900px. The four fixed columns claim
               9.5+9+6+8 = 32.5rem (520px), and the catalog track is the
               viewport less the 14rem bay and 4rem of padding. At the `md`
               breakpoint that track is 457px — the whole of it owed to columns
               that want 520 — so the one flexible column was squeezed to 74px
               and `overflow-wrap:anywhere` shredded `strip-ansi` into two
               lines. The header read `PACKAGE VERSION` with no gap. `lg` puts
               728px in the track, which is the first step where the five
               columns are all legible. -->
            <TableRow class="border-b border-solid border-border hover:bg-transparent">
              <TableHead class="pl-0 pr-3 lg:w-[9.5rem]">{{ t("common.state") }}</TableHead>
              <TableHead class="pl-0 pr-3">{{ t("common.package") }}</TableHead>
              <TableHead class="pl-0 pr-3 lg:w-[9rem]">{{ t("common.version") }}</TableHead>
              <!-- Dropped below `lg`, exactly as the proof drops them: at 390px
                 the fixed columns alone claimed 260px of a 358px box, and the
                 two numeric ones are the least load-bearing. -->
              <TableHead class="hidden pl-0 pr-3 text-right lg:table-cell lg:w-24">{{
                t("common.size")
              }}</TableHead>
              <TableHead class="hidden px-0 text-right lg:table-cell lg:w-32">{{
                t("packageCatalog.lastFetch")
              }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow v-if="loading" class="border-b-0 hover:bg-transparent">
              <TableCell colspan="5" class="px-0 py-4">
                <Skeleton :lines="6" />
              </TableCell>
            </TableRow>

            <template v-for="row in tableRows" :key="rowId(row)">
              <!-- No `@click` and no `cursor-pointer`: the row is not the
                   control, the name is. A whole-row handler is unreachable by
                   keyboard and invisible to assistive tech, and dressing it as
                   clickable promised a target that only a mouse could take. -->
              <TableRow :class="['align-baseline', noteKey(row) ? 'border-b-0' : '']">
                <TableCell class="pl-0 pr-3 py-3 align-baseline">
                  <Resolution :state="rowState(row)" :label="t(STATE_LABEL_KEYS[rowState(row)])" />
                </TableCell>
                <!-- `aria-describedby` is what ties a denial to the package it is
                   about. Without it the note below is a paragraph a screen
                   reader meets after the row and has to relate by proximity.

                   `overflow-wrap:anywhere` is what the proof sets on `.pkg` —
                   not `break-words`. The two differ in exactly the way that
                   matters here: `anywhere` is taken into account when the
                   browser computes the cell's *min-content* width, `break-word`
                   is not. So with `break-words` a name like
                   `github.com/spf13/cobra` still demanded its full width, the
                   table grew past the viewport, and the whole catalog sat
                   behind a horizontal scrollbar at 390px. -->
                <TableCell
                  class="[overflow-wrap:anywhere] pl-0 pr-3 py-3 align-baseline text-base"
                  :aria-describedby="noteKey(row) ? `note-${rowId(row)}` : undefined"
                >
                  <!-- The package name *is* the link — the pattern the version
                       list on the detail page already uses. `anywhere` rather
                       than `break-words` for the same min-content reason as the
                       cell above, and an underline on hover because the row no
                       longer carries any other affordance. -->
                  <RouterLink
                    :to="detailPath(row)"
                    class="[overflow-wrap:anywhere] hover:underline underline-offset-[3px]"
                  >
                    {{ row.name }}
                  </RouterLink>
                  <!-- One list, provenance stated per row. The question a reader
                     actually has is "does this instance already have it?", and
                     splitting the table into two makes that harder to answer,
                     not easier. -->
                  <span
                    v-if="row.kind === 'upstream'"
                    class="ml-2 text-xs uppercase tracking-[0.1em] text-muted-foreground"
                    >{{ t("packageCatalog.upstreamRow") }}</span
                  >
                  <span
                    v-else-if="!selectedRegistry"
                    class="ml-2 text-xs uppercase tracking-[0.1em] text-muted-foreground"
                    >{{ row.registry }}</span
                  >
                  <!-- Why this row is here. Shown only for a prose match: a row
                     that matched on its name needs no explanation, and labelling
                     every row would make the one that needs it invisible. -->
                  <span
                    v-if="matchLabel(t, row)"
                    class="ml-2 text-xs uppercase tracking-[0.1em] text-muted-foreground"
                    :title="t('packageCatalog.matchedInLabel', { where: matchLabel(t, row) })"
                    >{{ matchLabel(t, row) }}</span
                  >
                </TableCell>
                <TableCell class="pl-0 pr-3 py-3 align-baseline text-muted-foreground tabular-nums">
                  <template v-if="row.kind === 'cached'">{{ row.newest_version ?? "—" }}</template>
                  <template v-else>{{ row.latest_version ?? "—" }}</template>
                </TableCell>
                <TableCell
                  class="hidden pl-0 pr-3 py-3 text-right align-baseline text-muted-foreground tabular-nums lg:table-cell"
                >
                  <template v-if="row.kind === 'cached'">{{
                    formatBytes(row.cached_bytes)
                  }}</template>
                  <template v-else>—</template>
                </TableCell>
                <TableCell
                  class="hidden whitespace-nowrap px-0 py-3 text-right align-baseline text-muted-foreground tabular-nums lg:table-cell"
                >
                  <template v-if="row.kind === 'cached'">{{
                    formatRelative(row.last_fetched_at, { fallback: "—" })
                  }}</template>
                  <template v-else>—</template>
                </TableCell>
              </TableRow>

              <!-- Every denial states its rule in its own row, tied to the
                 package by `aria-describedby` — not one note floating above the
                 table. The rule is indented under the package rather than under
                 the state, so the eye reads it as belonging to the name. -->
              <!-- The matched fragment, as **text**. It is interpolated, never
                 `v-html`: the README panel's markup boundary is a deliberate,
                 tested, single-component one, and a search snippet is a second
                 surface for package-authored content reached by a much cheaper
                 path — no navigation, just a query (RFC 0007-bis §7.4). -->
              <TableRow
                v-if="rowSnippet(row)"
                class="border-dashed border-rule-soft hover:bg-transparent"
              >
                <TableCell class="pl-0 pr-3 pb-3 pt-0" />
                <TableCell colspan="4" class="pl-0 pr-3 pb-3 pt-0">
                  <RuleNote>
                    {{ rowSnippet(row) }}
                  </RuleNote>
                </TableCell>
              </TableRow>

              <!-- The action for an upstream row, in a row of its own rather
                 than as a chip in a cell — the shape the snippet and the
                 refusal note already use here, and the one the package page
                 arrived at after its own fetch button spent a while as a 24px
                 chip in a table column.

                 The label names the version, which is the version the upstream
                 search returned and the one the cell above shows. Nothing is
                 being chosen on this screen (RFC 0007-bis §11 q3).

                 On success no message is written: the listing and the upstream
                 results are both refetched, the row leaves the upstream half
                 and reappears as a held one, and its state chip is a better
                 answer than a sentence under a row that no longer exists. -->
              <TableRow
                v-if="canFetch(row) || fetchResult[rowId(row)]"
                class="border-dashed border-rule-soft hover:bg-transparent"
              >
                <TableCell class="pl-0 pr-3 pb-3 pt-0" />
                <!-- `colspan="2"` plus the two trailing cells the wide columns
                   use, exactly as the note row below does. A single
                   `colspan="4"` declares five columns on a row whose table
                   renders three below `lg` — the last two `<TableHead>`s are
                   `hidden lg:table-cell` — and the browser then widens the
                   whole table to five, shifting the header away from the body
                   on every viewport under the breakpoint. -->
                <TableCell colspan="2" class="pl-0 pr-3 pb-3 pt-0">
                  <div class="flex flex-wrap items-center gap-3">
                    <!-- The visible label names the version, which is all a
                       reader who can see the row it sits under needs. The
                       accessible name adds the package: a screen-reader user
                       arrives at this button out of that context, and "Fetch
                       9.9.9" on a page of fifty hits names nothing. -->
                    <Button
                      v-if="canFetch(row)"
                      variant="outline"
                      size="sm"
                      :disabled="fetching !== null"
                      :aria-label="
                        t('packageCatalog.fetchVersionLabel', {
                          name: row.name,
                          version: (row as UpstreamRow).latest_version,
                        })
                      "
                      @click="onFetchRow(row as UpstreamRow)"
                    >
                      {{
                        fetching === rowId(row)
                          ? t("packageCatalog.fetching")
                          : t("packageCatalog.fetchVersion", {
                              version: (row as UpstreamRow).latest_version,
                            })
                      }}
                    </Button>
                    <output v-if="fetchResult[rowId(row)]" class="text-sm text-muted-foreground">
                      {{ fetchResult[rowId(row)] }}
                    </output>
                  </div>
                </TableCell>
                <TableCell class="hidden px-0 pb-3 pt-0 lg:table-cell" />
                <TableCell class="hidden px-0 pb-3 pt-0 lg:table-cell" />
              </TableRow>

              <TableRow
                v-if="noteKey(row)"
                class="border-dashed border-rule-soft hover:bg-transparent"
              >
                <TableCell class="pl-0 pr-3 pb-3 pt-0" />
                <TableCell colspan="2" class="pl-0 pr-3 pb-3 pt-0">
                  <RuleNote :id="`note-${rowId(row)}`">
                    {{ t(noteKey(row)!) }}
                    <!-- Underlined at rest, not on hover. Crimson on the
                           note's dim ink measures 1.28:1 against the surrounding
                           text, so colour alone cannot mark it as a link — axe's
                           `link-in-text-block` wants 3:1 or a non-colour cue,
                           and the design gate failed on exactly that. The proof
                           underlines it too: its links take the browser default
                           and it only sets the offset. -->
                    <RouterLink
                      v-if="row.kind === 'cached'"
                      class="text-primary underline underline-offset-[3px]"
                      :to="detailPath(row)"
                      >{{ t("packageCatalog.noteDetails") }}</RouterLink
                    >
                  </RuleNote>
                </TableCell>
                <TableCell class="hidden px-0 pb-3 pt-0 lg:table-cell" />
                <TableCell class="hidden px-0 pb-3 pt-0 lg:table-cell" />
              </TableRow>
            </template>

            <!-- Upstream loading indicator (announced by the Announcer after the table) -->
            <TableRow v-if="loadingUpstream" class="border-b-0 hover:bg-transparent">
              <TableCell colspan="5" class="px-0 py-2 text-center text-xs text-muted-foreground">{{
                t("packageCatalog.searchingUpstreamRegistries")
              }}</TableCell>
            </TableRow>
          </TableBody>
        </Table>
        <Announcer
          :message="loadingUpstream ? t('packageCatalog.searchingUpstreamRegistries') : ''"
        />
        <!-- A cap that applied, said out loud. A silently shortened list reads
             as "that is all there is", which is a lie about the catalogue. -->
        <p v-if="searchTruncated" class="mt-2 text-xs text-muted-foreground">
          {{ t("packageCatalog.readmeSearchTruncated", { count: total }) }}
        </p>

        <!-- Empty is two states, and telling them apart is the point: a user
           shown "no packages" while a filter is applied concludes the registry
           is broken.

           Outside the table on purpose: in a `<td colspan>` this inherits the
           table's width, so at 390px the "nothing here" message sat off-screen
           behind a horizontal scroll — the one thing DESIGN.md's Own-Container
           Overflow Rule forbids the body to do. -->
        <div v-if="tableRows.length === 0 && !loadingUpstream && !loading && !error" class="mt-4">
          <!-- A prose search that found nothing gets its own words. "Nothing
             matches that search" would imply the query was checked against every
             package here, and it was checked against the READMEs of the versions
             this instance holds — which is a narrower claim and the honest one
             (RFC 0007-bis §4.3). -->
          <EmptyState
            :filtered="Boolean(search.trim())"
            :title="
              searchedProse
                ? t('packageCatalog.readmeSearchEmptyTitle')
                : search.trim()
                  ? t('catalog.emptyFilteredTitle')
                  : t('catalog.emptyTitle')
            "
            :description="
              searchedProse
                ? t('packageCatalog.readmeSearchEmptyBody')
                : search.trim()
                  ? t('catalog.emptyFilteredBody')
                  : t('catalog.emptyBody')
            "
          >
            <template v-if="search.trim()" #action>
              <Button size="sm" variant="outline" @click="onClearSearch">{{
                t("packageCatalog.clearSearch")
              }}</Button>
            </template>
            <template v-else #action>
              <Button size="sm" variant="outline" as-child>
                <RouterLink to="/setup">{{ t("packageCatalog.pointAToolAt") }}</RouterLink>
              </Button>
            </template>
          </EmptyState>
        </div>

        <!-- Pagination (cached results only) -->
        <div
          v-if="total > perPage"
          class="flex items-center justify-between text-sm text-muted-foreground"
        >
          <span>{{ t("packageCatalog.cachedPackagesTotal", total) }}</span>
          <Pagination :page="page" :total-pages="totalPages" @update:page="goToPage" />
        </div>
      </div>
    </div>
  </div>
</template>
